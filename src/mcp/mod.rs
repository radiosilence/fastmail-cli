//! MCP (Model Context Protocol) server for Fastmail
//!
//! Exposes Fastmail functionality via two GraphQL tools:
//! - `schema_sdl` — returns the full GraphQL SDL for introspection
//! - `graphql` — executes a GraphQL query/mutation

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::jmap::JmapClient;

type ToolResult = std::result::Result<CallToolResult, McpError>;

pub mod graphql;

use graphql::{FastmailSchema, SharedClient};

/// Header carrying the per-request Fastmail API token in HTTP transport mode.
/// A trusted upstream (the hosted service, after authenticating the user) sets
/// this before proxying the request. Over stdio it is absent and the config
/// token is used instead.
pub const TOKEN_HEADER: &str = "x-fastmail-token";

/// Cache of authenticated JMAP clients keyed by Fastmail token, so we don't
/// re-run the JMAP session handshake on every tool call. Shared across sessions.
type ClientCache = Arc<Mutex<HashMap<String, SharedClient>>>;

/// Prefer the per-request `X-Fastmail-Token` header (HTTP), else fall back to
/// the configured default (stdio). Pure so it can be unit-tested without a live
/// [`RequestContext`].
fn resolve_token(parts: Option<&http::request::Parts>, default: Option<&str>) -> Option<String> {
    parts
        .and_then(|p| p.headers.get(TOKEN_HEADER))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| default.map(str::to_owned))
}

// ============ Request Types ============

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GraphqlRequest {
    /// The GraphQL query or mutation string
    pub query: String,
    /// Optional JSON-encoded variables for the query
    #[serde(default)]
    pub variables: Option<String>,
}

// ============ Server Implementation ============

#[derive(Clone)]
pub struct FastmailMcp {
    schema: Arc<FastmailSchema>,
    clients: ClientCache,
    /// Fallback token for stdio mode (loaded from config). `None` in hosted
    /// HTTP mode, where the token must arrive per request via [`TOKEN_HEADER`].
    default_token: Option<String>,
    #[allow(dead_code)] // referenced by #[tool_handler] macro expansion
    tool_router: ToolRouter<Self>,
}

impl FastmailMcp {
    fn build(default_token: Option<String>) -> Self {
        Self {
            schema: Arc::new(graphql::build_schema()),
            clients: Arc::new(Mutex::new(HashMap::new())),
            default_token,
            tool_router: Self::tool_router(),
        }
    }

    /// Construct for stdio use: requires a token in config/env, used for every
    /// request. Errors if no token is configured.
    pub fn new() -> anyhow::Result<Self> {
        let config = Config::load()?;
        let token = config.get_token()?;
        Ok(Self::build(Some(token)))
    }

    /// Construct for hosted HTTP use: no default token. Each request must carry
    /// its own via [`TOKEN_HEADER`], injected by the trusted upstream service.
    pub fn hosted() -> Self {
        Self::build(None)
    }

    /// Resolve the Fastmail token for this request: the per-request header if
    /// present (HTTP), otherwise the configured default (stdio).
    fn resolve_token(&self, ctx: &RequestContext<RoleServer>) -> Option<String> {
        let header = ctx.extensions.get::<http::request::Parts>();
        resolve_token(header, self.default_token.as_deref())
    }

    /// Get or lazily create an authenticated JMAP client for `token`, caching it
    /// for reuse. The JMAP session handshake runs once per distinct token.
    async fn client_for(&self, token: &str) -> anyhow::Result<SharedClient> {
        if let Some(existing) = self.clients.lock().await.get(token) {
            return Ok(existing.clone());
        }
        // Authenticate outside the cache lock so concurrent callers for other
        // tokens aren't blocked on this network round-trip.
        let mut client = JmapClient::new(token.to_string());
        client.authenticate().await?;
        let shared: SharedClient = Arc::new(Mutex::new(client));

        // Re-check under lock: another caller may have inserted meanwhile.
        Ok(self
            .clients
            .lock()
            .await
            .entry(token.to_string())
            .or_insert(shared)
            .clone())
    }

    fn text_result(text: impl Into<String>) -> ToolResult {
        Ok(CallToolResult::success(vec![Content::text(text.into())]))
    }

    fn error_result(msg: impl Into<String>) -> ToolResult {
        Ok(CallToolResult::error(vec![Content::text(msg.into())]))
    }
}

#[tool_router]
impl FastmailMcp {
    #[tool(
        description = "Returns the full GraphQL SDL (Schema Definition Language) for the Fastmail API. Call this first to discover available queries, mutations, types, and their arguments. The schema includes all email, mailbox, identity, masked email, contact, and attachment operations."
    )]
    async fn schema_sdl(&self) -> ToolResult {
        Self::text_result(self.schema.sdl())
    }

    #[tool(
        description = "Execute a GraphQL query or mutation against the Fastmail API. Use `schema_sdl` first to discover the schema. Supports all email operations: listing mailboxes, reading/searching emails, sending/replying/forwarding (with preview/confirm pattern), managing masked emails, downloading attachments, and searching contacts. Pass variables as a JSON string."
    )]
    async fn graphql(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(req): Parameters<GraphqlRequest>,
    ) -> ToolResult {
        let Some(token) = self.resolve_token(&ctx) else {
            return Self::error_result(
                "No Fastmail token available. Configure one via `fastmail-cli auth` \
                 (stdio) or send the X-Fastmail-Token header (HTTP).",
            );
        };
        let client = match self.client_for(&token).await {
            Ok(client) => client,
            Err(e) => return Self::error_result(format!("Fastmail authentication failed: {e}")),
        };

        let mut request = async_graphql::Request::new(&req.query).data(client);

        if let Some(ref vars) = req.variables {
            match serde_json::from_str::<serde_json::Value>(vars) {
                Ok(serde_json::Value::Object(map)) => {
                    request = request.variables(async_graphql::Variables::from_json(
                        serde_json::Value::Object(map),
                    ));
                }
                Ok(_) => {
                    return Self::error_result("Variables must be a JSON object");
                }
                Err(e) => {
                    return Self::error_result(format!("Invalid variables JSON: {e}"));
                }
            }
        }

        let response = self.schema.execute(request).await;
        let json = serde_json::to_string_pretty(&response)
            .unwrap_or_else(|e| format!("{{\"error\": \"Serialization failed: {e}\"}}"));

        Self::text_result(json)
    }
}

#[tool_handler]
impl ServerHandler for FastmailMcp {
    fn get_info(&self) -> ServerInfo {
        let server_info = Implementation::new("fastmail-cli", env!("CARGO_PKG_VERSION"))
            .with_title("Fastmail MCP Server")
            .with_website_url("https://github.com/radiosilence/fastmail-cli");

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(rmcp::model::ProtocolVersion::V_2024_11_05)
            .with_server_info(server_info)
            .with_instructions(
                "Fastmail MCP Server — GraphQL interface for email operations.\n\n\
                ## Getting Started\n\
                1. Call `schema_sdl` to get the full GraphQL schema\n\
                2. Use `graphql` to execute queries and mutations\n\n\
                ## Common Queries\n\
                ```graphql\n\
                # List mailboxes\n\
                { mailboxes { id name role unreadEmails totalEmails } }\n\n\
                # List emails in inbox\n\
                { emails(mailbox: \"INBOX\", limit: 10) { id subject from { email name } receivedAt preview isUnread } }\n\n\
                # Get full email\n\
                { email(id: \"abc123\") { id subject from { email name } to { email name } textBody } }\n\n\
                # Search emails\n\
                { searchEmails(query: \"invoice\", after: \"2024-01-01\") { id subject from { email } receivedAt } }\n\
                ```\n\n\
                ## Sending Emails (ALWAYS preview first!)\n\
                ```graphql\n\
                # Step 1: Preview\n\
                mutation { sendEmail(action: PREVIEW, to: \"recipient@example.com\", subject: \"Hello\", body: \"...\") { preview } }\n\n\
                # Step 2: After user approval, confirm\n\
                mutation { sendEmail(action: CONFIRM, to: \"recipient@example.com\", subject: \"Hello\", body: \"...\") { emailId } }\n\
                ```\n\n\
                ## Safety Rules\n\
                - NEVER send without showing preview first\n\
                - NEVER confirm send without explicit user approval\n\
                - mark_as_spam affects future filtering — always preview first",
            )
    }
}

/// Run the MCP server with stdio transport. The Fastmail token comes from
/// config/env and is used for every request.
pub async fn run_server() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let service = FastmailMcp::new()?;
    let server = service
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    server
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;

    Ok(())
}

/// Run the MCP server over streamable HTTP on `addr`, mounted at `/mcp`.
///
/// No token is baked in: each request must carry its Fastmail token in the
/// [`TOKEN_HEADER`] header, set by a trusted upstream after authenticating the
/// user. This is the transport the hosted service puts behind OAuth.
pub async fn run_http_server(addr: &str) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    // One shared instance (shared schema + client cache) cloned into each session.
    let template = FastmailMcp::hosted();
    let service = StreamableHttpService::new(
        move || Ok(template.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("MCP streamable-HTTP server listening on http://{addr}/mcp");
    axum::serve(listener, router)
        .await
        .map_err(|e| anyhow::anyhow!("MCP HTTP server error: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts_with(header: Option<&str>) -> http::request::Parts {
        let mut builder = http::Request::builder();
        if let Some(token) = header {
            builder = builder.header(TOKEN_HEADER, token);
        }
        builder.body(()).unwrap().into_parts().0
    }

    #[test]
    fn header_token_wins_over_default() {
        let parts = parts_with(Some("header-tok"));
        let got = resolve_token(Some(&parts), Some("default-tok"));
        assert_eq!(got.as_deref(), Some("header-tok"));
    }

    #[test]
    fn falls_back_to_default_when_no_header() {
        let parts = parts_with(None);
        let got = resolve_token(Some(&parts), Some("default-tok"));
        assert_eq!(got.as_deref(), Some("default-tok"));
    }

    #[test]
    fn falls_back_to_default_when_no_parts() {
        // stdio: no HTTP parts in the request context at all.
        let got = resolve_token(None, Some("default-tok"));
        assert_eq!(got.as_deref(), Some("default-tok"));
    }

    #[test]
    fn none_when_neither_header_nor_default() {
        // hosted mode with no upstream-injected token — must refuse.
        assert_eq!(resolve_token(Some(&parts_with(None)), None), None);
        assert_eq!(resolve_token(None, None), None);
    }
}

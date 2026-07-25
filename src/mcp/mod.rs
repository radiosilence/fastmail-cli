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

/// Get or lazily create an authenticated JMAP client for `token`, caching it
/// for reuse. The JMAP session handshake runs once per distinct token.
async fn client_for(cache: &ClientCache, token: &str) -> anyhow::Result<SharedClient> {
    if let Some(existing) = cache.lock().await.get(token) {
        return Ok(existing.clone());
    }
    // Authenticate outside the cache lock so concurrent callers for other
    // tokens aren't blocked on this network round-trip.
    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;
    let shared: SharedClient = Arc::new(Mutex::new(client));

    // Re-check under lock: another caller may have inserted meanwhile.
    Ok(cache
        .lock()
        .await
        .entry(token.to_string())
        .or_insert(shared)
        .clone())
}

/// Prefer the per-request `X-Fastmail-Token` header (HTTP), else fall back to
/// the configured default. Pure so it can be unit-tested without a live
/// [`RequestContext`].
fn resolve_token(headers: Option<&http::HeaderMap>, default: Option<&str>) -> Option<String> {
    headers
        .and_then(|h| h.get(TOKEN_HEADER))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| default.map(str::to_owned))
}

/// The Fastmail token to use when no request header supplies one: the local
/// config or `FASTMAIL_API_TOKEN`, if either is present.
///
/// Best-effort by design. A hosted deployment ships neither, so this is `None`
/// there and every request must carry its own header — while running locally
/// picks up your credentials without ceremony.
fn local_token() -> Option<String> {
    Config::load().ok().and_then(|c| c.get_token().ok())
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
    /// Token used when a request carries no [`TOKEN_HEADER`]. Always set over
    /// stdio; over HTTP it is whatever [`local_token`] found, so `None` in a
    /// hosted deployment and every request must bring its own.
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

    /// Construct for HTTP use. A request's own [`TOKEN_HEADER`] always wins;
    /// [`local_token`] is the fallback, which exists when you run this yourself
    /// and not in a hosted deployment.
    pub fn http() -> Self {
        Self::build(local_token())
    }

    /// Resolve the Fastmail token for this request: the per-request header if
    /// present (HTTP), otherwise the configured default (stdio).
    fn resolve_token(&self, ctx: &RequestContext<RoleServer>) -> Option<String> {
        let headers = ctx
            .extensions
            .get::<http::request::Parts>()
            .map(|p| &p.headers);
        resolve_token(headers, self.default_token.as_deref())
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
        title = "Fastmail schema",
        description = "Returns the full GraphQL SDL (Schema Definition Language) for the Fastmail API. Call this first to discover available queries, mutations, types, and their arguments. The schema includes all email, mailbox, identity, masked email, contact, and attachment operations."
    )]
    async fn schema_sdl(&self) -> ToolResult {
        Self::text_result(self.schema.sdl())
    }

    #[tool(
        title = "Fastmail",
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
        let client = match client_for(&self.clients, &token).await {
            Ok(client) => client,
            Err(e) => return Self::error_result(format!("Fastmail authentication failed: {e}")),
        };

        let mut request = graphql::request(&req.query, client);

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

        // No protocol version is declared: rmcp defaults to the newest it
        // implements, and negotiation settles on the lower of ours and the
        // client's. The declared version is therefore a *ceiling* — pinning an
        // old one (this was stuck on 2024-11-05) caps every client to it, which
        // is how `title` on a tool went unused.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server_info)
            .with_instructions(
                "Fastmail, as a GraphQL API.\n\n\
                Call `schema_sdl` once — every type, argument and per-field cost \
                is documented there — then use `graphql`. Variables go as a JSON \
                string.\n\n\
                ## Querying well\n\
                - The graph is fully nested and everything below a list is \
                  batched, so ask for what you need in ONE query rather than \
                  looping. Selecting `textBody` across 25 emails costs one extra \
                  API call, not 25.\n\
                - Collections are connections: `nodes { ... }` for the items, \
                  `first`/`after` to page, cursors are IDs. Default page is 25, \
                  so check `pageInfo.hasNextPage` before assuming that is all of \
                  them.\n\
                - Ask only for fields you will use; their descriptions say what \
                  each costs. Attachment metadata is free, `text` parses the \
                  whole document. `emails { totalCount }` on its own answers \
                  \"how many?\" while fetching no mail at all.\n\n\
                ## Safety\n\
                Never send without showing the user a PREVIEW first, and never \
                CONFIRM without their explicit approval. Marking spam trains the \
                filter, so preview that too.",
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

/// Body of a GraphQL-over-HTTP request, as GraphiQL sends it.
#[derive(serde::Deserialize)]
struct HttpGraphqlRequest {
    query: String,
    #[serde(default)]
    variables: Option<serde_json::Value>,
    #[serde(default, rename = "operationName")]
    operation_name: Option<String>,
}

/// The GraphiQL IDE page. GraphiQL itself is loaded from a CDN with pinned
/// versions and SRI hashes — see `templates/graphiql.html`.
#[derive(askama::Template)]
#[template(path = "graphiql.html")]
struct GraphiqlPage<'a> {
    title: &'a str,
    endpoint: &'a str,
}

/// Whether every top-level selection is an introspection field, and so can be
/// answered from the schema alone.
///
/// GraphiQL sends exactly this on load to build its docs, autocomplete and
/// explorer. Requiring a Fastmail token for it would mean bad credentials leave
/// you with an IDE that cannot describe the API you are trying to explore.
/// Anything it cannot parse, or that mixes in real fields, is not introspection.
fn is_introspection_only(query: &str) -> bool {
    use async_graphql::parser::types::Selection;

    let Ok(doc) = async_graphql::parser::parse_query(query) else {
        return false;
    };
    doc.operations.iter().all(|(_, op)| {
        op.node
            .selection_set
            .node
            .items
            .iter()
            .all(|item| match &item.node {
                Selection::Field(field) => field.node.name.node.starts_with("__"),
                // Fragments could hide anything; make them take the auth path.
                _ => false,
            })
    })
}

/// Plain GraphQL-over-HTTP, for browsers and anything else that speaks it
/// directly rather than through MCP's JSON-RPC envelope. Shares the server's
/// schema, client cache and token resolution with the `graphql` tool.
async fn graphql_endpoint(
    axum::extract::State(mcp): axum::extract::State<FastmailMcp>,
    headers: http::HeaderMap,
    axum::Json(req): axum::Json<HttpGraphqlRequest>,
) -> axum::Json<async_graphql::Response> {
    let error = |msg: String| {
        axum::Json(async_graphql::Response::from_errors(vec![
            async_graphql::ServerError::new(msg, None),
        ]))
    };

    // Introspection is answered from the schema, so it neither needs a token nor
    // touches the network — the IDE stays usable while credentials are wrong.
    let mut request = if is_introspection_only(&req.query) {
        async_graphql::Request::new(&req.query)
    } else {
        let Some(token) = resolve_token(Some(&headers), mcp.default_token.as_deref()) else {
            return error(format!(
                "No Fastmail token available. Configure one via `fastmail-cli auth` \
                 or send the {TOKEN_HEADER} header."
            ));
        };
        // Authenticated on first use rather than at startup, so a missing or
        // expired token surfaces in the response pane instead of stopping the
        // server booting.
        let client = match client_for(&mcp.clients, &token).await {
            Ok(client) => client,
            Err(e) => return error(format!("Fastmail authentication failed: {e}")),
        };
        graphql::request(&req.query, client)
    };
    if let Some(vars) = req.variables {
        request = request.variables(async_graphql::Variables::from_json(vars));
    }
    if let Some(name) = req.operation_name {
        request = request.operation_name(name);
    }
    axum::Json(mcp.schema.execute(request).await)
}

/// Which surfaces [`run_http_server`] mounts alongside MCP at `/mcp`.
#[derive(Clone, Copy)]
pub struct HttpSurfaces {
    /// Plain GraphQL-over-HTTP at `/graphql`.
    pub graphql: bool,
    /// The GraphiQL IDE at `/`. Implies `graphql` — it is the IDE's endpoint.
    pub graphiql: bool,
    /// Open the IDE in the default browser once listening.
    pub browser: bool,
}

/// Run the HTTP server on `addr`: MCP streamable-HTTP at `/mcp`, plus whichever
/// of [`HttpSurfaces`] is enabled.
///
/// A request's own [`TOKEN_HEADER`] always wins; [`local_token`] is the
/// fallback. Running this yourself, that means your own credentials with no
/// ceremony. In a hosted deployment there is no local token, so every request
/// must carry the header — set by a trusted upstream after authenticating the
/// caller. Do **not** expose this to the internet without such a layer in
/// front: the header is trusted unconditionally.
pub async fn run_http_server(addr: &str, surfaces: HttpSurfaces) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    // One shared instance (shared schema + client cache) cloned into each session
    // and used as the axum state for the GraphQL routes.
    let mcp = FastmailMcp::http();
    // Disable rmcp's DNS-rebinding Host allowlist: this transport is designed to
    // run behind a trusted reverse proxy (see the note above), which forwards an
    // internal Host (e.g. the service name) that the default allowlist
    // (localhost/127.0.0.1/::1) would reject with 403. Rebinding protection
    // guards browsers hitting a localhost MCP directly — irrelevant for a
    // proxied, non-browser-facing backend; the proxy is the security boundary.
    let config = StreamableHttpServerConfig::default().disable_allowed_hosts();
    let service = StreamableHttpService::new(
        {
            let template = mcp.clone();
            move || Ok(template.clone())
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let mut router = axum::Router::new().nest_service("/mcp", service);
    tracing::info!("MCP streamable-HTTP listening on http://{addr}/mcp");

    if surfaces.graphql || surfaces.graphiql {
        router = router.route("/graphql", axum::routing::post(graphql_endpoint));
        tracing::info!("GraphQL endpoint on http://{addr}/graphql");
    }

    if surfaces.graphiql {
        // Rendered once: nothing in the page varies per request, and a template
        // error should stop the server rather than 500 on every hit.
        let ide = askama::Template::render(&GraphiqlPage {
            title: "Fastmail GraphQL",
            endpoint: "/graphql",
        })?;
        router = router.route(
            "/",
            axum::routing::get(move || {
                let ide = ide.clone();
                async move { axum::response::Html(ide) }
            }),
        );
        tracing::info!("GraphiQL IDE on http://{addr}/");
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Only once the listener is bound, so the browser cannot beat us to it.
    if surfaces.browser {
        let url = format!("http://{addr}/");
        if let Err(e) = open::that_detached(&url) {
            tracing::warn!("Could not open a browser at {url}: {e}");
        }
    }

    axum::serve(listener, router.with_state(mcp))
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server error: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(token: Option<&str>) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        if let Some(token) = token {
            headers.insert(TOKEN_HEADER, token.parse().unwrap());
        }
        headers
    }

    #[test]
    fn header_token_wins_over_default() {
        let headers = headers_with(Some("header-tok"));
        let got = resolve_token(Some(&headers), Some("default-tok"));
        assert_eq!(got.as_deref(), Some("header-tok"));
    }

    #[test]
    fn falls_back_to_default_when_no_header() {
        let headers = headers_with(None);
        let got = resolve_token(Some(&headers), Some("default-tok"));
        assert_eq!(got.as_deref(), Some("default-tok"));
    }

    #[test]
    fn falls_back_to_default_when_no_headers() {
        // stdio: no HTTP headers in the request context at all.
        let got = resolve_token(None, Some("default-tok"));
        assert_eq!(got.as_deref(), Some("default-tok"));
    }

    #[test]
    fn introspection_needs_no_token() {
        // What GraphiQL sends on load, plus the shapes around it.
        assert!(is_introspection_only("{ __schema { queryType { name } } }"));
        assert!(is_introspection_only(
            "query IntrospectionQuery { __schema { types { name } } }"
        ));
        assert!(is_introspection_only(
            "{ __type(name: \"Email\") { name } }"
        ));
        assert!(is_introspection_only("{ __typename }"));
    }

    #[test]
    fn real_fields_still_need_a_token() {
        assert!(!is_introspection_only("{ mailboxes { name } }"));
        // Mixed with introspection, and nested below it, still count as real.
        assert!(!is_introspection_only("{ __typename mailboxes { name } }"));
        assert!(!is_introspection_only(
            "mutation { sendEmail(action: PREVIEW) { preview } }"
        ));
        // Fragments could hide anything, and unparseable input proves nothing.
        assert!(!is_introspection_only(
            "{ ...F } fragment F on Query { __typename }"
        ));
        assert!(!is_introspection_only("{ this is not graphql"));
    }

    #[test]
    fn none_when_neither_header_nor_default() {
        // hosted mode with no upstream-injected token — must refuse.
        assert_eq!(resolve_token(Some(&headers_with(None)), None), None);
        assert_eq!(resolve_token(None, None), None);
    }
}

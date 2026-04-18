//! MCP (Model Context Protocol) server for Fastmail
//!
//! Exposes Fastmail functionality via two GraphQL tools:
//! - `schema_sdl` — returns the full GraphQL SDL for introspection
//! - `graphql` — executes a GraphQL query/mutation

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::jmap::JmapClient;

type ToolResult = std::result::Result<CallToolResult, McpError>;

pub mod graphql;

use graphql::FastmailSchema;

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
    #[allow(dead_code)] // referenced by #[tool_handler] macro expansion
    tool_router: ToolRouter<Self>,
}

impl FastmailMcp {
    pub async fn new() -> anyhow::Result<Self> {
        let config = Config::load()?;
        let token = config.get_token()?;

        let mut client = JmapClient::new(token);
        client.authenticate().await?;

        let schema = Arc::new(graphql::build_schema(Mutex::new(client)));

        Ok(Self {
            schema,
            tool_router: Self::tool_router(),
        })
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
    async fn graphql(&self, Parameters(req): Parameters<GraphqlRequest>) -> ToolResult {
        let mut request = async_graphql::Request::new(&req.query);

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

/// Run the MCP server with stdio transport
pub async fn run_server() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let service = FastmailMcp::new().await?;
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

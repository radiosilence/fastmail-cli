//! MCP (Model Context Protocol) server for Fastmail
//!
//! Exposes Fastmail functionality via two GraphQL tools:
//! - `schema_sdl` — returns the GraphQL SDL, whole or sliced to named types
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
mod sdl;

use graphql::{CardDavCreds, FastmailSchema, SharedClient};

/// Header carrying the per-request Fastmail API token in HTTP transport mode.
/// A trusted upstream (the hosted service, after authenticating the user) sets
/// this before proxying the request. Over stdio it is absent and the config
/// token is used instead.
pub const TOKEN_HEADER: &str = "x-fastmail-token";

/// Headers carrying the per-request CardDAV credentials, set by the same
/// trusted upstream as [`TOKEN_HEADER`].
///
/// Separate from the token because CardDAV is a separate protocol that rejects
/// API tokens outright — one bearer value cannot cover both, which is why the
/// gateway declares three credential fields for this backend rather than one.
pub const USERNAME_HEADER: &str = "x-fastmail-username";
pub const APP_PASSWORD_HEADER: &str = "x-fastmail-app-password";

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
    header_value(headers, TOKEN_HEADER).or_else(|| default.map(str::to_owned))
}

fn header_value(headers: Option<&http::HeaderMap>, name: &str) -> Option<String> {
    headers
        .and_then(|h| h.get(name))
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Per-request CardDAV credentials, resolved like [`resolve_token`]: the
/// request's own headers first, then whatever local config supplied.
///
/// Each half falls back independently. A deployment that stores a username but
/// no app password should get exactly that — half-configured, which
/// `carddavConfigured` reports as false — rather than silently mixing one
/// user's header with another's local default.
fn resolve_carddav(headers: Option<&http::HeaderMap>, default: &CardDavCreds) -> CardDavCreds {
    CardDavCreds {
        username: header_value(headers, USERNAME_HEADER).or_else(|| default.username.clone()),
        app_password: header_value(headers, APP_PASSWORD_HEADER)
            .or_else(|| default.app_password.clone()),
    }
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

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct SchemaRequest {
    /// Type names to return, e.g. `["QueryRoot", "EmailFilter"]`. Omit for the
    /// whole schema, which is large. Each named type comes back whole, with its
    /// documentation, but the types *it* references do not — name those too.
    #[serde(default)]
    pub types: Option<Vec<String>>,
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
    /// CardDAV credentials used when a request carries no credential headers.
    /// Exactly `default_token`'s counterpart: your own config over stdio,
    /// nothing in a hosted deployment.
    default_carddav: CardDavCreds,
    #[allow(dead_code)] // referenced by #[tool_handler] macro expansion
    tool_router: ToolRouter<Self>,
}

impl FastmailMcp {
    fn build(default_token: Option<String>) -> Self {
        Self {
            schema: Arc::new(graphql::build_schema()),
            clients: Arc::new(Mutex::new(HashMap::new())),
            default_token,
            default_carddav: CardDavCreds::from_local_config(),
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
        resolve_token(Self::headers(ctx), self.default_token.as_deref())
    }

    /// CardDAV credentials for this request — headers first, local config after.
    fn resolve_carddav(&self, ctx: &RequestContext<RoleServer>) -> CardDavCreds {
        resolve_carddav(Self::headers(ctx), &self.default_carddav)
    }

    /// The HTTP headers behind this request, absent over stdio.
    fn headers(ctx: &RequestContext<RoleServer>) -> Option<&http::HeaderMap> {
        ctx.extensions
            .get::<http::request::Parts>()
            .map(|p| &p.headers)
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
        description = "The full GraphQL SDL for the Fastmail API, with documentation on every type, argument and per-field cost.\n\
\n\
You do not need this for everyday mail — the `graphql` tool's own description already carries the queries, filters, fields and send flow for that. Reach for this when you want something it lists as not covered (attachment payloads, masked email, contacts, identities, moveEmail, markAsRead, markAsSpam, the remaining filter and sort options), or the exact cost of a field.\n\
\n\
Pass `types` to fetch only what you need: the whole schema is ~27KB, and `types: [\"MutationRoot\"]` or `[\"Attachment\", \"MaskedEmail\"]` is usually a few hundred bytes. Named types come back whole and documented, but the types they reference do not — name those too. An unrecognised name is reported back with the list of names that do exist. Omit `types` for the lot."
    )]
    async fn schema_sdl(&self, Parameters(req): Parameters<SchemaRequest>) -> ToolResult {
        let sdl = self.schema.sdl();
        match req.types {
            // An explicit empty list means "no types", which is never what a
            // caller wants; read it as the whole schema.
            Some(types) if !types.is_empty() => Self::text_result(sdl::slice(&sdl, &types)),
            _ => Self::text_result(sdl),
        }
    }

    #[tool(
        title = "Fastmail",
        description = "Execute a GraphQL query or mutation against the Fastmail API. Variables go as a JSON string.

Everyday mail is covered below — call `schema_sdl` only for what isn't.

QUERIES
  session: Session!                     # { status carddavConfigured username }
  mailboxes(first: Int): MailboxConnection!
  mailbox(name: String!): Mailbox       # name or role — \"INBOX\", \"sent\", \"drafts\"
  emails(filter: EmailFilter, sort: [EmailSort!], collapseThreads: Boolean,
         first: Int, after: String): EmailConnection!
  email(id: String!): Email
  thread(emailId: String!): Thread!     # { total emails { nodes { ... } } }

EmailFilter — scalars on one object AND together, and/or/not nest arbitrarily:
  text from to cc subject body: String  # text searches all of them
  inMailbox: String                     # name or role
  inMailboxOtherThan: [String!]
  unread flagged hasAttachment: Boolean
  before after: String                  # YYYY-MM-DD or ISO 8601
  hasKeyword notKeyword: String         # e.g. \"$answered\", \"$draft\"
  and: [EmailFilter!]  or: [EmailFilter!]  not: EmailFilter

Email fields: id subject preview textBody htmlBody receivedAt sentAt size
  from to cc bcc { name email }  isUnread isFlagged isDraft hasAttachment
  mailboxes { name role }  thread { total }  attachments { nodes { name size } }

Connections: `nodes` for items, first/last/after/before to page (default 25,
max 100), cursors are IDs, `pageInfo { hasNextPage endCursor }`. `totalCount`
is only computed when selected.

MUTATIONS — sendEmail, replyToEmail and forwardEmail all take
`action: PREVIEW | CONFIRM | DRAFT`. PREVIEW sends nothing and returns a
confirmationToken; CONFIRM repeats the same to/subject/body plus that token, and
is rejected if they differ. Recipients are comma-separated strings, not lists.
  sendEmail(action: SendAction!, to: String!, subject: String!, body: String!,
            cc: String, bcc: String, from: String, htmlBody: String,
            confirmationToken: String): ComposeResult!
  ComposeResult { success emailId preview confirmationToken error }

EXAMPLES
```
{ emails(filter: {unread: true, inMailbox: \"INBOX\", not: {hasKeyword: \"$answered\"}, or: [{from: \"a@b.com\"}, {to: \"a@b.com\"}]}, first: 10) { totalCount nodes { id subject from { email } } } }
mutation { sendEmail(action: PREVIEW, to: \"a@b.com\", subject: \"Hi\", body: \"...\") { preview confirmationToken } }
mutation { sendEmail(action: CONFIRM, to: \"a@b.com\", subject: \"Hi\", body: \"...\", confirmationToken: \"<from preview>\") { success emailId } }
```

SUBSCRIPTIONS are not available through this tool — it is request/response, and
a subscription never returns. The schema defines one (`emails`, streaming mail
as it arrives) for callers on the HTTP surface, which serves it over SSE at
`/graphql/stream`; `fastmail watch` is the same thing as a CLI.

NOT LISTED ABOVE — ask `schema_sdl` for these rather than guessing: attachment
payloads (base64/image/text), masked email, contacts and contact CRUD (CardDAV,
so check `session { carddavConfigured }` first), identities, moveEmail,
markAsRead, markAsSpam, and the remaining filter and sort options."
    )]
    async fn graphql(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(req): Parameters<GraphqlRequest>,
    ) -> ToolResult {
        let Some(token) = self.resolve_token(&ctx) else {
            return Self::error_result(
                "No Fastmail token available. Configure one via `fastmail auth` \
                 (stdio) or send the X-Fastmail-Token header (HTTP).",
            );
        };
        let client = match client_for(&self.clients, &token).await {
            Ok(client) => client,
            Err(e) => return Self::error_result(format!("Fastmail authentication failed: {e}")),
        };

        let mut request = graphql::request(&req.query, client, self.resolve_carddav(&ctx));

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
        let server_info = Implementation::new("fastmail", env!("CARGO_PKG_VERSION"))
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
                The `graphql` tool's description carries the queries, filters, \
                fields and send flow for everyday mail, so most sessions need no \
                schema fetch at all. `schema_sdl` has the rest, and takes a \
                `types` list so you can read one corner of it rather than all \
                ~27KB. Variables go as a JSON string.\n\n\
                Contacts are the one thing to check before planning around: they \
                go over CardDAV, which the API token does not cover, and \
                `{ session { status carddavConfigured } }` answers it without \
                failing a query first.\n\n\
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

/// Resolve credentials and build the GraphQL request an HTTP body describes.
///
/// Shared by the query and subscription endpoints so they cannot drift on which
/// token wins or what counts as introspection. The error case is a message for
/// the caller, not a status code — GraphQL reports its own failures in the
/// response body.
async fn build_http_request(
    mcp: &FastmailMcp,
    headers: &http::HeaderMap,
    req: HttpGraphqlRequest,
) -> std::result::Result<async_graphql::Request, String> {
    // Introspection is answered from the schema, so it neither needs a token nor
    // touches the network — the IDE stays usable while credentials are wrong.
    let mut request = if is_introspection_only(&req.query) {
        async_graphql::Request::new(&req.query)
    } else {
        // Must keep honouring `mcp.default_token` here: GraphiQL runs in a
        // browser and cannot attach the token header, so making this
        // headers-only breaks local development.
        let Some(token) = resolve_token(Some(headers), mcp.default_token.as_deref()) else {
            return Err(format!(
                "No Fastmail token available. Configure one via `fastmail auth` \
                 or send the {TOKEN_HEADER} header."
            ));
        };
        // Authenticated on first use rather than at startup, so a missing or
        // expired token surfaces in the response pane instead of stopping the
        // server booting.
        let client = client_for(&mcp.clients, &token)
            .await
            .map_err(|e| format!("Fastmail authentication failed: {e}"))?;
        graphql::request(
            &req.query,
            client,
            resolve_carddav(Some(headers), &mcp.default_carddav),
        )
    };
    if let Some(vars) = req.variables {
        request = request.variables(async_graphql::Variables::from_json(vars));
    }
    if let Some(name) = req.operation_name {
        request = request.operation_name(name);
    }
    Ok(request)
}

/// Plain GraphQL-over-HTTP, for browsers and anything else that speaks it
/// directly rather than through MCP's JSON-RPC envelope. Shares the server's
/// schema, client cache and token resolution with the `graphql` tool.
async fn graphql_endpoint(
    axum::extract::State(mcp): axum::extract::State<FastmailMcp>,
    headers: http::HeaderMap,
    axum::Json(req): axum::Json<HttpGraphqlRequest>,
) -> axum::Json<async_graphql::Response> {
    match build_http_request(&mcp, &headers, req).await {
        Ok(request) => axum::Json(mcp.schema.execute(request).await),
        Err(msg) => axum::Json(async_graphql::Response::from_errors(vec![
            async_graphql::ServerError::new(msg, None),
        ])),
    }
}

/// GraphQL subscriptions over Server-Sent Events, one event per response.
///
/// SSE rather than WebSockets because the only subscription here is a
/// server-to-client firehose: nothing is ever sent back up the socket, and SSE
/// reconnects on its own. It is also the same shape the CLI consumes from
/// Fastmail, which keeps one mental model for the whole path.
async fn graphql_stream_endpoint(
    axum::extract::State(mcp): axum::extract::State<FastmailMcp>,
    headers: http::HeaderMap,
    axum::Json(req): axum::Json<HttpGraphqlRequest>,
) -> axum::response::Response {
    use async_graphql::futures_util::stream::StreamExt;
    use axum::response::{IntoResponse, Sse, sse};

    let request = match build_http_request(&mcp, &headers, req).await {
        Ok(request) => request,
        Err(msg) => {
            return axum::Json(async_graphql::Response::from_errors(vec![
                async_graphql::ServerError::new(msg, None),
            ]))
            .into_response();
        }
    };

    let events = mcp.schema.execute_stream(request).map(|response| {
        let data = serde_json::to_string(&response)
            .unwrap_or_else(|e| format!(r#"{{"errors":[{{"message":"{e}"}}]}}"#));
        Ok::<_, std::convert::Infallible>(sse::Event::default().data(data))
    });

    // Proxies drop connections that go quiet, and a mail subscription is quiet
    // most of the time.
    Sse::new(events)
        .keep_alive(sse::KeepAlive::default())
        .into_response()
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
        router = router
            .route("/graphql", axum::routing::post(graphql_endpoint))
            .route(
                "/graphql/stream",
                axum::routing::post(graphql_stream_endpoint),
            );
        tracing::info!("GraphQL endpoint on http://{addr}/graphql");
        tracing::info!("GraphQL subscriptions (SSE) on http://{addr}/graphql/stream");
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

    fn carddav_headers(username: Option<&str>, app_password: Option<&str>) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        if let Some(u) = username {
            headers.insert(USERNAME_HEADER, u.parse().unwrap());
        }
        if let Some(p) = app_password {
            headers.insert(APP_PASSWORD_HEADER, p.parse().unwrap());
        }
        headers
    }

    fn local_carddav() -> CardDavCreds {
        CardDavCreds {
            username: Some("local@example.com".into()),
            app_password: Some("local-password".into()),
        }
    }

    #[test]
    fn carddav_headers_win_over_local_config() {
        // The hosted path: the gateway injects one user's credentials, and they
        // must not be shadowed by whatever the host machine happens to hold.
        let headers = carddav_headers(Some("hosted@example.com"), Some("hosted-password"));
        let got = resolve_carddav(Some(&headers), &local_carddav());

        assert_eq!(got.username.as_deref(), Some("hosted@example.com"));
        assert_eq!(got.app_password.as_deref(), Some("hosted-password"));
    }

    #[test]
    fn carddav_falls_back_to_local_config_over_stdio() {
        let got = resolve_carddav(None, &local_carddav());
        assert_eq!(got.username.as_deref(), Some("local@example.com"));
        assert!(got.is_complete());
    }

    #[test]
    fn a_hosted_deployment_with_no_carddav_credentials_reports_incomplete() {
        // No headers, no local config — `contacts` is genuinely unavailable,
        // and `carddavConfigured` must say so rather than half-claiming it.
        let got = resolve_carddav(Some(&http::HeaderMap::new()), &CardDavCreds::default());
        assert!(!got.is_complete());
        assert!(got.username.is_none() && got.app_password.is_none());
    }

    #[test]
    fn each_half_of_the_carddav_credential_falls_back_on_its_own() {
        // A username header with no password header is half a credential, and
        // completing it from local config would mix two users together.
        let headers = carddav_headers(Some("hosted@example.com"), None);
        let got = resolve_carddav(Some(&headers), &CardDavCreds::default());

        assert_eq!(got.username.as_deref(), Some("hosted@example.com"));
        assert!(got.app_password.is_none());
        assert!(!got.is_complete());
    }

    #[test]
    fn an_empty_credential_header_is_not_a_credential() {
        // The gateway skips a field the user left blank, but a proxy that sends
        // the header empty must not read as "configured".
        let headers = carddav_headers(Some(""), Some(""));
        assert!(!resolve_carddav(Some(&headers), &CardDavCreds::default()).is_complete());
    }

    /// The text a tool call came back with.
    fn text_of(result: CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    #[tokio::test]
    async fn schema_sdl_without_arguments_still_returns_everything() {
        // `types` was added to a tool that took no arguments at all, and rmcp
        // reads absent arguments as `{}` — so an existing client that sends
        // none must keep getting the whole schema.
        let empty: SchemaRequest = serde_json::from_str("{}").unwrap();
        assert!(empty.types.is_none());

        let sdl = text_of(
            FastmailMcp::http()
                .schema_sdl(Parameters(empty))
                .await
                .unwrap(),
        );
        assert!(sdl.contains("type QueryRoot {"));
        assert!(sdl.contains("type Session {"));
        assert!(sdl.contains("input EmailFilter {"));
    }

    #[tokio::test]
    async fn schema_sdl_with_types_returns_only_those() {
        let mcp = FastmailMcp::http();
        let sliced = text_of(
            mcp.schema_sdl(Parameters(SchemaRequest {
                types: Some(vec!["Session".into()]),
            }))
            .await
            .unwrap(),
        );

        assert!(sliced.contains("type Session {"));
        assert!(!sliced.contains("input EmailFilter {"));

        let full = text_of(
            mcp.schema_sdl(Parameters(SchemaRequest::default()))
                .await
                .unwrap(),
        );
        assert!(
            sliced.len() * 10 < full.len(),
            "{} of {} is not a saving worth the argument",
            sliced.len(),
            full.len()
        );
    }

    #[tokio::test]
    async fn an_empty_types_list_is_read_as_the_whole_schema() {
        // Never a useful request, and returning nothing would look like a bug
        // in the schema rather than in the call.
        let sdl = text_of(
            FastmailMcp::http()
                .schema_sdl(Parameters(SchemaRequest {
                    types: Some(Vec::new()),
                }))
                .await
                .unwrap(),
        );
        assert!(sdl.contains("type QueryRoot {"));
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

    /// GraphiQL runs entirely in the browser, with no way to attach the
    /// `x-fastmail-token` header, so `/graphql` has to keep honouring the
    /// locally configured token when a request carries none — otherwise the
    /// IDE that exists specifically to explore the API without ceremony
    /// becomes unusable the moment credential resolution changes.
    #[tokio::test]
    async fn graphql_falls_back_to_local_config_when_no_headers() {
        let mcp = FastmailMcp::build(Some("fake-token".to_string()));

        // Pre-seed the client cache with a client whose session already
        // points at an address that refuses connections instantly, so the
        // resolver's JMAP call fails fast instead of reaching the real
        // Fastmail API with a fake token.
        let client = JmapClient::with_test_session("http://127.0.0.1:1");
        mcp.clients
            .lock()
            .await
            .insert("fake-token".to_string(), Arc::new(Mutex::new(client)));

        let req = HttpGraphqlRequest {
            query: "{ session { status } }".to_string(),
            variables: None,
            operation_name: None,
        };

        let response = graphql_endpoint(
            axum::extract::State(mcp),
            http::HeaderMap::new(),
            axum::Json(req),
        )
        .await;

        let body = serde_json::to_string(&response.0).unwrap();
        assert!(
            !body.contains("No Fastmail token available"),
            "expected the default token to be used, got: {body}"
        );
    }
}

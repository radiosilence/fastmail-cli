//! GraphQL schema for Fastmail MCP
//!
//! Provides a complete GraphQL schema that wraps the JMAP and CardDAV clients,
//! replacing the previous 18 individual MCP tools with a composable query interface.

use async_graphql::Schema;

pub mod connection;
pub mod filter;
pub mod loaders;
mod mutation;
mod query;
#[cfg(test)]
mod tests;
pub mod types;

use mutation::MutationRoot;
use query::QueryRoot;

pub type FastmailSchema = Schema<QueryRoot, MutationRoot, async_graphql::EmptySubscription>;

/// The per-request JMAP client, injected into each GraphQL execution as request
/// data. Shared (`Arc`) so an authenticated client can be reused across requests
/// for the same Fastmail token rather than re-authenticating every call.
pub type SharedClient = std::sync::Arc<tokio::sync::Mutex<crate::jmap::JmapClient>>;

/// The credentials `contacts` needs.
///
/// CardDAV authenticates with a username and an app password and rejects API
/// tokens, so neither half comes from the JMAP credential — contacts can be
/// unreachable on an otherwise perfectly good connection.
///
/// Injected as request data rather than read inside a resolver so that
/// `Session.carddavConfigured` and the `contacts` query answer from the same
/// value. Reading the config in both places would let them disagree, and a
/// reachability flag that disagrees with the operation it describes is worse
/// than no flag.
#[derive(Clone, Debug, Default)]
pub struct CardDavCreds {
    pub username: Option<String>,
    pub app_password: Option<String>,
}

impl CardDavCreds {
    /// Read from `~/.config/fastmail-cli/config.toml` and the environment.
    ///
    /// The fallback for when a request carries no credential headers, exactly
    /// as [`crate::mcp::local_token`] is for the token: running this yourself
    /// picks up your own credentials, while a hosted deployment ships no local
    /// config and every request must bring its own.
    pub fn from_local_config() -> Self {
        let Ok(config) = crate::config::Config::load() else {
            return Self::default();
        };
        Self {
            username: config.get_username().ok(),
            app_password: config.get_app_password().ok(),
        }
    }

    /// Both halves present, so a CardDAV request can at least be attempted.
    /// Says nothing about whether the credentials are *correct*.
    pub fn is_complete(&self) -> bool {
        self.username.is_some() && self.app_password.is_some()
    }
}

/// Maximum selection-set nesting. The graph contains cycles by design — an
/// email's thread contains emails, a mailbox's emails belong to mailboxes — so
/// unbounded depth would let one query walk forever. 15 is far past any useful
/// query (mailbox → emails → thread → emails → attachments → text is 6).
const MAX_DEPTH: usize = 15;

/// Build the GraphQL schema with only the process-shared preview-nonce store.
///
/// The JMAP client is **not** baked in — it is supplied per request via
/// [`async_graphql::Request::data`] so a single schema can serve many tenants,
/// each with their own Fastmail token. The nonce store stays schema-level
/// because send preview→confirm spans two separate requests.
pub fn build_schema() -> FastmailSchema {
    // Complexity is deliberately **not** capped. Resolvers still declare costs
    // (the `complexity` attributes, priced so a document parse reads as far more
    // expensive than a download) but those are guidance, surfaced in the field
    // descriptions so a caller can choose a sensible page size — not a limit
    // that refuses the query. Rejecting an expensive-but-legitimate request
    // leaves the caller guessing at a threshold it cannot see.
    //
    // Depth stays capped: the graph contains cycles, and nothing else bounds
    // them.
    Schema::build(QueryRoot, MutationRoot, async_graphql::EmptySubscription)
        .data(types::NonceStore::default())
        .limit_depth(MAX_DEPTH)
        .finish()
}

/// Build a GraphQL request carrying everything a resolver may need: the
/// authenticated JMAP client, whatever CardDAV credentials exist locally, and a
/// fresh set of DataLoaders.
///
/// Loaders are per request on purpose — their cache is then a request-scoped
/// cache, so repeating a key inside one query is free while nothing is retained
/// long enough to go stale.
pub fn request(query: &str, client: SharedClient, carddav: CardDavCreds) -> async_graphql::Request {
    let loaders = loaders::Loaders::new(client.clone());
    async_graphql::Request::new(query)
        .data(client)
        .data(carddav)
        .data(loaders.email)
        .data(loaders.mailbox)
        .data(loaders.identity)
        .data(loaders.masked)
        .data(loaders.thread)
        .data(loaders.blob)
}

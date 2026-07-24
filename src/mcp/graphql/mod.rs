//! GraphQL schema for Fastmail MCP
//!
//! Provides a complete GraphQL schema that wraps the JMAP and CardDAV clients,
//! replacing the previous 18 individual MCP tools with a composable query interface.

use async_graphql::Schema;

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

/// Build the GraphQL schema with only the process-shared preview-nonce store.
///
/// The JMAP client is **not** baked in — it is supplied per request via
/// [`async_graphql::Request::data`] so a single schema can serve many tenants,
/// each with their own Fastmail token. The nonce store stays schema-level
/// because send preview→confirm spans two separate requests.
/// Maximum selection-set nesting. The graph contains cycles by design — an
/// email's thread contains emails, a mailbox's emails belong to mailboxes — so
/// unbounded depth would let one query walk forever. 15 is far past any useful
/// query (mailbox → emails → thread → emails → attachments → content is 6).
const MAX_DEPTH: usize = 15;

/// Maximum query complexity. Nested list fields declare a cost proportional to
/// their page size (see the `complexity` attributes on the resolvers), so this
/// bounds the *fan-out* a single query can request, which depth alone does not.
const MAX_COMPLEXITY: usize = 5_000;

pub fn build_schema() -> FastmailSchema {
    Schema::build(QueryRoot, MutationRoot, async_graphql::EmptySubscription)
        .data(types::NonceStore::default())
        .limit_depth(MAX_DEPTH)
        .limit_complexity(MAX_COMPLEXITY)
        .finish()
}

/// Build a GraphQL request carrying everything a resolver may need: the
/// authenticated JMAP client plus a fresh set of DataLoaders.
///
/// Loaders are per request on purpose — their cache is then a request-scoped
/// cache, so repeating a key inside one query is free while nothing is retained
/// long enough to go stale.
pub fn request(query: &str, client: SharedClient) -> async_graphql::Request {
    let loaders = loaders::Loaders::new(client.clone());
    async_graphql::Request::new(query)
        .data(client)
        .data(loaders.email)
        .data(loaders.mailbox)
        .data(loaders.thread)
        .data(loaders.blob)
}

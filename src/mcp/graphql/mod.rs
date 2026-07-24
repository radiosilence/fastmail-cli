//! GraphQL schema for Fastmail MCP
//!
//! Provides a complete GraphQL schema that wraps the JMAP and CardDAV clients,
//! replacing the previous 18 individual MCP tools with a composable query interface.

use async_graphql::Schema;

mod mutation;
mod query;
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
pub fn build_schema() -> FastmailSchema {
    Schema::build(QueryRoot, MutationRoot, async_graphql::EmptySubscription)
        .data(types::NonceStore::default())
        .finish()
}

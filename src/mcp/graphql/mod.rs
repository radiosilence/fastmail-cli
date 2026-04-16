//! GraphQL schema for Fastmail MCP
//!
//! Provides a complete GraphQL schema that wraps the JMAP and CardDAV clients,
//! replacing the previous 18 individual MCP tools with a composable query interface.

use async_graphql::Schema;
use tokio::sync::Mutex;

use crate::jmap::JmapClient;

mod mutation;
mod query;
pub mod types;

use mutation::MutationRoot;
use query::QueryRoot;

pub type FastmailSchema = Schema<QueryRoot, MutationRoot, async_graphql::EmptySubscription>;

/// Build the GraphQL schema with the JMAP client and the preview-nonce store
/// injected as context data.
pub fn build_schema(client: Mutex<JmapClient>) -> FastmailSchema {
    Schema::build(QueryRoot, MutationRoot, async_graphql::EmptySubscription)
        .data(client)
        .data(types::NonceStore::default())
        .finish()
}

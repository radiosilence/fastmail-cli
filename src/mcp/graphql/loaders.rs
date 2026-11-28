//! DataLoaders backing the nested GraphQL resolvers.
//!
//! Every nested field that needs another API call goes through one of these
//! instead of hitting the JMAP client directly. async-graphql resolves sibling
//! fields and list elements concurrently, so N resolvers each asking for one key
//! collapse into a single batched request per loader.
//!
//! Loaders are built **per GraphQL request** (see [`super::request`]) — their
//! internal cache is therefore a per-request cache, which is what you want:
//! asking for the same email twice in one query costs one fetch, and nothing is
//! held across requests where it could go stale.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dataloader::{DataLoader, HashMapCache, Loader};
use async_graphql::futures_util::future::join_all;

use super::SharedClient;
use crate::error::Error;
use crate::models::{Email, Identity, Mailbox, MaskedEmail};

/// Loader errors are cloned out to every waiter in a batch, so they must be
/// cheap to clone — hence the `Arc`.
pub type LoadError = Arc<Error>;

/// Turn a loader error into a GraphQL error. `Arc<Error>` can't implement
/// `Into<async_graphql::Error>` here (both types are foreign), so resolvers call
/// this explicitly.
pub fn to_gql_error(e: LoadError) -> async_graphql::Error {
    async_graphql::Error::new(e.to_string())
}

/// Batches `Email/get` for full email content (bodies + attachment metadata).
pub struct EmailLoader(SharedClient);

impl Loader<String> for EmailLoader {
    /// Shared, because every lazy field on an email asks the loader for it
    /// again, and an email with its bodies is the largest thing loaded.
    type Value = Arc<Email>;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Arc<Email>>, Self::Error> {
        let client = &self.0;
        let emails = client.get_emails(keys).await.map_err(Arc::new)?;
        // Missing IDs are simply absent from the map — `load_one` then yields
        // `Ok(None)`, which resolvers surface as a null rather than an error.
        Ok(emails
            .into_iter()
            .map(|e| (e.id.clone(), Arc::new(e)))
            .collect())
    }
}

/// Loads the account's whole mailbox list.
///
/// JMAP offers no way to fetch mailboxes by id more cheaply than fetching the
/// lot, and every mailbox question — by id, by name, children of, the full
/// list — is answered by filtering it. So there is one loader, keyed by `()`,
/// and each of those questions is in-memory work on its single result.
///
/// Keying by unit puts the deduplication in the request-scoped cache:
/// `{ mailboxes { children { name } } }` resolves `children` once per mailbox
/// and still issues exactly one `Mailbox/get`.
pub struct MailboxListLoader(SharedClient);

impl Loader<()> for MailboxListLoader {
    type Value = Arc<Vec<Mailbox>>;
    type Error = LoadError;

    async fn load(&self, _keys: &[()]) -> Result<HashMap<(), Arc<Vec<Mailbox>>>, Self::Error> {
        let client = &self.0;
        let mailboxes = client.fetch_mailboxes().await.map_err(Arc::new)?;
        Ok(HashMap::from([((), Arc::new(mailboxes))]))
    }
}

/// Loads the account's sender identities. Like mailboxes this is a whole-list
/// fetch with no per-id form, so it is keyed by `()` and deduplicated by the
/// request-scoped cache.
pub struct IdentityListLoader(SharedClient);

impl Loader<()> for IdentityListLoader {
    type Value = Arc<Vec<Identity>>;
    type Error = LoadError;

    async fn load(&self, _keys: &[()]) -> Result<HashMap<(), Arc<Vec<Identity>>>, Self::Error> {
        let client = &self.0;
        let identities = client.list_identities().await.map_err(Arc::new)?;
        Ok(HashMap::from([((), Arc::new(identities))]))
    }
}

/// Loads the account's masked email addresses. Whole-list, keyed by `()`.
pub struct MaskedEmailListLoader(SharedClient);

impl Loader<()> for MaskedEmailListLoader {
    type Value = Arc<Vec<MaskedEmail>>;
    type Error = LoadError;

    async fn load(&self, _keys: &[()]) -> Result<HashMap<(), Arc<Vec<MaskedEmail>>>, Self::Error> {
        let client = &self.0;
        let masked = client.list_masked_emails().await.map_err(Arc::new)?;
        Ok(HashMap::from([((), Arc::new(masked))]))
    }
}

/// Batches `Thread/get`, resolving thread IDs to their member email IDs.
pub struct ThreadLoader(SharedClient);

impl Loader<String> for ThreadLoader {
    type Value = Vec<String>;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Vec<String>>, Self::Error> {
        let client = &self.0;
        client.thread_email_ids(keys).await.map_err(Arc::new)
    }
}

/// Downloads attachment blobs.
///
/// Blob downloads are individual HTTP GETs — JMAP has no batch endpoint for
/// them — so "batching" here means issuing the batch's downloads
/// *concurrently* rather than one at a time, with the client bounding how many
/// run at once. Deduplication of repeated blob IDs within a batch is the other
/// win.
pub struct BlobLoader(SharedClient);

impl Loader<String> for BlobLoader {
    type Value = Arc<Vec<u8>>;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Arc<Vec<u8>>>, Self::Error> {
        let client = &self.0;
        let results = join_all(keys.iter().map(|id| client.download_blob(id))).await;

        let mut out = HashMap::with_capacity(keys.len());
        for (id, data) in keys.iter().zip(results) {
            out.insert(id.clone(), Arc::new(data.map_err(Arc::new)?));
        }
        Ok(out)
    }
}

/// Batches `SearchSnippet/get`. Keyed by (JMAP filter as JSON, email ID), so
/// a page of results from one search costs one call, and two searches in one
/// query cost one each.
pub struct SnippetLoader(SharedClient);

impl Loader<(String, String)> for SnippetLoader {
    type Value = Arc<crate::jmap::Snippet>;
    type Error = LoadError;

    async fn load(
        &self,
        keys: &[(String, String)],
    ) -> Result<HashMap<(String, String), Self::Value>, Self::Error> {
        let mut by_filter: HashMap<&str, Vec<String>> = HashMap::new();
        for (filter, id) in keys {
            by_filter.entry(filter).or_default().push(id.clone());
        }
        let mut out = HashMap::new();
        for (filter, ids) in by_filter {
            let parsed: serde_json::Value =
                serde_json::from_str(filter).map_err(|e| Arc::new(Error::Json(e)))?;
            for snippet in self
                .0
                .search_snippets(&parsed, &ids)
                .await
                .map_err(Arc::new)?
            {
                out.insert(
                    (filter.to_string(), snippet.email_id.clone()),
                    Arc::new(snippet),
                );
            }
        }
        Ok(out)
    }
}

/// Batches `Email/parse` over message blobs: attached emails, by blob ID.
pub struct ParsedEmailLoader(SharedClient);

impl Loader<String> for ParsedEmailLoader {
    type Value = Arc<Email>;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Arc<Email>>, Self::Error> {
        Ok(self
            .0
            .parse_emails(keys)
            .await
            .map_err(Arc::new)?
            .into_iter()
            .map(|(k, v)| (k, Arc::new(v)))
            .collect())
    }
}

/// Cap on IDs per `Email/get`. JMAP servers bound how many records one call may
/// request; this stays well inside Fastmail's limit while still collapsing a
/// full page of list results into one round trip.
const EMAIL_BATCH: usize = 100;

/// Cap on concurrent blob downloads. Attachments are large and are held in
/// memory while being processed, so this is deliberately small.
const BLOB_BATCH: usize = 4;

// Loader handles as resolvers see them. `DataLoader::new` builds a `NoCache`
// loader — it only deduplicates within a single batch — so the record loaders
// are built with `with_cache` to get a genuine request-scoped cache: an email
// pulled for one field is free for every later field in the same query.
pub type Emails = DataLoader<EmailLoader, HashMapCache>;
pub type Mailboxes = DataLoader<MailboxListLoader, HashMapCache>;
pub type Identities = DataLoader<IdentityListLoader, HashMapCache>;
pub type MaskedEmails = DataLoader<MaskedEmailListLoader, HashMapCache>;
pub type Threads = DataLoader<ThreadLoader, HashMapCache>;
/// Blobs stay uncached: the bytes are large, each is consumed once, and the
/// resolver keeps the processed output — caching the raw download on top would
/// only inflate peak memory. Within-batch deduplication still applies.
pub type Blobs = DataLoader<BlobLoader>;
pub type Snippets = DataLoader<SnippetLoader, HashMapCache>;
pub type ParsedEmails = DataLoader<ParsedEmailLoader, HashMapCache>;

/// All loaders for one GraphQL request, ready to be attached as request data.
pub struct Loaders {
    pub email: Emails,
    pub mailbox: Mailboxes,
    pub identity: Identities,
    pub masked: MaskedEmails,
    pub thread: Threads,
    pub blob: Blobs,
    pub snippet: Snippets,
    pub parsed: ParsedEmails,
}

impl Loaders {
    pub fn new(client: SharedClient) -> Self {
        Self {
            email: Emails::with_cache(
                EmailLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            )
            .max_batch_size(EMAIL_BATCH),
            mailbox: Mailboxes::with_cache(
                MailboxListLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            ),
            identity: Identities::with_cache(
                IdentityListLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            ),
            masked: MaskedEmails::with_cache(
                MaskedEmailListLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            ),
            thread: Threads::with_cache(
                ThreadLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            ),
            snippet: Snippets::with_cache(
                SnippetLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            )
            .max_batch_size(EMAIL_BATCH),
            parsed: ParsedEmails::with_cache(
                ParsedEmailLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            )
            .max_batch_size(BLOB_BATCH),
            blob: Blobs::new(BlobLoader(client), tokio::spawn).max_batch_size(BLOB_BATCH),
        }
    }
}

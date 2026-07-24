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
use crate::models::{Email, Mailbox};

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
    type Value = Email;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Email>, Self::Error> {
        let client = self.0.lock().await;
        let emails = client.get_emails(keys).await.map_err(Arc::new)?;
        // Missing IDs are simply absent from the map — `load_one` then yields
        // `Ok(None)`, which resolvers surface as a null rather than an error.
        Ok(emails.into_iter().map(|e| (e.id.clone(), e)).collect())
    }
}

/// Resolves mailbox IDs to mailboxes. The JMAP client caches the full mailbox
/// list after the first call, so a whole request costs at most one `Mailbox/get`
/// no matter how many mailboxes are referenced.
pub struct MailboxLoader(SharedClient);

impl Loader<String> for MailboxLoader {
    type Value = Mailbox;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Mailbox>, Self::Error> {
        let mut client = self.0.lock().await;
        let mailboxes = client.list_mailboxes().await.map_err(Arc::new)?;
        Ok(mailboxes
            .into_iter()
            .filter(|m| keys.contains(&m.id))
            .map(|m| (m.id.clone(), m))
            .collect())
    }
}

/// Batches `Thread/get`, resolving thread IDs to their member email IDs.
pub struct ThreadLoader(SharedClient);

impl Loader<String> for ThreadLoader {
    type Value = Vec<String>;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Vec<String>>, Self::Error> {
        let client = self.0.lock().await;
        client.thread_email_ids(keys).await.map_err(Arc::new)
    }
}

/// Downloads attachment blobs.
///
/// Blob downloads are individual HTTP GETs — JMAP has no batch endpoint for
/// them — so "batching" here means issuing the batch's downloads *concurrently*
/// under a single client lock, rather than one at a time. Deduplication of
/// repeated blob IDs within a batch is the other win.
pub struct BlobLoader(SharedClient);

impl Loader<String> for BlobLoader {
    type Value = Arc<Vec<u8>>;
    type Error = LoadError;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Arc<Vec<u8>>>, Self::Error> {
        let client = self.0.lock().await;
        let results = join_all(keys.iter().map(|id| client.download_blob(id))).await;

        let mut out = HashMap::with_capacity(keys.len());
        for (id, data) in keys.iter().zip(results) {
            out.insert(id.clone(), Arc::new(data.map_err(Arc::new)?));
        }
        Ok(out)
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
pub type Mailboxes = DataLoader<MailboxLoader, HashMapCache>;
pub type Threads = DataLoader<ThreadLoader, HashMapCache>;
/// Blobs stay uncached: the bytes are large, each is consumed once, and the
/// resolver keeps the processed output — caching the raw download on top would
/// only inflate peak memory. Within-batch deduplication still applies.
pub type Blobs = DataLoader<BlobLoader>;

/// All loaders for one GraphQL request, ready to be attached as request data.
pub struct Loaders {
    pub email: Emails,
    pub mailbox: Mailboxes,
    pub thread: Threads,
    pub blob: Blobs,
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
                MailboxLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            ),
            thread: Threads::with_cache(
                ThreadLoader(client.clone()),
                tokio::spawn,
                HashMapCache::default(),
            ),
            blob: Blobs::new(BlobLoader(client), tokio::spawn).max_batch_size(BLOB_BATCH),
        }
    }
}

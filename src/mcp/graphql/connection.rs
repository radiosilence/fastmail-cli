//! Relay-style connections over JMAP's `Email/query`.
//!
//! Cursors are **email IDs**, mapped onto JMAP's `anchor` / `anchorOffset`
//! arguments. That falls out of what the API already offers, and it is the
//! reason pagination here is stable: a positional cursor silently shifts every
//! time mail arrives, so `after: "24"` would re-show or skip messages between
//! pages. An anchor names a specific message, so the page after it is the same
//! page whatever else changed. It stays legible for a model composing the
//! follow-up query, too — the cursor is just an ID it has already seen.
//!
//! Backward pagination uses the same machinery: `last` without `before` becomes
//! a negative `position`, which JMAP counts from the end, so "the last N" costs
//! one call and never needs a total.

use std::collections::{HashMap, HashSet};

use async_graphql::connection::{Connection, Edge, EmptyFields, query};
use async_graphql::{Context, Result, SimpleObject};
use serde_json::Value;

use super::SharedClient;
use super::filter::{EmailFilter, EmailSort, and_also, sort_to_jmap};
use super::types::{GqlEmail, clamp_page};
use crate::jmap::{EmailQuery, QueryStart};

/// Connection-level fields beyond the Relay defaults.
#[derive(SimpleObject)]
pub struct EmailConnectionFields {
    /// Total number of emails matching the filter, ignoring pagination.
    ///
    /// Computing this costs the server extra work, so it is only requested when
    /// you actually select the field — a query that omits it pays nothing.
    pub total_count: Option<u64>,
    /// Zero-based index of the first returned email within the full result set.
    pub position: u64,
    /// Opaque server state for this query. Two pages sharing a `queryState`
    /// came from the same result set; a change means messages arrived, moved,
    /// or were deleted in between.
    pub query_state: Option<String>,
}

/// Cursors are email IDs — see the module docs for why.
pub type EmailConnection = Connection<String, GqlEmail, EmailConnectionFields>;

/// Arguments every email connection accepts.
pub struct PageArgs {
    pub after: Option<String>,
    pub before: Option<String>,
    pub first: Option<i32>,
    pub last: Option<i32>,
}

/// Cost of a connection field for the complexity limit: the page size the
/// caller asked for, times the cost of each node.
pub fn page_complexity(first: Option<i32>, last: Option<i32>, child_complexity: usize) -> usize {
    let requested = first.or(last).map(|n| n.max(0) as u32);
    clamp_page(requested) as usize * child_complexity
}

/// Turn JMAP's `anchorNotFound` into advice the caller can act on.
///
/// An anchor cursor points at a specific message; if that message is deleted or
/// filtered out before the next page is requested, the anchor no longer exists.
/// That's the trade for stability, and the recovery is simply to start over.
fn stale_anchor_hint(e: crate::error::Error) -> async_graphql::Error {
    if let crate::error::Error::Jmap { ref error_type, .. } = e
        && error_type == "anchorNotFound"
    {
        return async_graphql::Error::new(
            "The cursor's email is no longer in this result set — it was deleted, \
             moved, or no longer matches the filter. Restart pagination without \
             `after`/`before`.",
        );
    }
    e.into()
}

/// Resolve every mailbox name mentioned in a filter tree to its ID, in one go.
async fn resolve_mailbox_names(
    ctx: &Context<'_>,
    filter: Option<&EmailFilter>,
) -> Result<HashMap<String, String>> {
    let mut names = HashSet::new();
    if let Some(f) = filter {
        f.mailbox_names(&mut names);
    }
    if names.is_empty() {
        return Ok(HashMap::new());
    }

    let client = ctx.data::<SharedClient>()?;
    let mut client = client.lock().await;
    // One `Mailbox/get` (cached on the client) covers every name in the tree.
    let mailboxes = client.list_mailboxes().await?;

    let mut resolved = HashMap::new();
    for name in names {
        let lower = name.to_lowercase();
        let found = mailboxes
            .iter()
            .find(|m| m.name.to_lowercase() == lower)
            .or_else(|| {
                mailboxes
                    .iter()
                    .find(|m| m.role.as_deref().map(str::to_lowercase) == Some(lower.clone()))
            });
        match found {
            Some(m) => {
                resolved.insert(name, m.id.clone());
            }
            None => {
                return Err(async_graphql::Error::new(format!(
                    "Unknown mailbox: {name}"
                )));
            }
        }
    }
    Ok(resolved)
}

/// Run one page of an email query and build the connection.
///
/// `constraint` is an extra JMAP filter the resolver imposes regardless of what
/// the caller asked for — `Mailbox.emails` uses it to pin the mailbox.
pub async fn emails_connection(
    ctx: &Context<'_>,
    args: PageArgs,
    filter: Option<EmailFilter>,
    sort: Option<Vec<EmailSort>>,
    constraint: Option<Value>,
    collapse_threads: bool,
) -> Result<EmailConnection> {
    if args.first.is_some() && args.last.is_some() {
        return Err(async_graphql::Error::new(
            "Pass `first` or `last`, not both.",
        ));
    }
    if args.after.is_some() && args.before.is_some() {
        return Err(async_graphql::Error::new(
            "Pass `after` or `before`, not both.",
        ));
    }

    let mailboxes = resolve_mailbox_names(ctx, filter.as_ref()).await?;
    let jmap_filter = filter.as_ref().and_then(|f| f.to_jmap(&mailboxes));
    let jmap_filter = match constraint {
        Some(c) => Some(and_also(jmap_filter, c)),
        None => jmap_filter,
    };
    let jmap_sort = sort_to_jmap(sort.as_deref()).map_err(async_graphql::Error::new)?;

    // Selecting nothing but `totalCount` should not fetch a single email; and
    // `totalCount` is the only thing that makes the server compute the total.
    let wants_total = ctx.look_ahead().field("totalCount").exists();
    let wants_nodes =
        ctx.look_ahead().field("edges").exists() || ctx.look_ahead().field("nodes").exists();

    query(
        args.after,
        args.before,
        args.first,
        args.last,
        |after, before, first, last| async move {
            let after: Option<String> = after;
            let before: Option<String> = before;
            let limit = clamp_page(first.or(last).map(|n| n as u32));

            // Every combination maps onto one JMAP window. `last` without a
            // `before` is the only one that needs the negative-position form.
            let start = match (&after, &before, last) {
                (Some(id), _, _) => QueryStart::Anchor {
                    id: id.clone(),
                    offset: 1,
                },
                (_, Some(id), _) => QueryStart::Anchor {
                    id: id.clone(),
                    offset: -(limit as i64),
                },
                (_, _, Some(_)) => QueryStart::Position(-(limit as i64)),
                _ => QueryStart::Position(0),
            };

            let client = ctx.data::<SharedClient>()?;
            let client = client.lock().await;

            let page = client
                .query_emails(EmailQuery {
                    filter: jmap_filter.clone().unwrap_or_else(|| serde_json::json!({})),
                    sort: jmap_sort.clone(),
                    start,
                    limit,
                    calculate_total: wants_total,
                    collapse_threads,
                    fetch_summaries: wants_nodes,
                })
                .await
                .map_err(stale_anchor_hint)?;

            let first_index = page.position as usize;
            let count = page.ids.len();

            let has_previous = first_index > 0;
            let has_next = match page.total {
                Some(total) => (first_index + count) < total as usize,
                // Without a total, a full page is the signal there may be more.
                None => count as u32 == limit && limit > 0,
            };

            let mut conn = Connection::with_additional_fields(
                has_previous,
                has_next,
                EmailConnectionFields {
                    total_count: page.total,
                    position: page.position,
                    query_state: page.query_state,
                },
            );
            conn.edges = page
                .emails
                .into_iter()
                .map(|email| {
                    Edge::with_additional_fields(
                        email.id.clone(),
                        GqlEmail::summary(email),
                        EmptyFields,
                    )
                })
                .collect();

            Ok::<_, async_graphql::Error>(conn)
        },
    )
    .await
}

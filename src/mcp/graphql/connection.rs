//! Relay-style connections over JMAP's offset-based `Email/query`.
//!
//! JMAP paginates by position, so cursors here are the plain zero-based index
//! of an item within the result set. That keeps them legible — `after: "24"` is
//! obvious to a human or an LLM composing a follow-up query — at the cost of the
//! opacity the Relay spec suggests. Since the client is a language model rather
//! than a Relay runtime, legibility wins.

use std::collections::{HashMap, HashSet};

use async_graphql::connection::{Connection, Edge, EmptyFields, query};
use async_graphql::{Context, Result, SimpleObject};
use serde_json::Value;

use super::SharedClient;
use super::filter::{EmailFilter, EmailSort, and_also, sort_to_jmap};
use super::types::{GqlEmail, clamp_page};
use crate::jmap::EmailQuery;

/// Connection-level fields beyond the Relay defaults.
#[derive(SimpleObject)]
pub struct EmailConnectionFields {
    /// Total number of emails matching the filter, ignoring pagination.
    ///
    /// Computing this costs the server extra work, so it is only requested when
    /// you actually select the field — a query that omits it pays nothing.
    pub total_count: Option<u64>,
}

pub type EmailConnection = Connection<usize, GqlEmail, EmailConnectionFields>;

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
    let mailboxes = resolve_mailbox_names(ctx, filter.as_ref()).await?;
    let jmap_filter = filter.as_ref().and_then(|f| f.to_jmap(&mailboxes));
    let jmap_filter = match constraint {
        Some(c) => Some(and_also(jmap_filter, c)),
        None => jmap_filter,
    };
    let jmap_sort = sort_to_jmap(sort.as_deref());

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
            let after: Option<usize> = after;
            let before: Option<usize> = before;

            // Backward pagination needs to know where the end is.
            let backward = last.is_some();
            let calculate_total = wants_total || (backward && before.is_none());

            let limit = clamp_page(first.or(last).map(|n| n as u32));

            // Forward: start just past `after`. Backward: end just before
            // `before` and walk back `limit` items.
            let start = after.map(|a| a + 1).unwrap_or(0);

            let client = ctx.data::<SharedClient>()?;
            let client = client.lock().await;

            let run = |position: u64, limit: u32, fetch: bool, total: bool| {
                client.query_emails(EmailQuery {
                    filter: jmap_filter.clone().unwrap_or_else(|| serde_json::json!({})),
                    sort: jmap_sort.clone(),
                    position,
                    limit,
                    calculate_total: total,
                    collapse_threads,
                    fetch_summaries: fetch,
                })
            };

            let page = if backward {
                // Establish the end of the window, then step back from it.
                let end = match before {
                    Some(b) => b as u64,
                    None => run(0, 0, false, true).await?.total.unwrap_or(0),
                };
                let start = end.saturating_sub(limit as u64);
                let take = (end - start) as u32;
                run(start, take, wants_nodes, wants_total).await?
            } else {
                run(start as u64, limit, wants_nodes, calculate_total).await?
            };

            let first_index = page.position as usize;
            let count = if wants_nodes {
                page.emails.len()
            } else {
                page.ids.len()
            };

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
                },
            );
            conn.edges = page
                .emails
                .into_iter()
                .enumerate()
                .map(|(i, email)| {
                    Edge::with_additional_fields(
                        first_index + i,
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

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

use async_graphql::OutputType;
use async_graphql::connection::{Connection, Edge, EmptyFields, query};
use async_graphql::{Context, Result, SimpleObject};
use serde_json::Value;

use super::SharedClient;
use super::filter::{EmailFilter, EmailSort, and_also, sort_to_jmap};
use super::types::{GqlEmail, all_mailboxes, clamp_page, find_by_name_or_role};
use crate::jmap::{EmailQuery, QueryStart};

/// Connection-level fields beyond the Relay defaults.
#[derive(SimpleObject)]
pub struct EmailConnectionFields {
    /// Total number of emails matching, ignoring pagination.
    ///
    /// On a query-backed list this costs the server extra work, so it is only
    /// requested when you select it — omit it and you pay nothing. On
    /// `Thread.emails` it is free and always present, being a length.
    pub total_count: Option<u64>,
    /// Zero-based index of the first returned email within the whole result set.
    pub position: u64,
    /// Opaque server state for this query. Two pages sharing a `queryState`
    /// came from the same result set; a change means messages arrived, moved,
    /// or were deleted in between.
    ///
    /// **Null on `Thread.emails`** — a conversation is not a query, so there is
    /// no query state to report. Compare `totalCount` across pages instead.
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

/// The extra field on a connection built from a list we already hold.
#[derive(SimpleObject)]
pub struct CountFields {
    /// How many items match, before paging.
    ///
    /// Free to ask for: this list arrived whole from a single call, so the
    /// count is a length rather than extra work for the server.
    pub total_count: u64,
}

/// A connection over a list that is already in memory. The GraphQL type name
/// is derived from the node — `Mailbox` gives `MailboxConnection`, and so on.
pub type ListConnection<T> = Connection<String, T, CountFields>;

/// Work out which slice of an in-memory list a page refers to.
///
/// Everything except `emails` arrives whole from one API call — JMAP has no
/// windowed form for mailboxes, identities, a thread's messages or an email's
/// attachments. So paging here is slicing, and the honest consequence is that a
/// cursor is only as stable as the list: `after` names an item by id, and if
/// that item is gone next time you get a "restart pagination" error rather than
/// a quietly different page.
///
/// Returns the cursors paired with their items, plus where the window sits.
pub struct Page<T> {
    pub items: Vec<(String, T)>,
    pub start: usize,
    pub total: usize,
    pub has_previous: bool,
    pub has_next: bool,
}

pub fn page_of<T>(
    items: Vec<T>,
    args: PageArgs,
    cursor_of: impl Fn(&T) -> String,
) -> Result<Page<T>> {
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

    let cursors: Vec<String> = items.iter().map(&cursor_of).collect();
    let total = items.len();

    let locate = |cursor: &str| {
        cursors.iter().position(|c| c == cursor).ok_or_else(|| {
            async_graphql::Error::new(format!(
                "Cursor {cursor:?} is no longer in this list — it was removed since the \
                 cursor was issued. Restart pagination without `after`/`before`."
            ))
        })
    };

    let mut start = match &args.after {
        Some(cursor) => locate(cursor)? + 1,
        None => 0,
    };
    let mut end = match &args.before {
        Some(cursor) => locate(cursor)?,
        None => total,
    };
    end = end.max(start);

    // Same page-size rules as the email connection, so `first`/`last` behave the
    // same wherever they appear.
    match (args.first, args.last) {
        (Some(first), _) => {
            end = end.min(start + clamp_page(Some(first.max(0) as u32)) as usize);
        }
        (_, Some(last)) => {
            start = end.saturating_sub(clamp_page(Some(last.max(0) as u32)) as usize);
        }
        _ => end = end.min(start + clamp_page(None) as usize),
    }

    Ok(Page {
        items: items
            .into_iter()
            .zip(cursors)
            .skip(start)
            .take(end - start)
            .map(|(node, cursor)| (cursor, node))
            .collect(),
        start,
        total,
        has_previous: start > 0,
        has_next: end < total,
    })
}

/// Paginate a list we already hold. See [`page_of`] for what cursors mean here.
pub fn paginate<T: OutputType>(
    items: Vec<T>,
    args: PageArgs,
    cursor_of: impl Fn(&T) -> String,
) -> Result<ListConnection<T>> {
    let page = page_of(items, args, cursor_of)?;
    let mut connection = Connection::with_additional_fields(
        page.has_previous,
        page.has_next,
        CountFields {
            total_count: page.total as u64,
        },
    );
    connection.edges.extend(
        page.items
            .into_iter()
            .map(|(cursor, node)| Edge::new(cursor, node)),
    );
    Ok(connection)
}

/// Paginate a thread's messages into the same `EmailConnection` every other
/// email list returns.
///
/// A thread is not an `Email/query`, so `queryState` is null — there is no
/// query to have a state. `position` and `totalCount` are both exact and free:
/// the whole thread is already in hand.
pub fn thread_connection(emails: Vec<GqlEmail>, args: PageArgs) -> Result<EmailConnection> {
    let page = page_of(emails, args, |e| e.email_id().to_string())?;
    let mut connection = Connection::with_additional_fields(
        page.has_previous,
        page.has_next,
        EmailConnectionFields {
            total_count: Some(page.total as u64),
            position: page.start as u64,
            query_state: None,
        },
    );
    connection.edges.extend(
        page.items
            .into_iter()
            .map(|(cursor, node)| Edge::new(cursor, node)),
    );
    Ok(connection)
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

    // Through the same loader as every other mailbox lookup, so a filter naming
    // a folder costs nothing extra when the query also walks the folder tree.
    let mailboxes = all_mailboxes(ctx).await?;

    let mut resolved = HashMap::new();
    for name in names {
        match find_by_name_or_role(&mailboxes, &name) {
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

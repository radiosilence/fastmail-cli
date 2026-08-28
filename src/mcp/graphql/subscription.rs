//! GraphQL subscription resolvers.
//!
//! One subscription: mail as it arrives. It runs on the same
//! [`ArrivalWatcher`](crate::jmap::ArrivalWatcher) as `fastmail watch`, so the
//! cursor semantics are identical — push is a wake-up, `Email/changes` is the
//! answer, and a dropped connection costs latency rather than mail.

use async_graphql::futures_util::stream::{self, Stream, StreamExt};
use async_graphql::{Context, Result, Subscription};
use std::time::Duration;

use super::SharedClient;
use super::loaders::to_gql_error;
use super::types::GqlEmail;
use crate::jmap::ArrivalWatcher;
use std::sync::Arc;

pub struct SubscriptionRoot;

/// Whether the watcher is still worth polling. A fatal error yields once and
/// then closes the stream — there is nothing to retry with a dead credential.
enum Watching {
    Live(Box<ArrivalWatcher>),
    Done,
}

#[Subscription]
impl SubscriptionRoot {
    /// Emits each email as it arrives, indefinitely.
    ///
    /// Backed by JMAP's push channel, with the state cursor held server-side
    /// here rather than by the subscriber: a reconnect or a missed
    /// notification is reconciled through `Email/changes`, so the subscription
    /// reports late rather than losing mail. Only *new* messages are emitted —
    /// flag and folder changes to existing mail are not arrivals.
    ///
    /// Transient failures are retried internally and never reach the
    /// subscriber. The stream ends only when the token stops authenticating.
    ///
    /// Set `full` to select body and attachment fields. Unlike a query, a
    /// subscription has no request boundary at which the batching loaders
    /// reset, so leaving it off and selecting `textBody` anyway resolves each
    /// email through a loader that lives as long as the subscription.
    async fn emails(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Only emit mail landing in this mailbox, by name or role.")]
        mailbox: Option<String>,
        #[graphql(
            default = false,
            desc = "Fetch bodies and attachment metadata with each arrival."
        )]
        full: bool,
        #[graphql(
            desc = "Check on this interval instead of holding a push connection open. For \
                    networks that will not keep one alive; the results are the same."
        )]
        poll_seconds: Option<u64>,
    ) -> Result<impl Stream<Item = Result<GqlEmail>> + use<>> {
        let client = ctx.data::<SharedClient>()?.clone();

        let watcher = ArrivalWatcher::new(
            client,
            mailbox.as_deref(),
            full,
            poll_seconds.map(Duration::from_secs),
        )
        .await
        .map_err(|e| to_gql_error(Arc::new(e)))?;

        let batches = stream::unfold(Watching::Live(Box::new(watcher)), |state| async move {
            let Watching::Live(mut watcher) = state else {
                return None;
            };
            loop {
                match watcher.next_arrivals().await {
                    // A wake-up that turned up nothing is normal — keep
                    // waiting rather than emitting an empty tick.
                    Ok(arrivals) if arrivals.emails.is_empty() => continue,
                    Ok(arrivals) => {
                        return Some((Ok(arrivals.emails), Watching::Live(watcher)));
                    }
                    Err(e) => return Some((Err(to_gql_error(Arc::new(e))), Watching::Done)),
                }
            }
        });

        Ok(batches.flat_map(move |batch| match batch {
            Ok(emails) => stream::iter(
                emails
                    .into_iter()
                    .map(|email| {
                        Ok(if full {
                            GqlEmail::full(email)
                        } else {
                            GqlEmail::summary(email)
                        })
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(e) => stream::iter(vec![Err(e)]),
        }))
    }
}

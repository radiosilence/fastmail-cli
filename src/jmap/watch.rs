//! Watching for newly arrived mail.
//!
//! The cursor is the point: JMAP's push channel says only *something changed*,
//! so this holds an `Email` state string and asks `Email/changes` what that
//! actually was. Push, polling and reconnect-after-a-drop all funnel through
//! the same call, which is why a lost notification costs latency rather than
//! mail — and why `--poll` is a timer swapped for a socket, not a second
//! implementation.

use crate::error::{Error, Result};
use crate::jmap::{EventParser, JmapClient};
use crate::models::Email;
use std::time::Duration;
use tracing::debug;

/// A client shared between a watcher and whatever else is using the
/// connection. [`JmapClient`] is safe to use concurrently.
pub type SharedJmapClient = std::sync::Arc<JmapClient>;

/// Shortest poll interval accepted. Each tick is an `Email/changes` call, and a
/// zero interval would issue them back to back until rate-limited.
pub const MIN_POLL: Duration = Duration::from_secs(10);

/// Longest wait after repeated failures to read changes, so a struggling
/// server is not asked again at full rate.
const FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Attempts at one page before it is given up on. Without a limit, a page that
/// can never be fetched, such as one too large to arrive within the request
/// timeout, would hold the cursor still and stop every later arrival.
const MAX_PAGE_ATTEMPTS: u32 = 5;

/// Emails per `Email/get` when fetching bodies, which can be megabytes each.
const FULL_FETCH_CHUNK: usize = 10;

/// Reconnect backoff bounds. The ceiling is deliberately small: a watcher that
/// goes quiet for minutes after a blip is indistinguishable from a broken one.
const BACKOFF_START: u64 = 1;
const BACKOFF_MAX: u64 = 30;

/// What one wake-up turned up.
pub struct Arrivals {
    /// New emails, oldest first. Empty is normal — a wake-up is a prompt to
    /// look, not a promise of mail.
    pub emails: Vec<Email>,
    /// The server had discarded change history past our cursor, so it was reset
    /// to the present. Anything that arrived in the gap was never reported and
    /// now never will be.
    pub resynced: bool,
}

/// How the watcher learns it should look again.
enum Wake {
    Push {
        /// The live response, or `None` when it needs (re)opening.
        stream: Option<reqwest::Response>,
        parser: EventParser,
        backoff: u64,
        last_event_id: Option<String>,
    },
    Poll(Duration),
}

pub struct ArrivalWatcher {
    client: SharedJmapClient,
    state: String,
    mailbox_id: Option<String>,
    full: bool,
    wake: Wake,
    /// The last read left changes unread past `state`, so read again without
    /// waiting for a wake-up.
    more: bool,
    /// Consecutive failed reads, which set the backoff before the next one.
    failures: u32,
}

impl ArrivalWatcher {
    /// Start watching from the present moment.
    ///
    /// `poll` swaps the push connection for a timer of that interval, no
    /// shorter than [`MIN_POLL`]; without it the watcher holds JMAP's event
    /// source open.
    pub async fn new(
        client: SharedJmapClient,
        mailbox: Option<&str>,
        full: bool,
        poll: Option<Duration>,
    ) -> Result<Self> {
        let mailbox_id = match mailbox {
            Some(name) => Some(client.find_mailbox(name).await?.id),
            None => None,
        };
        // Start from now: the caller asked what arrives next, not what is
        // already sitting there.
        let state = client.email_state().await?;

        Ok(Self {
            client,
            state,
            mailbox_id,
            full,
            more: false,
            failures: 0,
            wake: match poll {
                Some(interval) => Wake::Poll(interval.max(MIN_POLL)),
                None => Wake::Push {
                    stream: None,
                    parser: EventParser::default(),
                    backoff: BACKOFF_START,
                    last_event_id: None,
                },
            },
        })
    }

    /// Block until the next wake-up, then report what arrived.
    ///
    /// Errors are returned only when the watcher can never succeed again — a
    /// dead credential. Everything else is transient by nature (a dropped
    /// connection, a bad response, a server that has forgotten our cursor) and
    /// is retried internally with backoff, because a watcher that exits on one
    /// blip is useless in the loop it exists to feed.
    pub async fn next_arrivals(&mut self) -> Result<Arrivals> {
        if self.failures > 0 {
            // An unread push connection is worse than none: reqwest's read
            // timeout keeps running while nobody polls it, so the first read
            // after a long pause fails. Reopen it afterwards instead.
            if let Wake::Push { stream, .. } = &mut self.wake {
                *stream = None;
            }
            // Retry the read after a pause; the cursor has not moved, so
            // nothing is lost by not waiting for a wake-up first.
            let exponent = (self.failures - 1).min(16);
            let delay = Duration::from_secs(1 << exponent).min(FAILURE_BACKOFF_MAX);
            tokio::time::sleep(delay).await;
        } else if !self.more {
            self.wait().await?;
        }
        self.drain().await
    }

    /// Wait until there is reason to believe something changed.
    async fn wait(&mut self) -> Result<()> {
        let interval = match &mut self.wake {
            Wake::Poll(interval) => *interval,
            Wake::Push { .. } => return self.wait_for_push().await,
        };
        tokio::time::sleep(interval).await;
        Ok(())
    }

    async fn wait_for_push(&mut self) -> Result<()> {
        loop {
            let Wake::Push {
                stream,
                parser,
                backoff,
                last_event_id,
            } = &mut self.wake
            else {
                unreachable!("wait_for_push is only entered for Wake::Push")
            };

            if stream.is_none() {
                // A frame half-read from a dead connection must not prefix the
                // first one from the new connection.
                *parser = EventParser::default();
                let opened = self
                    .client
                    .open_event_stream(last_event_id.as_deref())
                    .await;
                match opened {
                    Ok(resp) => *stream = Some(resp),
                    Err(e) if is_fatal(&e) => return Err(e),
                    Err(e) => {
                        debug!("Could not open event stream ({e}); retrying");
                        sleep_backoff(backoff).await;
                        // Reconcile anyway: while push is down, each retry is
                        // also a poll tick, so mail still surfaces.
                        return Ok(());
                    }
                }
            }

            let chunk = stream
                .as_mut()
                .expect("stream was just opened")
                .chunk()
                .await;

            match chunk {
                Ok(Some(bytes)) => {
                    // Reset only once the connection has carried something.
                    // Resetting on connect alone would let a server that
                    // accepts and immediately hangs up spin at full rate.
                    *backoff = BACKOFF_START;

                    let mut changed = false;
                    let Ok(events) = parser.feed(&bytes) else {
                        debug!("Event stream sent no frame boundary; reconnecting");
                        *stream = None;
                        sleep_backoff(backoff).await;
                        return Ok(());
                    };
                    for event in events {
                        if let Some(id) = event.id {
                            *last_event_id = Some(id);
                        }
                        // Keep-alives carry no state change.
                        if event.event.as_deref() == Some("ping") || event.data.is_empty() {
                            continue;
                        }
                        changed = true;
                    }
                    if changed {
                        return Ok(());
                    }
                }
                Ok(None) | Err(_) => {
                    debug!("Event stream ended; reconnecting");
                    *stream = None;
                    sleep_backoff(backoff).await;
                    // Reconcile across the gap before waiting again, so mail
                    // that landed while disconnected is reported now rather
                    // than whenever the next message happens to arrive.
                    return Ok(());
                }
            }
        }
    }

    /// Read one page of changes and collect whatever it turned up.
    ///
    /// The cursor moves only once the page's mail has been fetched: a failure
    /// either side leaves it where it was, so the same page is read again
    /// after a backoff and nothing in it is skipped.
    async fn drain(&mut self) -> Result<Arrivals> {
        let client = self.client.clone();

        let changes = match client.email_changes(&self.state).await {
            Ok(changes) => changes,
            Err(Error::Jmap { ref error_type, .. }) if error_type == "cannotCalculateChanges" => {
                // History is gone back to our cursor. There is no way to know
                // what was missed, and replaying the mailbox as "new" would be
                // a lie, so reset to the present and say so.
                let state = match client.email_state().await {
                    Ok(state) => state,
                    Err(e) if is_fatal(&e) => return Err(e),
                    Err(e) => {
                        debug!("Could not resync ({e})");
                        self.failures += 1;
                        return Ok(Arrivals::none());
                    }
                };
                self.advance(state, false);
                return Ok(Arrivals {
                    emails: Vec::new(),
                    resynced: true,
                });
            }
            Err(e) if is_fatal(&e) => return Err(e),
            Err(e) => {
                debug!("Could not read changes ({e})");
                self.failures += 1;
                return Ok(Arrivals::none());
            }
        };

        // A server that claims more but has not moved the state would be read
        // forever without a pause.
        let more = changes.has_more && changes.new_state != self.state;
        if changes.created.is_empty() {
            self.advance(changes.new_state, more);
            return Ok(Arrivals::none());
        }

        let mut emails = Vec::with_capacity(changes.created.len());
        let chunk = if self.full {
            FULL_FETCH_CHUNK
        } else {
            changes.created.len()
        };
        for ids in changes.created.chunks(chunk) {
            let fetched = if self.full {
                client.get_emails(ids).await
            } else {
                client.get_email_summaries(ids).await
            };
            match fetched {
                Ok(batch) => emails.extend(batch),
                Err(e) if is_fatal(&e) => return Err(e),
                Err(e) if self.failures + 1 >= MAX_PAGE_ATTEMPTS => {
                    tracing::warn!(
                        "Giving up on {} new email(s) after {MAX_PAGE_ATTEMPTS} attempts ({e})",
                        changes.created.len()
                    );
                    self.advance(changes.new_state, more);
                    return Ok(Arrivals::none());
                }
                Err(e) => {
                    debug!(
                        "Could not fetch {} new email(s) ({e})",
                        changes.created.len()
                    );
                    self.failures += 1;
                    return Ok(Arrivals::none());
                }
            }
        }
        self.advance(changes.new_state, more);

        emails.retain(|email| in_mailbox(email, self.mailbox_id.as_deref()));
        // `Email/get` makes no ordering guarantee, and a stream reads
        // chronologically.
        emails.sort_by(|a, b| a.received_at.cmp(&b.received_at));

        Ok(Arrivals {
            emails,
            resynced: false,
        })
    }
}

impl ArrivalWatcher {
    fn advance(&mut self, state: String, more: bool) {
        self.state = state;
        self.more = more;
        self.failures = 0;
    }
}

impl Arrivals {
    fn none() -> Self {
        Self {
            emails: Vec::new(),
            resynced: false,
        }
    }
}

async fn sleep_backoff(backoff: &mut u64) {
    tokio::time::sleep(Duration::from_secs(*backoff)).await;
    *backoff = (*backoff * 2).min(BACKOFF_MAX);
}

fn in_mailbox(email: &Email, mailbox_id: Option<&str>) -> bool {
    mailbox_id.is_none_or(|id| email.mailbox_ids.contains_key(id))
}

/// Whether an error means the watcher can never succeed again.
fn is_fatal(e: &Error) -> bool {
    matches!(e, Error::InvalidToken(_) | Error::NotAuthenticated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email_in(mailbox_ids: &[&str]) -> Email {
        let ids: serde_json::Map<_, _> = mailbox_ids
            .iter()
            .map(|id| (id.to_string(), serde_json::json!(true)))
            .collect();
        serde_json::from_value(serde_json::json!({ "id": "e1", "mailboxIds": ids })).unwrap()
    }

    #[test]
    fn no_mailbox_filter_accepts_everything() {
        assert!(in_mailbox(&email_in(&["archive"]), None));
    }

    #[test]
    fn mailbox_filter_matches_membership() {
        let email = email_in(&["inbox", "important"]);
        assert!(in_mailbox(&email, Some("inbox")));
        assert!(!in_mailbox(&email, Some("trash")));
    }

    #[test]
    fn a_dead_credential_is_fatal_but_a_bad_response_is_not() {
        assert!(is_fatal(&Error::NotAuthenticated));
        assert!(is_fatal(&Error::InvalidToken("nope")));
        assert!(!is_fatal(&Error::RateLimited));
        assert!(!is_fatal(&Error::Server("boom".into())));
    }

    // Paused clock: the sleeps are the point of the function, but waiting out
    // half a minute of them is not.
    #[tokio::test(start_paused = true)]
    async fn backoff_doubles_up_to_the_ceiling() {
        let mut backoff = BACKOFF_START;
        sleep_backoff(&mut backoff).await;
        assert_eq!(backoff, BACKOFF_START * 2);

        let mut backoff = BACKOFF_MAX - 1;
        sleep_backoff(&mut backoff).await;
        assert_eq!(backoff, BACKOFF_MAX);
        sleep_backoff(&mut backoff).await;
        assert_eq!(
            backoff, BACKOFF_MAX,
            "backoff must not grow past the ceiling"
        );
    }
}

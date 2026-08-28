use crate::error::Error;
use crate::jmap::{EventParser, JmapClient, authenticated_client};
use crate::models::{Email, Output};
use std::time::Duration;
use tracing::debug;

/// How often the server should send a keep-alive on the push channel. Also sets
/// the read timeout that detects a connection which died without saying so.
const PING_SECONDS: u32 = 30;

/// Reconnect backoff bounds. The ceiling is deliberately below the ping
/// interval's own scale: a watcher that goes quiet for minutes after a blip is
/// indistinguishable from a broken one.
const BACKOFF_START: u64 = 1;
const BACKOFF_MAX: u64 = 30;

pub struct WatchOptions {
    /// Only report mail landing in this mailbox, by name or role.
    pub mailbox: Option<String>,
    /// Fetch bodies and attachment metadata rather than summaries.
    pub full: bool,
    /// Check every N seconds instead of holding a push connection open.
    pub poll: Option<u64>,
}

/// Stream newly arrived emails as newline-delimited JSON, one object per line,
/// until interrupted.
///
/// Arrivals are discovered through `Email/changes` against a state cursor this
/// function owns. The push channel only ever says *look again* — so a dropped
/// connection, a missed event or a `--poll` fallback all converge on the same
/// answer, and the cost of losing a notification is latency rather than mail.
pub async fn watch(opts: WatchOptions) -> anyhow::Result<()> {
    let mut client = authenticated_client().await?;

    let mailbox_id = match opts.mailbox {
        Some(ref name) => Some(client.find_mailbox(name).await?.id),
        None => None,
    };

    // Start from now: the caller asked what arrives next, not what is already
    // sitting there.
    let mut state = client.email_state().await?;

    match opts.poll {
        Some(seconds) => {
            poll_loop(
                &client,
                &mut state,
                mailbox_id.as_deref(),
                opts.full,
                seconds,
            )
            .await
        }
        None => push_loop(&client, &mut state, mailbox_id.as_deref(), opts.full).await,
    }
}

async fn push_loop(
    client: &JmapClient,
    state: &mut String,
    mailbox_id: Option<&str>,
    full: bool,
) -> anyhow::Result<()> {
    let mut backoff = BACKOFF_START;
    let mut last_event_id: Option<String> = None;

    loop {
        match client
            .open_event_stream(PING_SECONDS, last_event_id.as_deref())
            .await
        {
            Ok(mut resp) => {
                let mut parser = EventParser::default();
                loop {
                    match resp.chunk().await {
                        Ok(Some(bytes)) => {
                            // Reset only once the connection has actually
                            // carried something. Resetting on connect alone
                            // would let a server that accepts and immediately
                            // hangs up spin at full rate.
                            backoff = BACKOFF_START;

                            for event in parser.feed(&String::from_utf8_lossy(&bytes)) {
                                if let Some(id) = event.id {
                                    last_event_id = Some(id);
                                }
                                // Keep-alives carry no state change.
                                if event.event.as_deref() == Some("ping") || event.data.is_empty() {
                                    continue;
                                }
                                drain(client, state, mailbox_id, full).await?;
                            }
                        }
                        Ok(None) => {
                            debug!("Event source closed by server");
                            break;
                        }
                        Err(e) => {
                            eprintln!("watch: event stream dropped ({e}); reconnecting");
                            break;
                        }
                    }
                }
            }
            Err(e) if fatal(&e) => return Err(e.into()),
            Err(e) => eprintln!("watch: could not open event stream ({e}); retrying"),
        }

        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);

        // Reconcile across the gap before waiting on the next push, so mail
        // that landed while disconnected is reported on reconnect rather than
        // on whatever arrives after it.
        drain(client, state, mailbox_id, full).await?;
    }
}

async fn poll_loop(
    client: &JmapClient,
    state: &mut String,
    mailbox_id: Option<&str>,
    full: bool,
    seconds: u64,
) -> anyhow::Result<()> {
    loop {
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        drain(client, state, mailbox_id, full).await?;
    }
}

/// Advance the cursor and print whatever arrived, oldest first.
///
/// Transient failures are reported and swallowed: a watcher that exits on one
/// bad response is worse than useless in the loop it is meant to feed. A dead
/// credential is not transient, so it still ends the process.
async fn drain(
    client: &JmapClient,
    state: &mut String,
    mailbox_id: Option<&str>,
    full: bool,
) -> anyhow::Result<()> {
    let changes = match client.email_changes(state).await {
        Ok(changes) => changes,
        Err(Error::Jmap { ref error_type, .. }) if error_type == "cannotCalculateChanges" => {
            // The server has discarded history back to our cursor. There is no
            // way to know what was missed, and replaying the mailbox as "new"
            // would be a lie, so resync to now and say so.
            *state = client.email_state().await?;
            eprintln!(
                "watch: server dropped change history; resynced, some arrivals may be missing"
            );
            return Ok(());
        }
        Err(e) if fatal(&e) => return Err(e.into()),
        Err(e) => {
            eprintln!("watch: could not read changes ({e})");
            return Ok(());
        }
    };

    *state = changes.new_state;
    if changes.created.is_empty() {
        return Ok(());
    }

    let fetched = if full {
        client.get_emails(&changes.created).await
    } else {
        client.get_email_summaries(&changes.created).await
    };

    let mut emails = match fetched {
        Ok(emails) => emails,
        Err(e) if fatal(&e) => return Err(e.into()),
        Err(e) => {
            eprintln!(
                "watch: could not fetch {} new email(s) ({e})",
                changes.created.len()
            );
            return Ok(());
        }
    };

    // `Email/get` makes no ordering guarantee, and a stream reads chronologically.
    emails.sort_by(|a, b| a.received_at.cmp(&b.received_at));

    for email in emails.iter().filter(|e| in_mailbox(e, mailbox_id)) {
        Output::success(email).print_compact();
    }

    Ok(())
}

fn in_mailbox(email: &Email, mailbox_id: Option<&str>) -> bool {
    mailbox_id.is_none_or(|id| email.mailbox_ids.contains_key(id))
}

/// Whether an error means the watcher can never succeed again.
fn fatal(e: &Error) -> bool {
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
        assert!(fatal(&Error::NotAuthenticated));
        assert!(fatal(&Error::InvalidToken("nope")));
        assert!(!fatal(&Error::RateLimited));
        assert!(!fatal(&Error::Server("boom".into())));
    }
}

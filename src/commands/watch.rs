use crate::jmap::{ArrivalWatcher, authenticated_client};
use crate::models::Output;
use std::time::Duration;

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
pub async fn watch(opts: WatchOptions) -> anyhow::Result<()> {
    let client = std::sync::Arc::new(tokio::sync::Mutex::new(authenticated_client().await?));

    let mut watcher = ArrivalWatcher::new(
        client,
        opts.mailbox.as_deref(),
        opts.full,
        opts.poll.map(Duration::from_secs),
    )
    .await?;

    loop {
        let arrivals = watcher.next_arrivals().await?;

        // Mail may have been lost, and stdout is reserved for mail that wasn't.
        if arrivals.resynced {
            eprintln!(
                "watch: server dropped change history; resynced, some arrivals may be missing"
            );
        }

        for email in &arrivals.emails {
            Output::success(email).print_compact();
        }
    }
}

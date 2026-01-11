use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;

/// Search filter matching JMAP Email/query FilterCondition
#[derive(Debug, Default)]
pub struct SearchFilter {
    pub text: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub mailbox: Option<String>,
    pub has_attachment: bool,
    pub min_size: Option<u32>,
    pub max_size: Option<u32>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub unread: bool,
    pub flagged: bool,
}

pub async fn search(filter: SearchFilter, limit: u32) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    // Resolve mailbox name to ID if specified
    let mailbox_id = if let Some(ref mailbox_name) = filter.mailbox {
        Some(client.find_mailbox(mailbox_name).await?.id)
    } else {
        None
    };

    let emails = client
        .search_emails_filtered(&filter, mailbox_id.as_deref(), limit)
        .await?;
    Output::success(emails).print();

    Ok(())
}

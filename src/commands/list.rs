use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::{Email, Mailbox, Output};

pub async fn list_mailboxes() -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let mailboxes = client.list_mailboxes().await?;
    Output::success(mailboxes).print();

    Ok(())
}

pub async fn list_emails(mailbox: &str, limit: u32) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let mailbox = client.find_mailbox(mailbox).await?;
    let emails = client.list_emails(&mailbox.id, limit).await?;

    #[derive(serde::Serialize)]
    struct EmailListResponse {
        mailbox: Mailbox,
        emails: Vec<Email>,
    }

    Output::success(EmailListResponse { mailbox, emails }).print();

    Ok(())
}

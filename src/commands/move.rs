use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;

pub async fn move_email(email_id: &str, mailbox: &str) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let mailbox = client.find_mailbox(mailbox).await?;
    client.move_email(email_id, &mailbox.id).await?;

    Output::<()>::success_msg(format!("Moved email to {}", mailbox.name)).print();

    Ok(())
}

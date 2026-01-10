use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;

pub async fn mark_spam(email_id: &str) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    client.mark_spam(email_id).await?;

    Output::<()>::success_msg("Email marked as spam").print();

    Ok(())
}

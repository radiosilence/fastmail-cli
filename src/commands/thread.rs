use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;

pub async fn get_thread(email_id: &str) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let emails = client.get_thread(email_id).await?;
    Output::success(emails).print();

    Ok(())
}

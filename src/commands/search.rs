use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;

pub async fn search(query: &str, limit: u32) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let emails = client.search_emails(query, limit).await?;
    Output::success(emails).print();

    Ok(())
}

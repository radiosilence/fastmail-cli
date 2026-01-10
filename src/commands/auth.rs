use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;

pub async fn auth(token: &str) -> anyhow::Result<()> {
    let mut client = JmapClient::new(token.to_string());
    let session = client.authenticate().await?;

    let mut config = Config::load()?;
    config.set_token(token.to_string());
    config.save()?;

    Output::<()>::success_msg(format!("Authenticated as {}", session.username)).print();

    Ok(())
}

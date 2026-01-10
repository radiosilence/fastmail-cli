use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;
use crate::util::parse_addresses;

pub async fn reply(
    email_id: &str,
    body: &str,
    reply_all: bool,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let original = client.get_email(email_id).await?;

    let cc_addrs = cc.map(parse_addresses).unwrap_or_default();
    let bcc_addrs = bcc.map(parse_addresses).unwrap_or_default();

    let new_email_id = client
        .reply_email(&original, body, reply_all, cc_addrs, bcc_addrs)
        .await?;

    #[derive(serde::Serialize)]
    struct ReplyResponse {
        email_id: String,
        in_reply_to: String,
    }

    Output::success(ReplyResponse {
        email_id: new_email_id,
        in_reply_to: email_id.to_string(),
    })
    .print();

    Ok(())
}

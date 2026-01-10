use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;
use crate::util::parse_addresses;

pub async fn forward(
    email_id: &str,
    to: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let original = client.get_email(email_id).await?;

    let to_addrs = parse_addresses(to);
    let cc_addrs = cc.map(parse_addresses).unwrap_or_default();
    let bcc_addrs = bcc.map(parse_addresses).unwrap_or_default();

    let new_email_id = client
        .forward_email(&original, to_addrs, body, cc_addrs, bcc_addrs)
        .await?;

    #[derive(serde::Serialize)]
    struct ForwardResponse {
        email_id: String,
        forwarded_from: String,
    }

    Output::success(ForwardResponse {
        email_id: new_email_id,
        forwarded_from: email_id.to_string(),
    })
    .print();

    Ok(())
}

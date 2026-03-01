use crate::jmap::authenticated_client;
use crate::models::Output;
use crate::util::parse_addresses;

pub async fn forward(
    email_id: &str,
    to: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
    from: Option<&str>,
    draft: bool,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    let original = client.get_email(email_id).await?;

    let to_addrs = parse_addresses(to);
    let cc_addrs = cc.map(parse_addresses).unwrap_or_default();
    let bcc_addrs = bcc.map(parse_addresses).unwrap_or_default();

    let new_email_id = client
        .forward_email(&original, to_addrs, body, cc_addrs, bcc_addrs, from, draft)
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

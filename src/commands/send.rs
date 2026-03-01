use crate::jmap::authenticated_client;
use crate::models::Output;
use crate::util::parse_addresses;

pub async fn send(
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
    reply_to: Option<&str>,
    from: Option<&str>,
    draft: bool,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    let to_addrs = parse_addresses(to);
    let cc_addrs = cc.map(parse_addresses).unwrap_or_default();
    let bcc_addrs = bcc.map(parse_addresses).unwrap_or_default();

    let email_id = client
        .send_email(
            to_addrs, cc_addrs, bcc_addrs, subject, body, reply_to, from, draft,
        )
        .await?;

    #[derive(serde::Serialize)]
    struct SendResponse {
        email_id: String,
    }

    Output::success(SendResponse { email_id }).print();

    Ok(())
}

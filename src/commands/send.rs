use crate::jmap::{ComposeParams, authenticated_client};
use crate::models::Output;
use crate::util::parse_addresses;

pub async fn send(
    to: &str,
    subject: &str,
    body: &str,
    reply_to: Option<&str>,
    params: ComposeParams<'_>,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    let draft = params.draft;

    let email_id = client
        .send_email(parse_addresses(to), subject, body, reply_to, params)
        .await?;

    #[derive(serde::Serialize)]
    struct SendResponse {
        email_id: String,
        status: &'static str,
    }

    Output::success(SendResponse {
        email_id,
        status: if draft { "draft" } else { "sent" },
    })
    .print();

    Ok(())
}

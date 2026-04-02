use crate::jmap::{ComposeParams, MessageBody, authenticated_client};
use crate::models::Output;
use crate::util::parse_addresses;

pub async fn forward(
    email_id: &str,
    to: &str,
    message_body: MessageBody,
    params: ComposeParams<'_>,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    let draft = params.draft;

    let original = client.get_email(email_id).await?;

    let new_email_id = client
        .forward_email(&original, parse_addresses(to), message_body, params)
        .await?;

    #[derive(serde::Serialize)]
    struct ForwardResponse {
        email_id: String,
        forwarded_from: String,
        status: &'static str,
    }

    Output::success(ForwardResponse {
        email_id: new_email_id,
        forwarded_from: email_id.to_string(),
        status: if draft { "draft" } else { "sent" },
    })
    .print();

    Ok(())
}

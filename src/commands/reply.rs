use crate::jmap::{ComposeParams, MessageBody, authenticated_client};
use crate::models::Output;

pub async fn reply(
    email_id: &str,
    message_body: MessageBody,
    reply_all: bool,
    params: ComposeParams<'_>,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    let draft = params.draft;

    let original = client.get_email(email_id).await?;

    let new_email_id = client
        .reply_email(&original, message_body, reply_all, params)
        .await?;

    #[derive(serde::Serialize)]
    struct ReplyResponse {
        email_id: String,
        in_reply_to: String,
        status: &'static str,
    }

    Output::success(ReplyResponse {
        email_id: new_email_id,
        in_reply_to: email_id.to_string(),
        status: if draft { "draft" } else { "sent" },
    })
    .print();

    Ok(())
}

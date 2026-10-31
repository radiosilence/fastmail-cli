use crate::jmap::authenticated_client;
use crate::models::Output;

pub async fn mark_spam(email_id: &str) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    client.mark_spam(email_id).await?;

    Output::<()>::success_msg("Email marked as spam").print();

    Ok(())
}

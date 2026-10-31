use crate::jmap::authenticated_client;
use crate::models::Output;

pub async fn move_email(email_id: &str, mailbox: &str) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    let mailbox = client.find_mailbox(mailbox).await?;
    client.move_email(email_id, &mailbox.id).await?;

    Output::<()>::success_msg(format!("Moved email to {}", mailbox.name)).print();

    Ok(())
}

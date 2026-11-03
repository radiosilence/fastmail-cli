use crate::jmap::authenticated_client;
use crate::jmap::lists::{MailingListReport, group_by_sender};
use crate::models::Output;
use crate::unsubscribe::{Plan, carry_out};

/// Every sender with an unsubscribe header, most prolific first.
pub async fn mailing_lists(limit: usize, after: Option<&str>) -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    let mut conditions = vec![serde_json::json!({ "header": ["List-Unsubscribe"] })];
    if let Some(after) = after {
        conditions.push(serde_json::json!({ "after": crate::jmap::normalize_date(after) }));
    }
    let filter = serde_json::json!({ "operator": "AND", "conditions": conditions });
    let (emails, total) = client.scan_list_emails(filter, limit).await?;
    let scanned = emails.len() as u32;
    Output::success(MailingListReport {
        scanned,
        total,
        lists: group_by_sender(emails),
    })
    .print();
    Ok(())
}

/// Leave the list `email_id` came from; without `yes`, say how and stop.
pub async fn unsubscribe(email_id: &str, yes: bool) -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    let email = client.get_list_email(email_id).await?;
    let plan = Plan::for_options(&email.unsubscribe());
    if let Plan::Manual(_) = plan {
        Output::<()>::error(plan.describe()).print();
        std::process::exit(1);
    }
    if !yes {
        eprintln!("Would: {}\nUse -y to do it.", plan.describe());
        std::process::exit(1);
    }
    let outcome = carry_out(&client, &email, &plan).await;
    if outcome.success {
        Output::<()>::success_msg(outcome.message).print();
        Ok(())
    } else {
        let mut msg = outcome.message;
        for url in outcome.manual_urls {
            msg.push_str(&format!("\n{url}"));
        }
        Output::<()>::error(msg).print();
        std::process::exit(1);
    }
}

pub async fn mark_not_spam(email_id: &str) -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    client.mark_not_spam(email_id).await?;
    Output::<()>::success_msg("Email marked as not spam and moved to the inbox").print();
    Ok(())
}

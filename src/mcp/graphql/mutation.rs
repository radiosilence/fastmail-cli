//! GraphQL mutation resolvers

use async_graphql::{Context, Object, Result};

use crate::models::EmailAddress;
use crate::util::parse_addresses;

use super::types::*;

pub struct MutationRoot;

#[Object]
#[allow(clippy::too_many_arguments)]
impl MutationRoot {
    /// Compose and send a new email. ALWAYS use action=PREVIEW first, show the user, then CONFIRM or DRAFT with the confirmation_token from the preview.
    async fn send_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "PREVIEW first, then CONFIRM to send or DRAFT to save")]
        action: SendAction,
        #[graphql(desc = "Recipient email address(es), comma-separated")] to: String,
        #[graphql(desc = "Email subject line")] subject: String,
        #[graphql(desc = "Email body text")] body: String,
        #[graphql(desc = "CC recipients, comma-separated")] cc: Option<String>,
        #[graphql(desc = "BCC recipients (hidden), comma-separated")] bcc: Option<String>,
        #[graphql(desc = "Send from a specific identity/email address")] from: Option<String>,
        #[graphql(desc = "HTML body content (alternative to plain text)")] html_body: Option<
            String,
        >,
        #[graphql(desc = "Token from PREVIEW response — required for CONFIRM/DRAFT")]
        confirmation_token: Option<String>,
    ) -> Result<GqlComposeResult> {
        let to_addrs = parse_addresses(&to);
        let cc_addrs = cc.as_deref().map(parse_addresses).unwrap_or_default();
        let bcc_addrs = bcc.as_deref().map(parse_addresses).unwrap_or_default();
        let token = super::types::confirmation_token(&[&to, &subject, &body]);

        if matches!(action, SendAction::Preview) {
            let mut preview =
                format_send_preview(&to_addrs, &cc_addrs, &bcc_addrs, &subject, &body);
            if html_body.is_some() {
                preview.push_str("\n[HTML body included]");
            }
            return Ok(GqlComposeResult {
                success: true,
                email_id: None,
                preview: Some(preview),
                confirmation_token: Some(token),
                error: None,
            });
        }

        if confirmation_token.as_deref() != Some(&token) {
            return Ok(GqlComposeResult {
                success: false,
                email_id: None,
                preview: None,
                confirmation_token: None,
                error: Some(
                    "Missing or invalid confirmation_token. Use action=PREVIEW first to get the token."
                        .to_string(),
                ),
            });
        }

        let draft = matches!(action, SendAction::Draft);
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let mut client = client.lock().await;

        match client
            .send_email(
                to_addrs,
                &subject,
                &body,
                None,
                crate::jmap::ComposeParams {
                    cc: cc_addrs,
                    bcc: bcc_addrs,
                    from: from.as_deref(),
                    draft,
                    html_body,
                    attachments: vec![],
                },
            )
            .await
        {
            Ok(email_id) => Ok(GqlComposeResult {
                success: true,
                email_id: Some(email_id),
                preview: None,
                confirmation_token: None,
                error: None,
            }),
            Err(e) => Ok(GqlComposeResult {
                success: false,
                email_id: None,
                preview: None,
                confirmation_token: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Reply to an existing email thread. ALWAYS use action=PREVIEW first.
    async fn reply_to_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "PREVIEW first, then CONFIRM to send or DRAFT to save")]
        action: SendAction,
        #[graphql(desc = "The email ID to reply to")] email_id: String,
        #[graphql(desc = "Reply body text (your response, without quoting original)")] body: String,
        #[graphql(desc = "Reply to all recipients")] all: Option<bool>,
        #[graphql(desc = "CC recipients, comma-separated")] cc: Option<String>,
        #[graphql(desc = "BCC recipients, comma-separated")] bcc: Option<String>,
        #[graphql(desc = "Send from a specific identity/email address")] from: Option<String>,
        #[graphql(desc = "HTML body content (alternative to plain text)")] html_body: Option<
            String,
        >,
        #[graphql(desc = "Token from PREVIEW response — required for CONFIRM/DRAFT")]
        confirmation_token: Option<String>,
    ) -> Result<GqlComposeResult> {
        let token = super::types::confirmation_token(&[&email_id, &body]);
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let mut client = client.lock().await;

        let original = client.get_email(&email_id).await?;
        let reply_all = all.unwrap_or(false);
        let cc_addrs = cc.as_deref().map(parse_addresses).unwrap_or_default();
        let bcc_addrs = bcc.as_deref().map(parse_addresses).unwrap_or_default();

        let subject = if original
            .subject
            .as_ref()
            .is_some_and(|s| s.to_lowercase().starts_with("re:"))
        {
            original.subject.clone().unwrap_or_default()
        } else {
            format!("Re: {}", original.subject.as_deref().unwrap_or(""))
        };

        let to_addrs: Vec<EmailAddress> = original.from.clone().unwrap_or_default();

        if matches!(action, SendAction::Preview) {
            let in_reply_to = original
                .message_id
                .as_ref()
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or_else(|| "(none)".to_string());
            let mut preview = format!(
                "REPLY PREVIEW:\nTo: {}\nCC: {}\nBCC: {}\nSubject: {}\nIn-Reply-To: {}\n\n--- Your Reply ---\n{}",
                format_addrs(&to_addrs),
                if cc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_addrs(&cc_addrs)
                },
                if bcc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_addrs(&bcc_addrs)
                },
                subject,
                in_reply_to,
                body
            );
            if html_body.is_some() {
                preview.push_str("\n[HTML body included]");
            }
            return Ok(GqlComposeResult {
                success: true,
                email_id: None,
                preview: Some(preview),
                confirmation_token: Some(token),
                error: None,
            });
        }

        if confirmation_token.as_deref() != Some(&token) {
            return Ok(GqlComposeResult {
                success: false,
                email_id: None,
                preview: None,
                confirmation_token: None,
                error: Some(
                    "Missing or invalid confirmation_token. Use action=PREVIEW first.".to_string(),
                ),
            });
        }

        let draft = matches!(action, SendAction::Draft);
        match client
            .reply_email(
                &original,
                &body,
                reply_all,
                crate::jmap::ComposeParams {
                    cc: cc_addrs,
                    bcc: bcc_addrs,
                    from: from.as_deref(),
                    draft,
                    html_body,
                    attachments: vec![],
                },
            )
            .await
        {
            Ok(eid) => Ok(GqlComposeResult {
                success: true,
                email_id: Some(eid),
                preview: None,
                confirmation_token: None,
                error: None,
            }),
            Err(e) => Ok(GqlComposeResult {
                success: false,
                email_id: None,
                preview: None,
                confirmation_token: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Forward an email to new recipients. ALWAYS use action=PREVIEW first.
    async fn forward_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "PREVIEW first, then CONFIRM to send or DRAFT to save")]
        action: SendAction,
        #[graphql(desc = "The email ID to forward")] email_id: String,
        #[graphql(desc = "Recipient email address(es), comma-separated")] to: String,
        #[graphql(desc = "Your message to include above forwarded content")] body: Option<String>,
        #[graphql(desc = "CC recipients, comma-separated")] cc: Option<String>,
        #[graphql(desc = "BCC recipients, comma-separated")] bcc: Option<String>,
        #[graphql(desc = "Send from a specific identity/email address")] from: Option<String>,
        #[graphql(desc = "HTML body content (alternative to plain text)")] html_body: Option<
            String,
        >,
        #[graphql(desc = "Token from PREVIEW response — required for CONFIRM/DRAFT")]
        confirmation_token: Option<String>,
    ) -> Result<GqlComposeResult> {
        let body_str = body.as_deref().unwrap_or("");
        let token = super::types::confirmation_token(&[&email_id, &to, body_str]);
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let mut client = client.lock().await;

        let original = client.get_email(&email_id).await?;
        let to_addrs = parse_addresses(&to);
        let cc_addrs = cc.as_deref().map(parse_addresses).unwrap_or_default();
        let bcc_addrs = bcc.as_deref().map(parse_addresses).unwrap_or_default();

        let subject = if original
            .subject
            .as_ref()
            .is_some_and(|s| s.to_lowercase().starts_with("fwd:"))
        {
            original.subject.clone().unwrap_or_default()
        } else {
            format!("Fwd: {}", original.subject.as_deref().unwrap_or(""))
        };

        if matches!(action, SendAction::Preview) {
            let original_body = original.text_content().unwrap_or("");
            let sender = format_addrs(&original.from.clone().unwrap_or_default());

            let mut preview = format!(
                "FORWARD PREVIEW:\nTo: {}\nCC: {}\nBCC: {}\nSubject: {}\nForwarding from: {}\n\n--- Your Message ---\n{}\n\n--- Forwarded ---\nFrom: {}\nDate: {}\nSubject: {}\n\n{}",
                format_addrs(&to_addrs),
                if cc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_addrs(&cc_addrs)
                },
                if bcc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_addrs(&bcc_addrs)
                },
                subject,
                sender,
                body_str,
                sender,
                original.received_at.as_deref().unwrap_or("unknown"),
                original.subject.as_deref().unwrap_or(""),
                original_body,
            );
            if html_body.is_some() {
                preview.push_str("\n[HTML body included]");
            }
            return Ok(GqlComposeResult {
                success: true,
                email_id: None,
                preview: Some(preview),
                confirmation_token: Some(token),
                error: None,
            });
        }

        if confirmation_token.as_deref() != Some(&token) {
            return Ok(GqlComposeResult {
                success: false,
                email_id: None,
                preview: None,
                confirmation_token: None,
                error: Some(
                    "Missing or invalid confirmation_token. Use action=PREVIEW first.".to_string(),
                ),
            });
        }

        let draft = matches!(action, SendAction::Draft);
        match client
            .forward_email(
                &original,
                to_addrs,
                body_str,
                crate::jmap::ComposeParams {
                    cc: cc_addrs,
                    bcc: bcc_addrs,
                    from: from.as_deref(),
                    draft,
                    html_body,
                    attachments: vec![],
                },
            )
            .await
        {
            Ok(eid) => Ok(GqlComposeResult {
                success: true,
                email_id: Some(eid),
                preview: None,
                confirmation_token: None,
                error: None,
            }),
            Err(e) => Ok(GqlComposeResult {
                success: false,
                email_id: None,
                preview: None,
                confirmation_token: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Move an email to a different mailbox/folder.
    async fn move_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID to move")] email_id: String,
        #[graphql(desc = "Target mailbox name (e.g., 'Archive', 'Trash') or role")]
        target_mailbox: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let mut client = client.lock().await;

        let email = client.get_email(&email_id).await?;
        let target = client.find_mailbox(&target_mailbox).await?;

        match client.move_email(&email_id, &target.id).await {
            Ok(()) => Ok(GqlStatus {
                success: true,
                message: Some(format!(
                    "Moved \"{}\" to {}",
                    email.subject.as_deref().unwrap_or("(no subject)"),
                    target.name
                )),
                error: None,
            }),
            Err(e) => Ok(GqlStatus {
                success: false,
                message: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Mark an email as read or unread.
    async fn mark_as_read(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID")] email_id: String,
        #[graphql(desc = "true to mark read, false to mark unread (default: true)")] read: Option<
            bool,
        >,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let client = client.lock().await;
        let read = read.unwrap_or(true);

        let email = client.get_email(&email_id).await?;
        let mut keywords = email.keywords.clone();
        if read {
            keywords.insert("$seen".to_string(), true);
        } else {
            keywords.remove("$seen");
        }

        match client.set_keywords(&email_id, keywords).await {
            Ok(()) => {
                let status = if read { "read" } else { "unread" };
                Ok(GqlStatus {
                    success: true,
                    message: Some(format!(
                        "Marked \"{}\" as {status}",
                        email.subject.as_deref().unwrap_or("(no subject)")
                    )),
                    error: None,
                })
            }
            Err(e) => Ok(GqlStatus {
                success: false,
                message: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Mark an email as spam. Moves to Junk AND trains the spam filter. MUST use action=PREVIEW first.
    async fn mark_as_spam(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID")] email_id: String,
        #[graphql(desc = "PREVIEW first, then CONFIRM")] action: SpamAction,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let mut client = client.lock().await;

        let email = client.get_email(&email_id).await?;

        if matches!(action, SpamAction::Preview) {
            let sender = email
                .from
                .as_ref()
                .and_then(|f| f.first())
                .map(|a| a.to_string())
                .unwrap_or_else(|| "(unknown)".to_string());
            return Ok(GqlStatus {
                success: true,
                message: Some(format!(
                    "SPAM PREVIEW — This will:\n1. Move to Junk folder\n2. Train spam filter\n\nEmail: \"{}\"\nFrom: {}\n\nUse action=CONFIRM to proceed.",
                    email.subject.as_deref().unwrap_or("(no subject)"),
                    sender
                )),
                error: None,
            });
        }

        match client.mark_spam(&email_id).await {
            Ok(()) => Ok(GqlStatus {
                success: true,
                message: Some(format!(
                    "Marked as spam: \"{}\"",
                    email.subject.as_deref().unwrap_or("(no subject)")
                )),
                error: None,
            }),
            Err(e) => Ok(GqlStatus {
                success: false,
                message: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Create a new masked email address.
    async fn create_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The website/domain this masked email is for")] for_domain: Option<String>,
        #[graphql(desc = "A note to remember what this is for")] description: Option<String>,
        #[graphql(desc = "Custom prefix for the email address")] prefix: Option<String>,
    ) -> Result<GqlMaskedEmail> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let client = client.lock().await;
        let masked = client
            .create_masked_email(
                for_domain.as_deref(),
                description.as_deref(),
                prefix.as_deref(),
            )
            .await?;
        Ok(GqlMaskedEmail::from(masked))
    }

    /// Enable a disabled masked email address.
    async fn enable_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The masked email ID")] id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let client = client.lock().await;
        match client
            .update_masked_email(&id, Some("enabled"), None, None)
            .await
        {
            Ok(()) => Ok(GqlStatus {
                success: true,
                message: Some(format!("Masked email {id} enabled.")),
                error: None,
            }),
            Err(e) => Ok(GqlStatus {
                success: false,
                message: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Disable a masked email address. Emails sent to it will be rejected.
    async fn disable_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The masked email ID")] id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let client = client.lock().await;
        match client
            .update_masked_email(&id, Some("disabled"), None, None)
            .await
        {
            Ok(()) => Ok(GqlStatus {
                success: true,
                message: Some(format!("Masked email {id} disabled.")),
                error: None,
            }),
            Err(e) => Ok(GqlStatus {
                success: false,
                message: None,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Permanently delete a masked email address. Cannot be undone!
    async fn delete_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The masked email ID")] id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let client = client.lock().await;
        match client
            .update_masked_email(&id, Some("deleted"), None, None)
            .await
        {
            Ok(()) => Ok(GqlStatus {
                success: true,
                message: Some(format!("Masked email {id} deleted.")),
                error: None,
            }),
            Err(e) => Ok(GqlStatus {
                success: false,
                message: None,
                error: Some(e.to_string()),
            }),
        }
    }
}

// ============ Formatting helpers (preview only) ============

fn format_addrs(addrs: &[EmailAddress]) -> String {
    if addrs.is_empty() {
        "(none)".to_string()
    } else {
        addrs
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn format_send_preview(
    to: &[EmailAddress],
    cc: &[EmailAddress],
    bcc: &[EmailAddress],
    subject: &str,
    body: &str,
) -> String {
    format!(
        "EMAIL PREVIEW:\nTo: {}\nCC: {}\nBCC: {}\nSubject: {}\n\n--- Body ---\n{}\n\nTo send: use action=CONFIRM. To save draft: use action=DRAFT.",
        format_addrs(to),
        if cc.is_empty() {
            "(none)".to_string()
        } else {
            format_addrs(cc)
        },
        if bcc.is_empty() {
            "(none)".to_string()
        } else {
            format_addrs(bcc)
        },
        subject,
        body
    )
}

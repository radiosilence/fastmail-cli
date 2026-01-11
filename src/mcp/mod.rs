//! MCP (Model Context Protocol) server for Fastmail
//!
//! Exposes Fastmail functionality as MCP tools for use with Claude and other LLMs.

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use tokio::sync::Mutex;

use crate::carddav::CardDavClient;
use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::EmailAddress;
use crate::util::{MCP_IMAGE_MAX_BYTES, extract_text, infer_image_mime, is_image, resize_image};

type ToolResult = std::result::Result<CallToolResult, McpError>;

mod format;
use format::*;

// ============ Request Types ============

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListEmailsRequest {
    /// Mailbox name (e.g., 'INBOX', 'Sent', 'Archive') or role (e.g., 'inbox', 'sent', 'drafts', 'trash', 'junk')
    pub mailbox: String,
    /// Maximum number of emails to return (default 25, max 100)
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetEmailRequest {
    /// The email ID (obtained from list_emails or search_emails)
    pub email_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchEmailsRequest {
    /// General search - searches subject, body, from, and to fields
    #[serde(default)]
    pub query: Option<String>,
    /// Search sender address/name
    #[serde(default)]
    pub from: Option<String>,
    /// Search recipient address/name
    #[serde(default)]
    pub to: Option<String>,
    /// Search CC recipients
    #[serde(default)]
    pub cc: Option<String>,
    /// Search subject line only
    #[serde(default)]
    pub subject: Option<String>,
    /// Search email body only
    #[serde(default)]
    pub body: Option<String>,
    /// Limit search to a specific mailbox/folder
    #[serde(default)]
    pub mailbox: Option<String>,
    /// Only emails with attachments
    #[serde(default)]
    pub has_attachment: Option<bool>,
    /// Emails before this date (YYYY-MM-DD or ISO 8601)
    #[serde(default)]
    pub before: Option<String>,
    /// Emails after this date (YYYY-MM-DD or ISO 8601)
    #[serde(default)]
    pub after: Option<String>,
    /// Only unread emails
    #[serde(default)]
    pub unread: Option<bool>,
    /// Only flagged/starred emails
    #[serde(default)]
    pub flagged: Option<bool>,
    /// Maximum number of results (default 25, max 100)
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MoveEmailRequest {
    /// The email ID to move
    pub email_id: String,
    /// Target mailbox name (e.g., 'Archive', 'Trash') or role (e.g., 'archive', 'trash')
    pub target_mailbox: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MarkAsReadRequest {
    /// The email ID
    pub email_id: String,
    /// true to mark read, false to mark unread (default: true)
    #[serde(default)]
    pub read: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MarkAsSpamRequest {
    /// The email ID to mark as spam
    pub email_id: String,
    /// 'preview' first to see what will happen, then 'confirm' after user approval
    pub action: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SendEmailRequest {
    /// 'preview' to see the draft, 'confirm' to send - ALWAYS preview first
    pub action: String,
    /// Recipient email address(es), comma-separated
    pub to: String,
    /// Email subject line
    pub subject: String,
    /// Email body text
    pub body: String,
    /// CC recipients, comma-separated
    #[serde(default)]
    pub cc: Option<String>,
    /// BCC recipients (hidden), comma-separated
    #[serde(default)]
    pub bcc: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReplyEmailRequest {
    /// 'preview' to see the draft, 'confirm' to send - ALWAYS preview first
    pub action: String,
    /// The email ID to reply to
    pub email_id: String,
    /// Reply body text (your response, without quoting original)
    pub body: String,
    /// Reply to all recipients
    #[serde(default)]
    pub all: Option<bool>,
    /// CC recipients for reply-all, comma-separated
    #[serde(default)]
    pub cc: Option<String>,
    /// BCC recipients (hidden), comma-separated
    #[serde(default)]
    pub bcc: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ForwardEmailRequest {
    /// 'preview' to see the draft, 'confirm' to send - ALWAYS preview first
    pub action: String,
    /// The email ID to forward
    pub email_id: String,
    /// Recipient email address(es), comma-separated
    pub to: String,
    /// Your message to include above the forwarded content
    #[serde(default)]
    pub body: Option<String>,
    /// CC recipients, comma-separated
    #[serde(default)]
    pub cc: Option<String>,
    /// BCC recipients (hidden), comma-separated
    #[serde(default)]
    pub bcc: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListAttachmentsRequest {
    /// The email ID to get attachments from
    pub email_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetAttachmentRequest {
    /// The email ID the attachment belongs to
    pub email_id: String,
    /// The blob ID of the attachment (from list_attachments)
    pub blob_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateMaskedEmailRequest {
    /// The website/domain this masked email is for (e.g., 'netflix.com')
    #[serde(default)]
    pub for_domain: Option<String>,
    /// A note to remember what this is for (e.g., 'Netflix account')
    #[serde(default)]
    pub description: Option<String>,
    /// Custom prefix for the email address (optional, random if not specified)
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaskedEmailIdRequest {
    /// The masked email ID (from list_masked_emails)
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchContactsRequest {
    /// Search query - matches name, email, or organization
    pub query: String,
}

// ============ Server Implementation ============

#[derive(Clone)]
pub struct FastmailMcp {
    client: Arc<Mutex<JmapClient>>,
    tool_router: ToolRouter<Self>,
}

impl FastmailMcp {
    pub async fn new() -> anyhow::Result<Self> {
        let config = Config::load()?;
        let token = config.get_token()?;

        let mut client = JmapClient::new(token);
        client.authenticate().await?;

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            tool_router: Self::tool_router(),
        })
    }

    fn text_result(text: impl Into<String>) -> ToolResult {
        Ok(CallToolResult::success(vec![Content::text(text.into())]))
    }

    fn error_result(msg: impl Into<String>) -> ToolResult {
        Ok(CallToolResult::error(vec![Content::text(msg.into())]))
    }

    fn parse_addresses(s: &str) -> Vec<EmailAddress> {
        s.split(',')
            .map(|e| EmailAddress {
                name: None,
                email: e.trim().to_string(),
            })
            .collect()
    }
}

#[tool_router]
impl FastmailMcp {
    // ============ Read-Only Tools ============

    #[tool(
        description = "List all mailboxes (folders) in the account with their unread counts. START HERE - use this to discover available folders before listing emails."
    )]
    async fn list_mailboxes(&self) -> ToolResult {
        let client = self.client.lock().await;
        match client.list_mailboxes().await {
            Ok(mut mailboxes) => {
                mailboxes.sort_by(|a, b| {
                    // Put role-based mailboxes first
                    match (&a.role, &b.role) {
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        _ => a.name.cmp(&b.name),
                    }
                });
                let text = mailboxes
                    .iter()
                    .map(format_mailbox)
                    .collect::<Vec<_>>()
                    .join("\n");
                Self::text_result(text)
            }
            Err(e) => Self::error_result(format!("Failed to list mailboxes: {}", e)),
        }
    }

    #[tool(
        description = "List emails in a specific mailbox/folder. Returns email summaries with ID, from, subject, date, and preview. Use the email ID with get_email for full content."
    )]
    async fn list_emails(&self, Parameters(req): Parameters<ListEmailsRequest>) -> ToolResult {
        let client = self.client.lock().await;
        let limit = req.limit.unwrap_or(25).min(100);

        match client.find_mailbox(&req.mailbox).await {
            Ok(mailbox) => match client.list_emails(&mailbox.id, limit).await {
                Ok(emails) => {
                    if emails.is_empty() {
                        return Self::text_result(format!("No emails in {}", req.mailbox));
                    }
                    let text = emails
                        .iter()
                        .map(format_email_summary)
                        .collect::<Vec<_>>()
                        .join("\n\n---\n\n");
                    Self::text_result(text)
                }
                Err(e) => Self::error_result(format!("Failed to list emails: {}", e)),
            },
            Err(e) => Self::error_result(format!("Mailbox not found: {} ({})", req.mailbox, e)),
        }
    }

    #[tool(
        description = "Get the full content of a specific email by its ID. Automatically includes the full thread context (all emails in the conversation) sorted oldest-first."
    )]
    async fn get_email(&self, Parameters(req): Parameters<GetEmailRequest>) -> ToolResult {
        let client = self.client.lock().await;

        match client.get_email(&req.email_id).await {
            Ok(email) => {
                // Get full thread context
                match client.get_thread(&req.email_id).await {
                    Ok(mut thread_emails) if thread_emails.len() > 1 => {
                        // Sort by date ascending
                        thread_emails.sort_by(|a, b| a.received_at.cmp(&b.received_at));

                        let thread_text = thread_emails
                            .iter()
                            .enumerate()
                            .map(|(i, e)| {
                                let marker = if e.id == req.email_id {
                                    ">>> SELECTED EMAIL <<<\n"
                                } else {
                                    ""
                                };
                                format!(
                                    "{}[{}/{}]\n{}",
                                    marker,
                                    i + 1,
                                    thread_emails.len(),
                                    format_email_full(e)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n========== THREAD ==========\n\n");

                        Self::text_result(format!(
                            "Thread contains {} emails:\n\n{}",
                            thread_emails.len(),
                            thread_text
                        ))
                    }
                    _ => Self::text_result(format_email_full(&email)),
                }
            }
            Err(e) => Self::error_result(format!("Email not found: {} ({})", req.email_id, e)),
        }
    }

    #[tool(
        description = "Search for emails with flexible filters. Use 'query' for general search, or specific fields for precise filtering. Supports date ranges, attachment filtering, unread/flagged status."
    )]
    async fn search_emails(&self, Parameters(req): Parameters<SearchEmailsRequest>) -> ToolResult {
        let client = self.client.lock().await;
        let limit = req.limit.unwrap_or(25).min(100);

        // Build search filter
        let filter = crate::commands::SearchFilter {
            text: req.query,
            from: req.from,
            to: req.to,
            cc: req.cc,
            bcc: None,
            subject: req.subject,
            body: req.body,
            mailbox: None,
            has_attachment: req.has_attachment.unwrap_or(false),
            min_size: None,
            max_size: None,
            before: req.before,
            after: req.after,
            unread: req.unread.unwrap_or(false),
            flagged: req.flagged.unwrap_or(false),
        };

        // Get mailbox ID if specified
        let mailbox_id = if let Some(ref name) = req.mailbox {
            match client.find_mailbox(name).await {
                Ok(m) => Some(m.id),
                Err(_) => None,
            }
        } else {
            None
        };

        match client
            .search_emails_filtered(&filter, mailbox_id.as_deref(), limit)
            .await
        {
            Ok(emails) => {
                if emails.is_empty() {
                    return Self::text_result("No emails found.");
                }
                let text = emails
                    .iter()
                    .map(format_email_summary)
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                Self::text_result(text)
            }
            Err(e) => Self::error_result(format!("Search failed: {}", e)),
        }
    }

    // ============ Mutation Tools ============

    #[tool(description = "Move an email to a different mailbox/folder.")]
    async fn move_email(&self, Parameters(req): Parameters<MoveEmailRequest>) -> ToolResult {
        let client = self.client.lock().await;

        // Verify email exists
        let email = match client.get_email(&req.email_id).await {
            Ok(e) => e,
            Err(e) => return Self::error_result(format!("Email not found: {}", e)),
        };

        // Find target mailbox
        let target = match client.find_mailbox(&req.target_mailbox).await {
            Ok(m) => m,
            Err(e) => {
                return Self::error_result(format!(
                    "Mailbox not found: {} ({})",
                    req.target_mailbox, e
                ));
            }
        };

        match client.move_email(&req.email_id, &target.id).await {
            Ok(()) => Self::text_result(format!(
                "Moved email \"{}\" to {}",
                email.subject.as_deref().unwrap_or("(no subject)"),
                target.name
            )),
            Err(e) => Self::error_result(format!("Failed to move email: {}", e)),
        }
    }

    #[tool(description = "Mark an email as read or unread.")]
    async fn mark_as_read(&self, Parameters(req): Parameters<MarkAsReadRequest>) -> ToolResult {
        let client = self.client.lock().await;
        let read = req.read.unwrap_or(true);

        // Get email first
        let email = match client.get_email(&req.email_id).await {
            Ok(e) => e,
            Err(e) => return Self::error_result(format!("Email not found: {}", e)),
        };

        // Update keywords
        let mut keywords = email.keywords.clone();
        if read {
            keywords.insert("$seen".to_string(), true);
        } else {
            keywords.remove("$seen");
        }

        match client.set_keywords(&req.email_id, keywords).await {
            Ok(()) => {
                let status = if read { "read" } else { "unread" };
                Self::text_result(format!(
                    "Marked \"{}\" as {}",
                    email.subject.as_deref().unwrap_or("(no subject)"),
                    status
                ))
            }
            Err(e) => Self::error_result(format!("Failed to mark as read: {}", e)),
        }
    }

    #[tool(
        description = "Mark an email as spam. This moves it to Junk AND trains the spam filter - affects future filtering! MUST use action='preview' first, then 'confirm' after user approval."
    )]
    async fn mark_as_spam(&self, Parameters(req): Parameters<MarkAsSpamRequest>) -> ToolResult {
        let client = self.client.lock().await;

        let email = match client.get_email(&req.email_id).await {
            Ok(e) => e,
            Err(e) => return Self::error_result(format!("Email not found: {}", e)),
        };

        if req.action == "preview" {
            return Self::text_result(format!(
                "SPAM PREVIEW - This will:\n\
                1. Move the email to Junk folder\n\
                2. Train the spam filter to mark similar emails as spam\n\n\
                Email: \"{}\"\n\
                From: {}\n\n\
                To proceed, call this tool again with action: \"confirm\"",
                email.subject.as_deref().unwrap_or("(no subject)"),
                format_address_list(email.from.as_ref())
            ));
        }

        match client.mark_spam(&req.email_id).await {
            Ok(()) => Self::text_result(format!(
                "Marked as spam: \"{}\" from {}",
                email.subject.as_deref().unwrap_or("(no subject)"),
                format_address_list(email.from.as_ref())
            )),
            Err(e) => Self::error_result(format!("Failed to mark as spam: {}", e)),
        }
    }

    // ============ Send/Reply/Forward Tools ============

    #[tool(
        description = "Compose and send a new email. CRITICAL: You MUST call with action='preview' first, show the user the draft, get explicit approval, then call again with action='confirm'. NEVER skip the preview step."
    )]
    async fn send_email(&self, Parameters(req): Parameters<SendEmailRequest>) -> ToolResult {
        let to_addrs = Self::parse_addresses(&req.to);
        let cc_addrs = req
            .cc
            .as_ref()
            .map(|s| Self::parse_addresses(s))
            .unwrap_or_default();
        let bcc_addrs = req
            .bcc
            .as_ref()
            .map(|s| Self::parse_addresses(s))
            .unwrap_or_default();

        if req.action == "preview" {
            return Self::text_result(format!(
                "EMAIL PREVIEW - Review before sending:\n\n\
                To: {}\n\
                CC: {}\n\
                BCC: {}\n\
                Subject: {}\n\n\
                --- Body ---\n\
                {}\n\n\
                ---\n\
                To send this email, call this tool again with action: \"confirm\" and the same parameters.",
                format_address_list(Some(&to_addrs)),
                if cc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_address_list(Some(&cc_addrs))
                },
                if bcc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_address_list(Some(&bcc_addrs))
                },
                req.subject,
                req.body
            ));
        }

        let client = self.client.lock().await;
        match client
            .send_email(
                to_addrs.clone(),
                cc_addrs,
                bcc_addrs,
                &req.subject,
                &req.body,
                None,
            )
            .await
        {
            Ok(email_id) => Self::text_result(format!(
                "Email sent successfully!\n\
                To: {}\n\
                Subject: {}\n\
                Email ID: {}",
                format_address_list(Some(&to_addrs)),
                req.subject,
                email_id
            )),
            Err(e) => Self::error_result(format!("Failed to send email: {}", e)),
        }
    }

    #[tool(
        description = "Reply to an existing email thread. CRITICAL: You MUST call with action='preview' first, show the user the draft, get explicit approval, then call again with action='confirm'. NEVER skip the preview step. For reply-all, set all=true."
    )]
    async fn reply_to_email(&self, Parameters(req): Parameters<ReplyEmailRequest>) -> ToolResult {
        let client = self.client.lock().await;

        let original = match client.get_email(&req.email_id).await {
            Ok(e) => e,
            Err(e) => return Self::error_result(format!("Email not found: {}", e)),
        };

        let reply_all = req.all.unwrap_or(false);
        let cc_addrs = req
            .cc
            .as_ref()
            .map(|s| Self::parse_addresses(s))
            .unwrap_or_default();
        let bcc_addrs = req
            .bcc
            .as_ref()
            .map(|s| Self::parse_addresses(s))
            .unwrap_or_default();

        // Build subject
        let subject = if original
            .subject
            .as_ref()
            .is_some_and(|s| s.to_lowercase().starts_with("re:"))
        {
            original.subject.clone().unwrap_or_default()
        } else {
            format!("Re: {}", original.subject.as_deref().unwrap_or(""))
        };

        // Determine recipients
        let to_addrs: Vec<EmailAddress> = original.from.clone().unwrap_or_default();

        if req.action == "preview" {
            return Self::text_result(format!(
                "REPLY PREVIEW - Review before sending:\n\n\
                To: {}\n\
                CC: {}\n\
                BCC: {}\n\
                Subject: {}\n\
                In-Reply-To: {}\n\n\
                --- Your Reply ---\n\
                {}\n\n\
                ---\n\
                To send this reply, call this tool again with action: \"confirm\" and the same parameters.",
                format_address_list(Some(&to_addrs)),
                if cc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_address_list(Some(&cc_addrs))
                },
                if bcc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_address_list(Some(&bcc_addrs))
                },
                subject,
                original
                    .message_id
                    .as_ref()
                    .and_then(|v| v.first())
                    .unwrap_or(&"(none)".to_string()),
                req.body
            ));
        }

        match client
            .reply_email(&original, &req.body, reply_all, cc_addrs, bcc_addrs)
            .await
        {
            Ok(email_id) => Self::text_result(format!(
                "Reply sent successfully!\n\
                To: {}\n\
                Subject: {}\n\
                Email ID: {}",
                format_address_list(Some(&to_addrs)),
                subject,
                email_id
            )),
            Err(e) => Self::error_result(format!("Failed to send reply: {}", e)),
        }
    }

    #[tool(
        description = "Forward an email to new recipients. CRITICAL: You MUST call with action='preview' first, show the user the draft, get explicit approval, then call again with action='confirm'. NEVER skip the preview step."
    )]
    async fn forward_email(&self, Parameters(req): Parameters<ForwardEmailRequest>) -> ToolResult {
        let client = self.client.lock().await;

        let original = match client.get_email(&req.email_id).await {
            Ok(e) => e,
            Err(e) => return Self::error_result(format!("Email not found: {}", e)),
        };

        let to_addrs = Self::parse_addresses(&req.to);
        let cc_addrs = req
            .cc
            .as_ref()
            .map(|s| Self::parse_addresses(s))
            .unwrap_or_default();
        let bcc_addrs = req
            .bcc
            .as_ref()
            .map(|s| Self::parse_addresses(s))
            .unwrap_or_default();
        let body = req.body.as_deref().unwrap_or("");

        // Build subject
        let subject = if original
            .subject
            .as_ref()
            .is_some_and(|s| s.to_lowercase().starts_with("fwd:"))
        {
            original.subject.clone().unwrap_or_default()
        } else {
            format!("Fwd: {}", original.subject.as_deref().unwrap_or(""))
        };

        // Get original body for preview
        let original_body = original
            .body_values
            .as_ref()
            .and_then(|bv| bv.values().next())
            .map(|v| v.value.as_str())
            .unwrap_or("");

        let sender = format_address_list(original.from.as_ref());

        if req.action == "preview" {
            return Self::text_result(format!(
                "FORWARD PREVIEW - Review before sending:\n\n\
                To: {}\n\
                CC: {}\n\
                BCC: {}\n\
                Subject: {}\n\
                Forwarding from: {}\n\n\
                --- Your Message + Forwarded Content ---\n\
                {}\n\n\
                ---------- Forwarded message ---------\n\
                From: {}\n\
                Date: {}\n\
                Subject: {}\n\n\
                {}\n\n\
                ---\n\
                To send this forward, call this tool again with action: \"confirm\" and the same parameters.",
                format_address_list(Some(&to_addrs)),
                if cc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_address_list(Some(&cc_addrs))
                },
                if bcc_addrs.is_empty() {
                    "(none)".to_string()
                } else {
                    format_address_list(Some(&bcc_addrs))
                },
                subject,
                sender,
                body,
                sender,
                original.received_at.as_deref().unwrap_or("unknown date"),
                original.subject.as_deref().unwrap_or(""),
                original_body
            ));
        }

        match client
            .forward_email(&original, to_addrs.clone(), body, cc_addrs, bcc_addrs)
            .await
        {
            Ok(email_id) => Self::text_result(format!(
                "Email forwarded successfully!\n\
                To: {}\n\
                Subject: {}\n\
                Email ID: {}",
                format_address_list(Some(&to_addrs)),
                subject,
                email_id
            )),
            Err(e) => Self::error_result(format!("Failed to forward email: {}", e)),
        }
    }

    // ============ Attachment Tools ============

    #[tool(
        description = "List all attachments on an email. Returns attachment names, types, sizes, and blob IDs for downloading."
    )]
    async fn list_attachments(
        &self,
        Parameters(req): Parameters<ListAttachmentsRequest>,
    ) -> ToolResult {
        let client = self.client.lock().await;

        let email = match client.get_email(&req.email_id).await {
            Ok(e) => e,
            Err(e) => return Self::error_result(format!("Email not found: {}", e)),
        };

        let attachments = email.attachments.as_ref();
        if attachments.is_none() || attachments.unwrap().is_empty() {
            return Self::text_result("No attachments on this email.");
        }

        let lines: Vec<String> = attachments
            .unwrap()
            .iter()
            .filter(|a| a.blob_id.is_some())
            .enumerate()
            .map(|(i, a)| {
                let size = a.size;
                let size_str = if size > 1024 * 1024 {
                    format!("{:.1} MB", size as f64 / 1024.0 / 1024.0)
                } else if size > 1024 {
                    format!("{:.1} KB", size as f64 / 1024.0)
                } else {
                    format!("{} bytes", size)
                };
                format!(
                    "{}. {}\n   Type: {}\n   Size: {}\n   Blob ID: {}",
                    i + 1,
                    a.name.as_deref().unwrap_or("(unnamed)"),
                    a.content_type.as_deref().unwrap_or("unknown"),
                    size_str,
                    a.blob_id.as_deref().unwrap_or("none")
                )
            })
            .collect();

        Self::text_result(format!(
            "Attachments ({}):\n\n{}",
            lines.len(),
            lines.join("\n\n")
        ))
    }

    #[tool(
        description = "Download an attachment. Text files and documents (PDF, DOC, DOCX) have text extracted and returned. Images are resized if needed and returned as viewable content."
    )]
    async fn get_attachment(
        &self,
        Parameters(req): Parameters<GetAttachmentRequest>,
    ) -> ToolResult {
        let client = self.client.lock().await;

        // Get attachment info
        let email = match client.get_email(&req.email_id).await {
            Ok(e) => e,
            Err(e) => return Self::error_result(format!("Email not found: {}", e)),
        };

        let attachment = email.attachments.as_ref().and_then(|atts| {
            atts.iter()
                .find(|a| a.blob_id.as_deref() == Some(&req.blob_id))
        });

        let attachment = match attachment {
            Some(a) => a,
            None => return Self::error_result(format!("Attachment not found: {}", req.blob_id)),
        };

        let content_type = attachment
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");
        let name = attachment.name.as_deref().unwrap_or("attachment");

        // Download the blob
        let data = match client.download_blob(&req.blob_id).await {
            Ok(d) => d,
            Err(e) => return Self::error_result(format!("Failed to download: {}", e)),
        };

        // Handle images - resize and return as base64
        let mime = if is_image(content_type, name) {
            infer_image_mime(name).unwrap_or(content_type)
        } else {
            content_type
        };

        if is_image(mime, name) {
            return match resize_image(&data, mime, MCP_IMAGE_MAX_BYTES) {
                Ok((processed_data, mime_type)) => {
                    let base64_data = base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &processed_data,
                    );
                    Ok(CallToolResult::success(vec![Content::image(
                        base64_data,
                        mime_type,
                    )]))
                }
                Err(e) => Self::error_result(format!("Failed to process image: {}", e)),
            };
        }

        // Try to extract text from documents (PDF, DOC, DOCX, XLSX, PPTX, etc.)
        match extract_text(&data, name).await {
            Ok(Some(text)) => {
                return Self::text_result(format!("Extracted text from {}:\n\n{}", name, text));
            }
            Ok(None) => {}
            Err(e) => {
                return Self::error_result(format!("Failed to extract text: {}", e));
            }
        }

        // Binary file - return info only
        Self::text_result(format!(
            "Binary attachment: {}\nType: {}\nSize: {} bytes\n\nThis file type cannot be displayed directly.",
            name,
            content_type,
            data.len()
        ))
    }

    // ============ Masked Email Tools ============

    #[tool(description = "List all masked email addresses in the account.")]
    async fn list_masked_emails(&self) -> ToolResult {
        let client = self.client.lock().await;

        match client.list_masked_emails().await {
            Ok(mut masked_emails) => {
                // Sort: enabled first, then by email
                masked_emails.sort_by(|a, b| {
                    let a_enabled = a.state.as_deref() == Some("enabled");
                    let b_enabled = b.state.as_deref() == Some("enabled");
                    match (a_enabled, b_enabled) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        _ => a.email.cmp(&b.email),
                    }
                });

                let text = masked_emails
                    .iter()
                    .map(format_masked_email)
                    .collect::<Vec<_>>()
                    .join("\n\n");

                Self::text_result(format!(
                    "Masked Emails ({}):\n\n{}",
                    masked_emails.len(),
                    text
                ))
            }
            Err(e) => Self::error_result(format!("Failed to list masked emails: {}", e)),
        }
    }

    #[tool(
        description = "Create a new masked email address. Perfect for signups where you want a disposable address. The masked email forwards to your inbox."
    )]
    async fn create_masked_email(
        &self,
        Parameters(req): Parameters<CreateMaskedEmailRequest>,
    ) -> ToolResult {
        let client = self.client.lock().await;

        match client
            .create_masked_email(
                req.for_domain.as_deref(),
                req.description.as_deref(),
                req.prefix.as_deref(),
            )
            .await
        {
            Ok(masked) => Self::text_result(format!(
                "Created masked email:\n\n{}",
                format_masked_email(&masked)
            )),
            Err(e) => Self::error_result(format!("Failed to create masked email: {}", e)),
        }
    }

    #[tool(description = "Enable a disabled masked email address so it can receive emails again.")]
    async fn enable_masked_email(
        &self,
        Parameters(req): Parameters<MaskedEmailIdRequest>,
    ) -> ToolResult {
        let client = self.client.lock().await;

        match client
            .update_masked_email(&req.id, Some("enabled"), None, None)
            .await
        {
            Ok(()) => Self::text_result(format!("Masked email {} enabled.", req.id)),
            Err(e) => Self::error_result(format!("Failed to enable masked email: {}", e)),
        }
    }

    #[tool(
        description = "Disable a masked email address. Emails sent to it will be rejected but the address is preserved."
    )]
    async fn disable_masked_email(
        &self,
        Parameters(req): Parameters<MaskedEmailIdRequest>,
    ) -> ToolResult {
        let client = self.client.lock().await;

        match client
            .update_masked_email(&req.id, Some("disabled"), None, None)
            .await
        {
            Ok(()) => Self::text_result(format!("Masked email {} disabled.", req.id)),
            Err(e) => Self::error_result(format!("Failed to disable masked email: {}", e)),
        }
    }

    #[tool(description = "Permanently delete a masked email address. This cannot be undone!")]
    async fn delete_masked_email(
        &self,
        Parameters(req): Parameters<MaskedEmailIdRequest>,
    ) -> ToolResult {
        let client = self.client.lock().await;

        match client
            .update_masked_email(&req.id, Some("deleted"), None, None)
            .await
        {
            Ok(()) => Self::text_result(format!("Masked email {} deleted.", req.id)),
            Err(e) => Self::error_result(format!("Failed to delete masked email: {}", e)),
        }
    }

    // ============ Contact Tools (CardDAV) ============

    #[tool(
        description = "Search contacts by name, email, or organization. Use this to find someone's email address when composing. Returns name, emails, phones, and organization. Requires FASTMAIL_APP_PASSWORD to be set (API tokens don't work for CardDAV)."
    )]
    async fn search_contacts(
        &self,
        Parameters(req): Parameters<SearchContactsRequest>,
    ) -> ToolResult {
        let config = match Config::load() {
            Ok(c) => c,
            Err(e) => return Self::error_result(format!("Config error: {}", e)),
        };

        let username = match config.get_username() {
            Ok(u) => u,
            Err(_) => {
                return Self::error_result(
                    "Username not configured. Set FASTMAIL_USERNAME env var.",
                );
            }
        };

        let app_password = match config.get_app_password() {
            Ok(p) => p,
            Err(_) => {
                return Self::error_result(
                    "App password not configured. Set FASTMAIL_APP_PASSWORD env var (API tokens don't work for CardDAV).",
                );
            }
        };

        let client = CardDavClient::new(username, app_password);

        match client.search_contacts(&req.query).await {
            Ok(contacts) => {
                if contacts.is_empty() {
                    return Self::text_result(format!(
                        "No contacts found matching \"{}\"",
                        req.query
                    ));
                }

                let text = contacts
                    .iter()
                    .map(format_contact)
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");

                Self::text_result(format!("Found {} contact(s):\n\n{}", contacts.len(), text))
            }
            Err(e) => Self::error_result(format!("Failed to search contacts: {}", e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for FastmailMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "fastmail-cli".to_string(),
                title: Some("Fastmail MCP Server".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                icons: None,
                website_url: Some("https://github.com/radiosilence/fastmail-cli".to_string()),
            },
            instructions: Some(
                "Fastmail MCP Server - Read, search, and send emails via Claude.\n\n\
                ## Reading Emails\n\
                1. Use `list_mailboxes` to see available folders\n\
                2. Use `list_emails` with a mailbox name to see emails\n\
                3. Use `get_email` with an email ID to read full content\n\
                4. Use `search_emails` to find emails across all folders\n\n\
                ## Sending Emails (ALWAYS preview first!)\n\
                1. Use `send_email` with action=\"preview\" to draft\n\
                2. Review the preview with the user\n\
                3. Only use action=\"confirm\" after explicit user approval\n\n\
                ## Safety Rules\n\
                - NEVER send without showing preview first\n\
                - NEVER confirm send without explicit user approval\n\
                - Be careful with mark_as_spam - it affects future filtering"
                    .to_string(),
            ),
        }
    }
}

/// Run the MCP server with stdio transport
pub async fn run_server() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let service = FastmailMcp::new().await?;
    let server = service
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    server
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;

    Ok(())
}

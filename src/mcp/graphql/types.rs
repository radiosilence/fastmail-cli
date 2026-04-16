//! GraphQL type wrappers around existing model structs

use async_graphql::{Context, Enum, Object, Result, SimpleObject};

use crate::carddav::{Contact, ContactEmail, ContactPhone};
use crate::models::{Email, EmailAddress, Identity, Mailbox, MaskedEmail};

// ============ Output Types ============

#[derive(SimpleObject)]
#[graphql(name = "Mailbox")]
pub struct GqlMailbox {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub role: Option<String>,
    pub total_emails: u32,
    pub unread_emails: u32,
    pub total_threads: u32,
    pub unread_threads: u32,
    pub sort_order: u32,
}

impl From<Mailbox> for GqlMailbox {
    fn from(m: Mailbox) -> Self {
        Self {
            id: m.id,
            name: m.name,
            parent_id: m.parent_id,
            role: m.role,
            total_emails: m.total_emails,
            unread_emails: m.unread_emails,
            total_threads: m.total_threads,
            unread_threads: m.unread_threads,
            sort_order: m.sort_order,
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "EmailAddress")]
pub struct GqlEmailAddress {
    pub name: Option<String>,
    pub email: String,
}

impl From<EmailAddress> for GqlEmailAddress {
    fn from(a: EmailAddress) -> Self {
        Self {
            name: a.name,
            email: a.email,
        }
    }
}

pub(crate) fn convert_addrs(addrs: Option<Vec<EmailAddress>>) -> Vec<GqlEmailAddress> {
    addrs
        .unwrap_or_default()
        .into_iter()
        .map(GqlEmailAddress::from)
        .collect()
}

/// Compact email summary returned by list/search queries
#[derive(SimpleObject)]
#[graphql(name = "EmailSummary")]
pub struct GqlEmailSummary {
    pub id: String,
    pub thread_id: Option<String>,
    pub subject: Option<String>,
    #[graphql(name = "from")]
    pub sender: Vec<GqlEmailAddress>,
    #[graphql(name = "to")]
    pub recipients: Vec<GqlEmailAddress>,
    #[graphql(name = "cc")]
    pub cc_recipients: Vec<GqlEmailAddress>,
    pub received_at: Option<String>,
    pub preview: Option<String>,
    pub has_attachment: bool,
    pub is_unread: bool,
    pub is_flagged: bool,
    pub size: u64,
}

impl From<Email> for GqlEmailSummary {
    fn from(e: Email) -> Self {
        let is_unread = e.is_unread();
        let is_flagged = e.is_flagged();
        Self {
            id: e.id,
            thread_id: e.thread_id,
            subject: e.subject,
            sender: convert_addrs(e.from),
            recipients: convert_addrs(e.to),
            cc_recipients: convert_addrs(e.cc),
            received_at: e.received_at,
            preview: e.preview,
            has_attachment: e.has_attachment,
            is_unread,
            is_flagged,
            size: e.size,
        }
    }
}

/// Full email with body content and nested attachment resolution
pub struct GqlEmail(pub Email);

impl GqlEmail {
    /// Build attachment list from the inner email — shared by the nested resolver
    /// and the top-level `attachments`/`attachment` queries.
    pub fn make_attachments(&self) -> Vec<GqlAttachment> {
        self.0
            .attachments
            .as_ref()
            .map(|atts| {
                atts.iter()
                    .filter(|a| a.blob_id.is_some())
                    .map(|a| GqlAttachment {
                        blob_id: a.blob_id.clone().unwrap_or_default(),
                        name: a.name.clone(),
                        content_type: a.content_type.clone(),
                        size: a.size,
                        disposition: a.disposition.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[Object(name = "Email")]
impl GqlEmail {
    async fn id(&self) -> &str {
        &self.0.id
    }
    async fn blob_id(&self) -> Option<&str> {
        self.0.blob_id.as_deref()
    }
    async fn thread_id(&self) -> Option<&str> {
        self.0.thread_id.as_deref()
    }
    async fn subject(&self) -> Option<&str> {
        self.0.subject.as_deref()
    }
    async fn from(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.0.from.clone())
    }
    async fn to(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.0.to.clone())
    }
    async fn cc(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.0.cc.clone())
    }
    async fn bcc(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.0.bcc.clone())
    }
    async fn reply_to(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.0.reply_to.clone())
    }
    async fn received_at(&self) -> Option<&str> {
        self.0.received_at.as_deref()
    }
    async fn sent_at(&self) -> Option<&str> {
        self.0.sent_at.as_deref()
    }
    async fn preview(&self) -> Option<&str> {
        self.0.preview.as_deref()
    }
    async fn has_attachment(&self) -> bool {
        self.0.has_attachment
    }
    async fn is_unread(&self) -> bool {
        self.0.is_unread()
    }
    async fn is_flagged(&self) -> bool {
        self.0.is_flagged()
    }
    async fn is_draft(&self) -> bool {
        self.0.is_draft()
    }
    async fn size(&self) -> u64 {
        self.0.size
    }
    async fn message_id(&self) -> Option<&Vec<String>> {
        self.0.message_id.as_ref()
    }
    async fn in_reply_to(&self) -> Option<&Vec<String>> {
        self.0.in_reply_to.as_ref()
    }
    async fn references(&self) -> Option<&Vec<String>> {
        self.0.references.as_ref()
    }
    /// Plain text body content
    async fn text_body(&self) -> Option<&str> {
        self.0.text_content()
    }
    /// HTML body content
    async fn html_body(&self) -> Option<&str> {
        self.0.html_content()
    }
    /// Attachments with metadata. Select `content` on an attachment to fetch its data (images
    /// are base64-encoded, documents have text extracted). Content is lazily resolved — only
    /// fetched when you include it in your query.
    async fn attachments(&self) -> Vec<GqlAttachment> {
        self.make_attachments()
    }
    /// Mailbox IDs this email belongs to
    async fn mailbox_ids(&self) -> Vec<String> {
        self.0.mailbox_ids.keys().cloned().collect()
    }

    /// Keywords (flags) on this email: $seen, $flagged, $draft, $junk, etc.
    async fn keywords(&self) -> Vec<String> {
        self.0.keywords.keys().cloned().collect()
    }
}

/// Attachment with metadata and lazy content resolution.
/// Query just metadata fields (blobId, name, size, etc.) for listings,
/// or include `content` to download and process the attachment data.
pub struct GqlAttachment {
    pub blob_id: String,
    pub name: Option<String>,
    pub content_type: Option<String>,
    pub size: u64,
    pub disposition: Option<String>,
}

#[Object(name = "Attachment")]
impl GqlAttachment {
    async fn blob_id(&self) -> &str {
        &self.blob_id
    }
    async fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    async fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
    async fn size(&self) -> u64 {
        self.size
    }
    async fn disposition(&self) -> Option<&str> {
        self.disposition.as_deref()
    }
    /// Fetch the actual attachment content. Images are resized and base64-encoded,
    /// documents have text extracted. Only fetched when this field is included in the query.
    async fn content(&self, ctx: &Context<'_>) -> Result<GqlAttachmentContent> {
        let client = ctx.data::<tokio::sync::Mutex<crate::jmap::JmapClient>>()?;
        let client = client.lock().await;

        let content_type = self
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");
        let name = self.name.as_deref().unwrap_or("attachment");

        let data = client.download_blob(&self.blob_id).await?;

        let mime = if crate::util::is_image(content_type, name) {
            crate::util::infer_image_mime(name).unwrap_or(content_type)
        } else {
            content_type
        };

        // Images — resize and base64 encode
        if crate::util::is_image(mime, name) {
            return match crate::util::resize_image(&data, mime, crate::util::MCP_IMAGE_MAX_BYTES) {
                Ok((processed_data, _mime_type)) => {
                    let base64_data = base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &processed_data,
                    );
                    Ok(GqlAttachmentContent {
                        size: processed_data.len(),
                        base64_content: Some(base64_data),
                        text_content: None,
                        info: None,
                    })
                }
                Err(e) => Err(async_graphql::Error::new(format!(
                    "Failed to process image: {e}"
                ))),
            };
        }

        // Documents — extract text
        match crate::util::extract_text(&data, name).await {
            Ok(Some(text)) => {
                return Ok(GqlAttachmentContent {
                    size: data.len(),
                    base64_content: None,
                    text_content: Some(text),
                    info: None,
                });
            }
            Ok(None) => {}
            Err(e) => {
                return Err(async_graphql::Error::new(format!(
                    "Failed to extract text: {e}"
                )));
            }
        }

        // Binary fallback
        Ok(GqlAttachmentContent {
            size: data.len(),
            base64_content: None,
            text_content: None,
            info: Some("Binary attachment — cannot be displayed directly.".to_string()),
        })
    }
}

#[derive(SimpleObject)]
#[graphql(name = "AttachmentContent")]
pub struct GqlAttachmentContent {
    pub size: usize,
    /// For images: base64-encoded image data
    pub base64_content: Option<String>,
    /// For documents: extracted text content
    pub text_content: Option<String>,
    /// Description when content can't be returned directly
    pub info: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "Identity")]
pub struct GqlIdentity {
    pub id: String,
    pub name: String,
    pub email: String,
    pub may_delete: bool,
    pub text_signature: Option<String>,
    pub html_signature: Option<String>,
    pub reply_to: Vec<GqlEmailAddress>,
    pub bcc: Vec<GqlEmailAddress>,
}

impl From<Identity> for GqlIdentity {
    fn from(i: Identity) -> Self {
        Self {
            id: i.id,
            name: i.name,
            email: i.email,
            may_delete: i.may_delete,
            text_signature: i.text_signature,
            html_signature: i.html_signature,
            reply_to: convert_addrs(i.reply_to),
            bcc: convert_addrs(i.bcc),
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "MaskedEmail")]
pub struct GqlMaskedEmail {
    pub id: String,
    pub email: String,
    pub state: Option<String>,
    pub for_domain: Option<String>,
    pub description: Option<String>,
    pub last_message_at: Option<String>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
    pub url: Option<String>,
}

impl From<MaskedEmail> for GqlMaskedEmail {
    fn from(m: MaskedEmail) -> Self {
        Self {
            id: m.id,
            email: m.email,
            state: m.state,
            for_domain: m.for_domain,
            description: m.description,
            last_message_at: m.last_message_at,
            created_at: m.created_at,
            created_by: m.created_by,
            url: m.url,
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "ContactEmail")]
pub struct GqlContactEmail {
    pub email: String,
    pub label: Option<String>,
}

impl From<ContactEmail> for GqlContactEmail {
    fn from(e: ContactEmail) -> Self {
        Self {
            email: e.email,
            label: e.label,
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "ContactPhone")]
pub struct GqlContactPhone {
    pub number: String,
    pub label: Option<String>,
}

impl From<ContactPhone> for GqlContactPhone {
    fn from(p: ContactPhone) -> Self {
        Self {
            number: p.number,
            label: p.label,
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "Contact")]
pub struct GqlContact {
    pub id: String,
    pub name: String,
    pub emails: Vec<GqlContactEmail>,
    pub phones: Vec<GqlContactPhone>,
    pub organization: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
}

impl From<Contact> for GqlContact {
    fn from(c: Contact) -> Self {
        Self {
            id: c.id,
            name: c.name,
            emails: c.emails.into_iter().map(GqlContactEmail::from).collect(),
            phones: c.phones.into_iter().map(GqlContactPhone::from).collect(),
            organization: c.organization,
            title: c.title,
            notes: c.notes,
        }
    }
}

// ============ Enums ============

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum SendAction {
    /// Preview the email before sending — ALWAYS do this first
    Preview,
    /// Send the email (requires prior preview)
    Confirm,
    /// Save as draft without sending
    Draft,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum SpamAction {
    /// Preview what will happen
    Preview,
    /// Confirm marking as spam
    Confirm,
}

// ============ Result Types ============

#[derive(SimpleObject)]
#[graphql(name = "ComposeResult")]
pub struct GqlComposeResult {
    /// Whether the operation succeeded
    pub success: bool,
    /// The email ID (for confirm/draft actions)
    pub email_id: Option<String>,
    /// Preview text (for preview action)
    pub preview: Option<String>,
    /// Confirmation token — returned by PREVIEW, required by CONFIRM/DRAFT
    pub confirmation_token: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
}

/// Server-side store of issued but unused confirmation nonces.
///
/// PREVIEW issues a random UUID paired with a fingerprint of the compose
/// params. CONFIRM/DRAFT must supply a nonce that's still in the store and
/// whose stored fingerprint matches the current params — this prevents
/// skipping PREVIEW and prevents reusing a nonce for different params.
pub type NonceStore = tokio::sync::Mutex<std::collections::HashMap<String, String>>;

/// Fingerprint the compose params so we can detect param tampering between
/// PREVIEW and CONFIRM. This is a non-cryptographic hash — it only needs to
/// detect accidental drift, not defeat an attacker who already controls the
/// process.
pub fn params_fingerprint(parts: &[&str]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Issue a new one-shot confirmation nonce for the given params.
pub async fn issue_nonce(store: &NonceStore, parts: &[&str]) -> String {
    let nonce = uuid::Uuid::new_v4().to_string();
    let fingerprint = params_fingerprint(parts);
    store.lock().await.insert(nonce.clone(), fingerprint);
    nonce
}

/// Consume a nonce, returning Ok(()) if it was issued for the given params.
/// The nonce is always removed on consumption, even on mismatch, so a bad
/// CONFIRM forces the caller back to PREVIEW.
pub async fn consume_nonce(
    store: &NonceStore,
    nonce: Option<&str>,
    parts: &[&str],
) -> std::result::Result<(), &'static str> {
    let nonce =
        nonce.ok_or("Missing confirmation_token. Use action=PREVIEW first to obtain one.")?;
    let stored = store
        .lock()
        .await
        .remove(nonce)
        .ok_or("Invalid or already-used confirmation_token. Re-run PREVIEW.")?;
    if stored != params_fingerprint(parts) {
        return Err("Params changed between PREVIEW and CONFIRM. Re-run PREVIEW.");
    }
    Ok(())
}

#[derive(SimpleObject)]
#[graphql(name = "Status")]
pub struct GqlStatus {
    pub success: bool,
    pub message: Option<String>,
    pub error: Option<String>,
}

/// Thread result containing all emails in a conversation with full content.
/// Each email has lazy attachment content resolution — only fetched when queried.
pub struct GqlThread {
    pub emails: Vec<Email>,
    pub total: usize,
}

#[Object(name = "Thread")]
impl GqlThread {
    async fn emails(&self) -> Vec<GqlEmail> {
        self.emails.iter().cloned().map(GqlEmail).collect()
    }
    async fn total(&self) -> usize {
        self.total
    }
}

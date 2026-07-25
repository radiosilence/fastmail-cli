//! GraphQL type wrappers around existing model structs
//!
//! # Lazy resolution
//!
//! The object graph is fully navigable: mailboxes → emails → bodies →
//! attachments → content, plus emails → thread → emails and emails → mailboxes.
//! Nothing below the top-level list is fetched eagerly. A field that needs data
//! the parent doesn't already hold goes through a DataLoader (see
//! [`super::loaders`]), so sibling fields and list elements collapse into one
//! batched API call instead of N sequential ones.

use std::borrow::Cow;

use async_graphql::{Context, Enum, Object, Result, SimpleObject};

use super::connection::{EmailConnection, PageArgs, emails_connection, page_complexity};
use super::filter::{EmailFilter, EmailSort};
use super::loaders::{Blobs, Emails, Mailboxes, Threads, to_gql_error};
use crate::carddav::{Contact, ContactEmail, ContactPhone};
use crate::models::{Email, EmailAddress, Identity, Mailbox, MaskedEmail};

/// Page size used when a connection field names no `first`/`last`.
pub(crate) const DEFAULT_PAGE: u32 = 25;
/// Hard cap on a single page, and the figure nested lists are costed at.
pub(crate) const MAX_PAGE: u32 = 100;

pub(crate) fn clamp_page(size: Option<u32>) -> u32 {
    size.unwrap_or(DEFAULT_PAGE).min(MAX_PAGE)
}

// ============ Output Types ============

/// The account's mailboxes, through the loader. Every mailbox question is
/// answered by filtering this one list, so a query touching mailboxes at ten
/// different points still costs a single `Mailbox/get`.
pub(crate) async fn all_mailboxes(ctx: &Context<'_>) -> Result<std::sync::Arc<Vec<Mailbox>>> {
    ctx.data::<Mailboxes>()?
        .load_one(())
        .await
        .map_err(to_gql_error)?
        .ok_or_else(|| async_graphql::Error::new("Mailbox list unavailable"))
}

/// Find a mailbox by name, falling back to matching its role. Case-insensitive
/// both ways, so `"INBOX"`, `"Inbox"` and `"inbox"` all land on the same folder.
pub(crate) fn find_by_name_or_role<'a>(
    mailboxes: &'a [Mailbox],
    name: &str,
) -> Option<&'a Mailbox> {
    let wanted = name.to_lowercase();
    mailboxes
        .iter()
        .find(|m| m.name.to_lowercase() == wanted)
        .or_else(|| {
            mailboxes
                .iter()
                .find(|m| m.role.as_deref().map(str::to_lowercase).as_deref() == Some(&wanted))
        })
}

/// A mailbox (folder). Navigate into its contents with `emails`, or around the
/// folder tree with `parent` / `children`.
pub struct GqlMailbox(pub Mailbox);

impl From<Mailbox> for GqlMailbox {
    fn from(m: Mailbox) -> Self {
        Self(m)
    }
}

#[Object(name = "Mailbox")]
#[allow(clippy::too_many_arguments)]
impl GqlMailbox {
    async fn id(&self) -> &str {
        &self.0.id
    }
    async fn name(&self) -> &str {
        &self.0.name
    }
    async fn parent_id(&self) -> Option<&str> {
        self.0.parent_id.as_deref()
    }
    async fn role(&self) -> Option<&str> {
        self.0.role.as_deref()
    }
    async fn total_emails(&self) -> u32 {
        self.0.total_emails
    }
    async fn unread_emails(&self) -> u32 {
        self.0.unread_emails
    }
    async fn total_threads(&self) -> u32 {
        self.0.total_threads
    }
    async fn unread_threads(&self) -> u32 {
        self.0.unread_threads
    }
    async fn sort_order(&self) -> u32 {
        self.0.sort_order
    }

    /// The mailbox this one is nested under, if any.
    async fn parent(&self, ctx: &Context<'_>) -> Result<Option<GqlMailbox>> {
        let Some(parent_id) = self.0.parent_id.as_deref() else {
            return Ok(None);
        };
        Ok(all_mailboxes(ctx)
            .await?
            .iter()
            .find(|m| m.id == parent_id)
            .cloned()
            .map(GqlMailbox::from))
    }

    /// Mailboxes nested directly under this one.
    async fn children(&self, ctx: &Context<'_>) -> Result<Vec<GqlMailbox>> {
        Ok(all_mailboxes(ctx)
            .await?
            .iter()
            .filter(|m| m.parent_id.as_deref() == Some(self.0.id.as_str()))
            .cloned()
            .map(GqlMailbox::from)
            .collect())
    }

    /// Emails in this mailbox, newest first by default.
    ///
    /// Accepts the same `filter` and `sort` as the top-level `emails` query,
    /// implicitly scoped to this mailbox. Bodies and attachments on the results
    /// are resolved lazily and in one batch.
    #[graphql(complexity = "page_complexity(first, last, child_complexity)")]
    async fn emails(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Narrow the results. Combined with this mailbox via AND.")] filter: Option<
            EmailFilter,
        >,
        #[graphql(desc = "Sort order, most significant first. Defaults to newest first.")]
        sort: Option<Vec<EmailSort>>,
        #[graphql(desc = "Return one email per conversation instead of every message.")]
        collapse_threads: Option<bool>,
        after: Option<String>,
        before: Option<String>,
        first: Option<i32>,
        last: Option<i32>,
    ) -> Result<EmailConnection> {
        emails_connection(
            ctx,
            PageArgs {
                after,
                before,
                first,
                last,
            },
            filter,
            sort,
            Some(serde_json::json!({ "inMailbox": self.0.id })),
            collapse_threads.unwrap_or(false),
        )
        .await
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

/// Build the attachment list for an email that has already been fully fetched.
pub(crate) fn attachments_of(email: &Email) -> Vec<GqlAttachment> {
    email
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
                    cid: a.cid.clone(),
                    charset: a.charset.clone(),
                    part_id: a.part_id.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An email.
///
/// Lists (`emails`, `searchEmails`, `Mailbox.emails`) return these carrying only
/// the cheap header fields. Selecting `textBody`, `htmlBody`, `attachments` or
/// the threading headers triggers a batched fetch of the full record for every
/// email in the list at once — one extra API call, not one per email.
pub struct GqlEmail {
    inner: Email,
    /// Whether `inner` came from a full fetch (bodies + attachment metadata),
    /// as opposed to a list/search result carrying headers only.
    complete: bool,
}

impl GqlEmail {
    /// Wrap an email fetched with the full property set.
    pub fn full(email: Email) -> Self {
        Self {
            inner: email,
            complete: true,
        }
    }

    /// Wrap a list/search result. Body and attachment fields on it resolve lazily.
    pub fn summary(email: Email) -> Self {
        Self {
            inner: email,
            complete: false,
        }
    }

    /// The full record: borrowed when we already have it, otherwise batch-loaded.
    async fn detail<'a>(&'a self, ctx: &Context<'_>) -> Result<Cow<'a, Email>> {
        if self.complete {
            return Ok(Cow::Borrowed(&self.inner));
        }
        let loader = ctx.data::<Emails>()?;
        let full = loader
            .load_one(self.inner.id.clone())
            .await
            .map_err(to_gql_error)?
            .ok_or_else(|| {
                async_graphql::Error::new(format!("Email {} no longer exists", self.inner.id))
            })?;
        Ok(Cow::Owned(full))
    }
}

#[Object(name = "Email")]
impl GqlEmail {
    async fn id(&self) -> &str {
        &self.inner.id
    }
    async fn blob_id(&self) -> Option<&str> {
        self.inner.blob_id.as_deref()
    }
    async fn thread_id(&self) -> Option<&str> {
        self.inner.thread_id.as_deref()
    }
    async fn subject(&self) -> Option<&str> {
        self.inner.subject.as_deref()
    }
    async fn from(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.inner.from.clone())
    }
    async fn to(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.inner.to.clone())
    }
    async fn cc(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.inner.cc.clone())
    }
    async fn bcc(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.inner.bcc.clone())
    }
    async fn reply_to(&self) -> Vec<GqlEmailAddress> {
        convert_addrs(self.inner.reply_to.clone())
    }
    async fn received_at(&self) -> Option<&str> {
        self.inner.received_at.as_deref()
    }
    async fn sent_at(&self) -> Option<&str> {
        self.inner.sent_at.as_deref()
    }
    async fn preview(&self) -> Option<&str> {
        self.inner.preview.as_deref()
    }
    async fn has_attachment(&self) -> bool {
        self.inner.has_attachment
    }
    async fn is_unread(&self) -> bool {
        self.inner.is_unread()
    }
    async fn is_flagged(&self) -> bool {
        self.inner.is_flagged()
    }
    async fn is_draft(&self) -> bool {
        self.inner.is_draft()
    }
    async fn size(&self) -> u64 {
        self.inner.size
    }

    /// RFC 5322 Message-ID header. Lazily fetched.
    async fn message_id(&self, ctx: &Context<'_>) -> Result<Option<Vec<String>>> {
        Ok(self.detail(ctx).await?.message_id.clone())
    }
    /// RFC 5322 In-Reply-To header. Lazily fetched.
    async fn in_reply_to(&self, ctx: &Context<'_>) -> Result<Option<Vec<String>>> {
        Ok(self.detail(ctx).await?.in_reply_to.clone())
    }
    /// RFC 5322 References header. Lazily fetched.
    async fn references(&self, ctx: &Context<'_>) -> Result<Option<Vec<String>>> {
        Ok(self.detail(ctx).await?.references.clone())
    }

    /// Plain text body content. Lazily fetched — selecting it on a list of
    /// emails costs one batched call for the whole list.
    async fn text_body(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let email = self.detail(ctx).await?;
        Ok(email.text_content().map(str::to_owned))
    }

    /// HTML body content. Lazily fetched — selecting it on a list of emails
    /// costs one batched call for the whole list.
    async fn html_body(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let email = self.detail(ctx).await?;
        Ok(email.html_content().map(str::to_owned))
    }

    /// Attachments with metadata. Lazily fetched. Select `content` on an
    /// attachment to also download its data (images are base64-encoded,
    /// documents have text extracted).
    #[graphql(complexity = "5 * child_complexity")]
    async fn attachments(&self, ctx: &Context<'_>) -> Result<Vec<GqlAttachment>> {
        // Emails flagged as having no attachment need no fetch at all.
        if !self.inner.has_attachment && !self.complete {
            return Ok(Vec::new());
        }
        let email = self.detail(ctx).await?;
        Ok(attachments_of(&email))
    }

    /// Mailbox IDs this email belongs to
    async fn mailbox_ids(&self) -> Vec<String> {
        self.inner.mailbox_ids.keys().cloned().collect()
    }

    /// The mailboxes this email belongs to, resolved from `mailboxIds`.
    async fn mailboxes(&self, ctx: &Context<'_>) -> Result<Vec<GqlMailbox>> {
        let all = all_mailboxes(ctx).await?;
        let mut mailboxes: Vec<Mailbox> = all
            .iter()
            .filter(|m| self.inner.mailbox_ids.contains_key(&m.id))
            .cloned()
            .collect();
        mailboxes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(mailboxes.into_iter().map(GqlMailbox::from).collect())
    }

    /// Keywords (flags) on this email: $seen, $flagged, $draft, $junk, etc.
    async fn keywords(&self) -> Vec<String> {
        self.inner.keywords.keys().cloned().collect()
    }

    /// The conversation this email belongs to, with every message in it.
    #[graphql(complexity = "10 * child_complexity")]
    async fn thread(&self, ctx: &Context<'_>) -> Result<Option<GqlThread>> {
        let Some(thread_id) = self.inner.thread_id.clone() else {
            return Ok(None);
        };
        GqlThread::load(ctx, thread_id).await.map(Some)
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
    pub cid: Option<String>,
    pub charset: Option<String>,
    pub part_id: Option<String>,
}

impl GqlAttachment {
    /// The blob's bytes, through the loader so sibling fields on this
    /// attachment — and every other attachment resolving at the same time —
    /// share one batch of downloads rather than one each.
    async fn bytes(&self, ctx: &Context<'_>) -> Result<std::sync::Arc<Vec<u8>>> {
        ctx.data::<Blobs>()?
            .load_one(self.blob_id.clone())
            .await
            .map_err(to_gql_error)?
            .ok_or_else(|| async_graphql::Error::new(format!("Blob {} not found", self.blob_id)))
    }
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
    /// Content-ID. Inline images are referenced from the HTML body as
    /// `<img src="cid:...">`, so this is what ties an attachment to where it
    /// appears in `htmlBody`.
    async fn cid(&self) -> Option<&str> {
        self.cid.as_deref()
    }
    /// Character set, for decoding text attachments.
    async fn charset(&self) -> Option<&str> {
        self.charset.as_deref()
    }
    /// This part's identifier within the message.
    async fn part_id(&self) -> Option<&str> {
        self.part_id.as_deref()
    }
    /// The raw bytes, base64-encoded. Downloads the blob and does nothing else,
    /// so it works for any attachment whatever its type.
    #[graphql(complexity = "10 + child_complexity")]
    async fn base64(&self, ctx: &Context<'_>) -> Result<String> {
        let data = self.bytes(ctx).await?;
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            data.as_slice(),
        ))
    }

    /// The image resized to fit `maxBytes`, then base64-encoded. Null when this
    /// attachment is not an image — use `base64` for the untouched bytes.
    ///
    /// Resizing exists so a model isn't handed a 10MB photo; it costs a decode
    /// and re-encode, so it is priced above a plain download.
    #[graphql(complexity = "20 + child_complexity")]
    async fn image(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Shrink the image until it fits this many bytes.")] max_bytes: Option<u32>,
    ) -> Result<Option<String>> {
        let name = self.name.as_deref().unwrap_or("attachment");
        let declared = self
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");
        if !crate::util::is_image(declared, name) {
            return Ok(None);
        }
        let mime = crate::util::infer_image_mime(name).unwrap_or(declared);

        let data = self.bytes(ctx).await?;
        let limit = max_bytes.map_or(crate::util::MCP_IMAGE_MAX_BYTES, |b| b as usize);
        let (resized, _mime) = crate::util::resize_image(&data, mime, limit)
            .map_err(|e| async_graphql::Error::new(format!("Failed to process image: {e}")))?;
        Ok(Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &resized,
        )))
    }

    /// Text extracted from a document (PDF, DOCX, XLSX, …). Null when nothing
    /// can be extracted — an image, or a format with no text in it.
    ///
    /// **This is the expensive field.** Extraction parses the whole document,
    /// so it is priced well above the download and should only be selected when
    /// the text is actually wanted. Nothing else on `Attachment` triggers it.
    #[graphql(complexity = "50 + child_complexity")]
    async fn text(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let name = self.name.as_deref().unwrap_or("attachment");
        let data = self.bytes(ctx).await?;
        crate::util::extract_text(&data, name)
            .await
            .map_err(|e| async_graphql::Error::new(format!("Failed to extract text: {e}")))
    }
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

/// A pending confirmation nonce: fingerprint of the compose params, plus
/// the monotonic instant it was issued at so we can expire it.
pub struct Nonce {
    pub fingerprint: String,
    pub issued_at: std::time::Instant,
}

/// Server-side store of issued but unused confirmation nonces.
///
/// PREVIEW issues a random UUID paired with a fingerprint of the compose
/// params. CONFIRM/DRAFT must supply a nonce that's still in the store and
/// whose stored fingerprint matches the current params — this prevents
/// skipping PREVIEW and prevents reusing a nonce for different params.
///
/// The store is bounded by `NONCE_CAP` (oldest entries evicted) and entries
/// older than `NONCE_TTL` are swept on every issue and treated as invalid
/// on consume, so a misbehaving client that previews without confirming
/// cannot grow the map without limit.
pub type NonceStore = tokio::sync::Mutex<std::collections::HashMap<String, Nonce>>;

/// Hard cap on outstanding nonces. A compose preview without a confirm is
/// user intent — capacity for hundreds of drafts is plenty for a single
/// MCP session.
const NONCE_CAP: usize = 256;

/// How long a PREVIEW'd nonce stays valid. Chosen to outlive a human writing
/// an email but expire well before the MCP process restarts.
const NONCE_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Drop entries older than TTL, then if still over cap drop the oldest.
fn evict(map: &mut std::collections::HashMap<String, Nonce>) {
    let now = std::time::Instant::now();
    map.retain(|_, n| now.duration_since(n.issued_at) < NONCE_TTL);
    while map.len() >= NONCE_CAP {
        let oldest = map
            .iter()
            .min_by_key(|(_, n)| n.issued_at)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                map.remove(&k);
            }
            None => break,
        }
    }
}

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
    let entry = Nonce {
        fingerprint: params_fingerprint(parts),
        issued_at: std::time::Instant::now(),
    };
    let mut map = store.lock().await;
    evict(&mut map);
    map.insert(nonce.clone(), entry);
    nonce
}

/// Consume a nonce, returning Ok(()) if it was issued for the given params.
/// The nonce is always removed on consumption, even on mismatch or expiry,
/// so a bad CONFIRM forces the caller back to PREVIEW.
pub async fn consume_nonce(
    store: &NonceStore,
    nonce: Option<&str>,
    parts: &[&str],
) -> std::result::Result<(), &'static str> {
    let nonce =
        nonce.ok_or("Missing confirmation_token. Use action=PREVIEW first to obtain one.")?;
    let entry = store
        .lock()
        .await
        .remove(nonce)
        .ok_or("Invalid or already-used confirmation_token. Re-run PREVIEW.")?;
    if std::time::Instant::now().duration_since(entry.issued_at) >= NONCE_TTL {
        return Err("confirmation_token expired. Re-run PREVIEW.");
    }
    if entry.fingerprint != params_fingerprint(parts) {
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

/// A conversation: every email sharing a thread ID, oldest first. Each email is
/// fully fetched, so bodies and attachment metadata are already present.
pub struct GqlThread {
    pub id: Option<String>,
    pub emails: Vec<Email>,
}

impl GqlThread {
    /// Load a thread through the loaders: one batched `Thread/get` for the
    /// member IDs, then one batched `Email/get` for their content.
    pub async fn load(ctx: &Context<'_>, thread_id: String) -> Result<Self> {
        let threads = ctx.data::<Threads>()?;
        let emails = ctx.data::<Emails>()?;

        let ids = threads
            .load_one(thread_id.clone())
            .await
            .map_err(to_gql_error)?
            .unwrap_or_default();

        let loaded = emails.load_many(ids).await.map_err(to_gql_error)?;
        let mut emails: Vec<Email> = loaded.into_values().collect();
        emails.sort_by(|a, b| a.received_at.cmp(&b.received_at));

        Ok(Self {
            id: Some(thread_id),
            emails,
        })
    }
}

#[Object(name = "Thread")]
impl GqlThread {
    async fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    async fn emails(&self) -> Vec<GqlEmail> {
        self.emails.iter().cloned().map(GqlEmail::full).collect()
    }
    async fn total(&self) -> usize {
        self.emails.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    fn fresh_store() -> NonceStore {
        Mutex::new(std::collections::HashMap::new())
    }

    #[tokio::test]
    async fn issue_then_consume_with_matching_params_ok() {
        let store = fresh_store();
        let parts = ["to@example.com", "subject", "body"];
        let nonce = issue_nonce(&store, &parts).await;
        assert!(consume_nonce(&store, Some(&nonce), &parts).await.is_ok());
    }

    #[tokio::test]
    async fn consume_rejects_missing_nonce() {
        let store = fresh_store();
        let err = consume_nonce(&store, None, &["x"]).await.unwrap_err();
        assert!(err.contains("Missing"));
    }

    #[tokio::test]
    async fn consume_rejects_reused_nonce() {
        let store = fresh_store();
        let parts = ["x"];
        let nonce = issue_nonce(&store, &parts).await;
        consume_nonce(&store, Some(&nonce), &parts).await.unwrap();
        let err = consume_nonce(&store, Some(&nonce), &parts)
            .await
            .unwrap_err();
        assert!(err.contains("Invalid or already-used"));
    }

    #[tokio::test]
    async fn consume_rejects_tampered_params() {
        let store = fresh_store();
        let nonce = issue_nonce(&store, &["preview"]).await;
        let err = consume_nonce(&store, Some(&nonce), &["tampered"])
            .await
            .unwrap_err();
        assert!(err.contains("Params changed"));
    }

    #[tokio::test]
    async fn consume_rejects_expired_nonce() {
        let store = fresh_store();
        let parts = ["x"];
        let nonce = uuid::Uuid::new_v4().to_string();
        store.lock().await.insert(
            nonce.clone(),
            Nonce {
                fingerprint: params_fingerprint(&parts),
                issued_at: std::time::Instant::now()
                    - NONCE_TTL
                    - std::time::Duration::from_secs(1),
            },
        );
        let err = consume_nonce(&store, Some(&nonce), &parts)
            .await
            .unwrap_err();
        assert!(err.contains("expired"));
    }

    #[tokio::test]
    async fn issue_evicts_oldest_when_at_cap() {
        let store = fresh_store();
        let base = std::time::Instant::now() - std::time::Duration::from_secs(60);
        let oldest_nonce = "oldest".to_string();
        {
            let mut map = store.lock().await;
            map.insert(
                oldest_nonce.clone(),
                Nonce {
                    fingerprint: params_fingerprint(&["oldest"]),
                    issued_at: base,
                },
            );
            for i in 1..NONCE_CAP {
                map.insert(
                    format!("n{i}"),
                    Nonce {
                        fingerprint: params_fingerprint(&["x"]),
                        issued_at: base + std::time::Duration::from_millis(i as u64),
                    },
                );
            }
            assert_eq!(map.len(), NONCE_CAP);
        }
        let new_nonce = issue_nonce(&store, &["new"]).await;
        let map = store.lock().await;
        assert_eq!(map.len(), NONCE_CAP);
        assert!(!map.contains_key(&oldest_nonce), "oldest should be evicted");
        assert!(map.contains_key(&new_nonce), "new nonce should be present");
    }

    #[tokio::test]
    async fn issue_sweeps_expired_entries() {
        let store = fresh_store();
        let expired = "expired".to_string();
        store.lock().await.insert(
            expired.clone(),
            Nonce {
                fingerprint: params_fingerprint(&["old"]),
                issued_at: std::time::Instant::now()
                    - NONCE_TTL
                    - std::time::Duration::from_secs(1),
            },
        );
        issue_nonce(&store, &["new"]).await;
        assert!(!store.lock().await.contains_key(&expired));
    }
}

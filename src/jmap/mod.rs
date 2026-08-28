mod events;
mod watch;

use crate::commands::SearchFilter;
use crate::error::{Error, Result};
use crate::models::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, instrument};

pub use events::{EventParser, ServerEvent};
pub use watch::{ArrivalWatcher, Arrivals, SharedJmapClient};

const SESSION_URL: &str = "https://api.fastmail.com/jmap/session";
const TIMEOUT: Duration = Duration::from_secs(30);

/// Properties fetched for list/search results: everything that is cheap to
/// serialise. Bodies and attachment metadata are deliberately excluded — those
/// are pulled on demand, in one batched call, by [`JmapClient::get_emails`].
pub const EMAIL_SUMMARY_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "sentAt",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "preview",
    "hasAttachment",
];

/// The summary set plus bodies, attachment metadata and threading headers —
/// the expensive fetch.
pub const EMAIL_FULL_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "sentAt",
    "messageId",
    "inReplyTo",
    "references",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "preview",
    "hasAttachment",
    "textBody",
    "htmlBody",
    "attachments",
    "bodyValues",
    "headers",
];

const DESIRED_CAPABILITIES: &[&str] = &[
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "https://www.fastmail.com/dev/maskedemail",
];

pub struct JmapClient {
    client: Client,
    token: String,
    session_url: String,
    session: Option<Session>,
    available_capabilities: Vec<String>,
    cached_mailboxes: Option<Vec<Mailbox>>,
}

/// Create an authenticated JMAP client from config
pub async fn authenticated_client() -> crate::error::Result<JmapClient> {
    let config = crate::config::Config::load()?;
    let token = config.get_token()?;
    let mut client = JmapClient::new(token);
    client.authenticate().await?;
    Ok(client)
}

/// File attachment data ready for upload
pub struct AttachmentData {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// Common parameters for compose operations (send, reply, forward)
pub struct ComposeParams<'a> {
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub from: Option<&'a str>,
    pub draft: bool,
    pub html_body: Option<String>,
    pub attachments: Vec<AttachmentData>,
}

/// Threading headers for reply/forward
struct ThreadingHeaders {
    in_reply_to: Vec<String>,
    references: Vec<String>,
}

/// Bundled content for the create_and_submit_email helper
struct EmailDraft<'a> {
    to: &'a [EmailAddress],
    cc: &'a [EmailAddress],
    bcc: &'a [EmailAddress],
    subject: &'a str,
    body: &'a str,
    html_body: Option<&'a str>,
    attachments: Vec<AttachmentData>,
    threading: Option<ThreadingHeaders>,
}

/// An attachment after blob upload — holds the server-assigned blobId.
#[derive(Debug)]
struct UploadedAttachment {
    blob_id: String,
    filename: String,
    content_type: String,
}

/// Build bodyValues and body structure fields on `email_create`.
///
/// Handles three JMAP body modes:
/// - Plain text only → `textBody` array
/// - Text + HTML (no attachments) → `textBody` + `htmlBody` arrays
/// - With attachments → explicit `bodyStructure` MIME tree
fn apply_body_structure(
    email_create: &mut HashMap<String, Value>,
    text_body: &str,
    html_body: Option<&str>,
    attachments: &[UploadedAttachment],
) {
    let mut body_values = json!({
        "textBody": { "value": text_body, "charset": "utf-8" }
    });
    if let Some(html) = html_body {
        body_values["htmlBody"] = json!({ "value": html, "charset": "utf-8" });
    }
    email_create.insert("bodyValues".into(), body_values);

    let has_html = html_body.is_some();
    let has_attachments = !attachments.is_empty();

    if has_attachments {
        let text_part = json!({ "partId": "textBody", "type": "text/plain" });
        let content_part = if has_html {
            let html_part = json!({ "partId": "htmlBody", "type": "text/html" });
            json!({ "type": "multipart/alternative", "subParts": [text_part, html_part] })
        } else {
            text_part
        };

        let mut sub_parts = vec![content_part];
        for att in attachments {
            sub_parts.push(json!({
                "blobId": att.blob_id,
                "name": att.filename,
                "type": att.content_type,
                "disposition": "attachment"
            }));
        }

        email_create.insert(
            "bodyStructure".into(),
            json!({ "type": "multipart/mixed", "subParts": sub_parts }),
        );
    } else if has_html {
        email_create.insert(
            "textBody".into(),
            json!([{ "partId": "textBody", "type": "text/plain" }]),
        );
        email_create.insert(
            "htmlBody".into(),
            json!([{ "partId": "htmlBody", "type": "text/html" }]),
        );
    } else {
        email_create.insert(
            "textBody".into(),
            json!([{ "partId": "textBody", "type": "text/plain" }]),
        );
    }
}

/// Resolved context for a compose operation
struct ComposeContext {
    account_id: String,
    mailbox: Mailbox,
    identity: Option<Identity>,
    draft: bool,
}

impl ComposeContext {
    fn apply_to_email(&self, email_create: &mut HashMap<String, Value>) {
        email_create.insert(
            "mailboxIds".into(),
            json!({ self.mailbox.id.clone(): true }),
        );
        if self.draft {
            email_create.insert("keywords".into(), json!({ "$draft": true, "$seen": true }));
        }
        if let Some(ref identity) = self.identity {
            email_create.insert(
                "from".into(),
                json!([{ "email": identity.email, "name": identity.name }]),
            );
        }
    }

    fn build_method_calls(&self, email_create: HashMap<String, Value>) -> Vec<Value> {
        let mut calls = vec![json!([
            "Email/set",
            {
                "accountId": self.account_id,
                "create": { "email": email_create }
            },
            "e0"
        ])];
        if !self.draft
            && let Some(ref identity) = self.identity
        {
            calls.push(json!([
                "EmailSubmission/set",
                {
                    "accountId": self.account_id,
                    "create": {
                        "submission": {
                            "identityId": identity.id,
                            "emailId": "#email"
                        }
                    },
                    "onSuccessUpdateEmail": {
                        "#submission": {
                            "keywords/$seen": true
                        }
                    }
                },
                "s0"
            ]));
        }
        calls
    }
}

/// How many changes to ask for per `Email/changes` call. The server may cap it
/// lower; `hasMoreChanges` then drives the next page.
const CHANGES_PAGE: u32 = 100;

/// What arrived since a known `Email` state.
#[derive(Debug)]
pub struct EmailChanges {
    /// The state to pass as `sinceState` next time.
    pub new_state: String,
    /// IDs created in that window, oldest change first.
    pub created: Vec<String>,
}

// Shared JMAP response types used across multiple methods
#[derive(Deserialize)]
struct GetResponse<T> {
    list: Vec<T>,
}

#[derive(Deserialize)]
struct QueryResponse {
    ids: Vec<String>,
    #[serde(default)]
    position: u64,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default, rename = "queryState")]
    query_state: Option<String>,
}

/// Where a query window starts.
///
/// JMAP offers two ways to say this, and the anchor form is what makes stable
/// pagination possible: an index shifts whenever mail arrives, but an anchor is
/// an ID in the result set, so a cursor built from one still points at the same
/// message a minute later.
pub enum QueryStart {
    /// Zero-based index. Negative counts back from the end, so `-10` is "the
    /// last ten" without needing to know the total first.
    Position(i64),
    /// Start relative to a known ID: offset `1` is the item after it, `-n` the
    /// `n` items ending just before it.
    Anchor { id: String, offset: i64 },
}

impl Default for QueryStart {
    fn default() -> Self {
        Self::Position(0)
    }
}

/// Parameters for one `Email/query` window.
pub struct EmailQuery {
    /// A JMAP `FilterCondition` or `FilterOperator` object.
    pub filter: Value,
    /// JMAP sort comparators, most significant first.
    pub sort: Vec<Value>,
    /// Where the window begins.
    pub start: QueryStart,
    pub limit: u32,
    /// Ask the server for the total match count. Costs the server extra work,
    /// so only set it when the caller actually needs the number.
    pub calculate_total: bool,
    /// Return one email per conversation instead of every message.
    pub collapse_threads: bool,
    /// Also fetch summary records for the matched IDs, in query order.
    pub fetch_summaries: bool,
}

impl EmailQuery {
    /// Newest-first page of `limit` emails, with summaries and no total.
    pub fn first(limit: u32) -> Self {
        Self {
            filter: json!({}),
            sort: vec![json!({ "property": "receivedAt", "isAscending": false })],
            start: QueryStart::Position(0),
            limit,
            calculate_total: false,
            collapse_threads: false,
            fetch_summaries: true,
        }
    }
}

/// Normalise a date to ISO 8601, accepting a bare `YYYY-MM-DD`.
pub fn normalize_date(date: &str) -> String {
    if date.contains('T') {
        date.to_string()
    } else {
        format!("{date}T00:00:00Z")
    }
}

/// Build a flat JMAP `FilterCondition` from the CLI's [`SearchFilter`].
///
/// This is the flattened view the CLI needs. The GraphQL layer builds richer
/// `FilterOperator` trees directly — see `mcp::graphql::filter`.
pub fn search_filter_to_jmap(filter: &SearchFilter, mailbox_id: Option<&str>) -> Value {
    let mut f = json!({});

    let mut set = |key: &str, value: Value| {
        f[key] = value;
    };

    if let Some(ref text) = filter.text {
        set("text", json!(text));
    }
    if let Some(ref from) = filter.from {
        set("from", json!(from));
    }
    if let Some(ref to) = filter.to {
        set("to", json!(to));
    }
    if let Some(ref cc) = filter.cc {
        set("cc", json!(cc));
    }
    if let Some(ref bcc) = filter.bcc {
        set("bcc", json!(bcc));
    }
    if let Some(ref subject) = filter.subject {
        set("subject", json!(subject));
    }
    if let Some(ref body) = filter.body {
        set("body", json!(body));
    }
    if let Some(mailbox) = mailbox_id {
        set("inMailbox", json!(mailbox));
    }
    if filter.has_attachment {
        set("hasAttachment", json!(true));
    }
    if let Some(min_size) = filter.min_size {
        set("minSize", json!(min_size));
    }
    if let Some(max_size) = filter.max_size {
        set("maxSize", json!(max_size));
    }
    if let Some(ref before) = filter.before {
        set("before", json!(normalize_date(before)));
    }
    if let Some(ref after) = filter.after {
        set("after", json!(normalize_date(after)));
    }
    if filter.unread {
        set("notKeyword", json!("$seen"));
    }
    if filter.flagged {
        set("hasKeyword", json!("$flagged"));
    }

    f
}

/// One window of an `Email/query` result set.
pub struct EmailPage {
    /// Summary records in query order. Empty when summaries weren't requested.
    pub emails: Vec<Email>,
    /// The IDs this window matched, in order.
    pub ids: Vec<String>,
    /// Index of the first returned ID within the full result set.
    pub position: u64,
    /// Total matches, when `calculate_total` was set.
    pub total: Option<u64>,
    /// Opaque server state string for this query, letting a client tell whether
    /// the result set has changed since it last looked.
    pub query_state: Option<String>,
}

#[derive(Deserialize)]
struct EmailSetResponse {
    created: Option<HashMap<String, Value>>,
    #[serde(rename = "notCreated")]
    not_created: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
struct SetResponse {
    #[serde(rename = "notUpdated")]
    not_updated: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
struct MaskedEmailCreateResponse {
    created: Option<HashMap<String, MaskedEmail>>,
    #[serde(rename = "notCreated")]
    not_created: Option<HashMap<String, Value>>,
}

#[derive(Debug, Serialize)]
struct JmapRequest {
    using: Vec<String>,
    #[serde(rename = "methodCalls")]
    method_calls: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct JmapResponse {
    #[serde(rename = "methodResponses")]
    method_responses: Vec<Value>,
}

/// Substitute `{placeholder}` tokens in a URL template in a single pass.
///
/// Unlike chaining `str::replace`, this never re-scans an already-substituted
/// value, so a variable value that contains another template marker cannot
/// bleed into a later replacement.
fn apply_url_template(tmpl: &str, vars: &[(&str, &str)]) -> String {
    let mut result = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find('}') {
            let key = &after_open[..close];
            match vars.iter().find(|(k, _)| *k == key) {
                Some((_, v)) => result.push_str(v),
                None => {
                    // Unknown placeholder — preserve literally so a downstream
                    // system that recognises it still can.
                    result.push('{');
                    result.push_str(key);
                    result.push('}');
                }
            }
            rest = &after_open[close + 1..];
        } else {
            // Unterminated — emit the remainder verbatim and stop.
            result.push('{');
            result.push_str(after_open);
            return result;
        }
    }
    result.push_str(rest);
    result
}

fn pick_identity(identities: Vec<Identity>, from: Option<&str>) -> Result<Identity> {
    match from {
        Some(email) => identities
            .into_iter()
            .find(|i| i.email.eq_ignore_ascii_case(email))
            .ok_or_else(|| Error::IdentityNotFoundForEmail(email.to_string())),
        None => identities.into_iter().next().ok_or(Error::IdentityNotFound),
    }
}

/// Build reply To/CC lists from an original email, expanding reply-all if
/// requested and filtering out the sending identity.
///
/// Returns `(to, cc)` where:
/// - `to` starts with `original.reply_to` when the sender set one, otherwise
///   `original.from`. When `reply_all` is set, the original `To` recipients are
///   appended (minus `my_email` if provided).
/// - `cc` starts with the caller-supplied `extra_cc`. When `reply_all` is
///   set, the original `Cc` recipients are appended (minus `my_email`).
///
/// Both lists are deduplicated by lowercase email, and anything already in
/// `to` is stripped from `cc` — so an overlap between `extra_cc` and
/// reply-all-expanded `to` never produces a duplicate delivery.
///
/// `my_email` is optional: pass `None` for the preview path before an
/// identity has been resolved. In that case the "filter me out" step is
/// skipped, so the resulting preview may list the user's own address —
/// still a safer failure mode than the old preview, which silently
/// under-reported recipients.
pub fn expand_reply_recipients(
    original: &Email,
    reply_all: bool,
    my_email: Option<&str>,
    extra_cc: Vec<EmailAddress>,
) -> (Vec<EmailAddress>, Vec<EmailAddress>) {
    let me_lower = my_email.map(str::to_lowercase);
    let is_me = |addr: &EmailAddress| -> bool {
        me_lower
            .as_deref()
            .is_some_and(|m| addr.email.eq_ignore_ascii_case(m))
    };

    // Reply-To wins over From, as in any mail client. Transactional and support
    // senders routinely put a branded, undeliverable address in From and the
    // inbox that actually receives mail in Reply-To — replying to From then
    // bounces.
    let mut to_addrs: Vec<EmailAddress> = original
        .reply_to
        .clone()
        .filter(|addrs| !addrs.is_empty())
        .or_else(|| original.from.clone())
        .unwrap_or_default();
    if reply_all && let Some(ref orig_to) = original.to {
        for addr in orig_to {
            if !is_me(addr) {
                to_addrs.push(addr.clone());
            }
        }
    }

    let mut cc_addrs = extra_cc;
    if reply_all && let Some(ref orig_cc) = original.cc {
        for addr in orig_cc {
            if !is_me(addr) {
                cc_addrs.push(addr.clone());
            }
        }
    }

    dedup_by_email(&mut to_addrs);
    let to_lower: std::collections::HashSet<String> =
        to_addrs.iter().map(|a| a.email.to_lowercase()).collect();
    cc_addrs.retain(|c| !to_lower.contains(&c.email.to_lowercase()));
    dedup_by_email(&mut cc_addrs);

    (to_addrs, cc_addrs)
}

fn dedup_by_email(addrs: &mut Vec<EmailAddress>) {
    let mut seen = std::collections::HashSet::<String>::new();
    addrs.retain(|a| seen.insert(a.email.to_lowercase()));
}

impl JmapClient {
    pub fn new(token: String) -> Self {
        let client = Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            token,
            session_url: SESSION_URL.to_string(),
            session: None,
            available_capabilities: Vec::new(),
            cached_mailboxes: None,
        }
    }

    /// Build a client that is already "authenticated" against `api_url`, so
    /// tests can point it at a mock JMAP server without going through the
    /// real session endpoint. Re-authentication is redirected there too, so
    /// tests can exercise the handshake itself.
    #[cfg(test)]
    pub fn with_test_session(api_url: &str) -> Self {
        let mut client = Self::new("test-token".into());
        client.session_url = format!("{api_url}/session");
        client.available_capabilities =
            DESIRED_CAPABILITIES.iter().map(|s| s.to_string()).collect();
        client.session = Some(Session {
            capabilities: DESIRED_CAPABILITIES
                .iter()
                .map(|c| (c.to_string(), json!({})))
                .collect(),
            accounts: HashMap::new(),
            primary_accounts: HashMap::from([(
                "urn:ietf:params:jmap:mail".to_string(),
                "acct1".to_string(),
            )]),
            username: "test@example.com".into(),
            api_url: api_url.to_string(),
            download_url: format!("{api_url}/download/{{blobId}}"),
            upload_url: format!("{api_url}/upload"),
            event_source_url: None,
            state: None,
        });
        client
    }

    #[instrument(skip(self))]
    pub async fn authenticate(&mut self) -> Result<&Session> {
        debug!("Fetching JMAP session");
        let resp = self
            .client
            .get(&self.session_url)
            .bearer_auth(&self.token)
            .send()
            .await?;

        match resp.status().as_u16() {
            401 => return Err(Error::InvalidToken("Authentication failed")),
            429 => return Err(Error::RateLimited),
            500..=599 => return Err(Error::Server(format!("Server error: {}", resp.status()))),
            _ => {}
        }

        let session: Session = resp.json().await?;
        debug!(username = %session.username, "Session established");
        self.available_capabilities = DESIRED_CAPABILITIES
            .iter()
            .filter(|cap| session.capabilities.contains_key(**cap))
            .map(|s| s.to_string())
            .collect();
        self.session = Some(session);
        Ok(self.session.as_ref().unwrap())
    }

    pub fn session(&self) -> Result<&Session> {
        self.session.as_ref().ok_or(Error::NotAuthenticated)
    }

    fn account_id(&self) -> Result<&str> {
        self.session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))
    }

    fn require_capability(&self, capability: &str, action: &str) -> Result<()> {
        let session = self.session()?;

        if !session.capabilities.contains_key(capability) {
            return Err(Error::Config(format!(
                "{action} requires the '{capability}' capability. \
                Your API token may be read-only. Generate a new token with appropriate permissions \
                at Fastmail Settings > Privacy & Security > Integrations > API tokens."
            )));
        }
        Ok(())
    }

    #[instrument(skip(self, method_calls))]
    async fn request(&self, method_calls: Vec<Value>) -> Result<Vec<Value>> {
        let session = self.session()?;
        let req = JmapRequest {
            using: self.available_capabilities.clone(),
            method_calls,
        };

        debug!(url = %session.api_url, "Making JMAP request");
        let resp = self
            .client
            .post(&session.api_url)
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await?;

        match resp.status().as_u16() {
            401 => return Err(Error::InvalidToken("Token expired or invalid")),
            429 => return Err(Error::RateLimited),
            500..=599 => return Err(Error::Server(format!("Server error: {}", resp.status()))),
            _ => {}
        }

        let body = resp.text().await?;
        let jmap_resp: JmapResponse = serde_json::from_str(&body).map_err(|e| {
            debug!("Failed to parse JMAP response: {e}");
            Error::Server(body.trim().to_string())
        })?;
        Ok(jmap_resp.method_responses)
    }

    fn parse_response<T: for<'de> Deserialize<'de>>(
        response: &Value,
        expected_method: &str,
    ) -> Result<T> {
        let arr = response.as_array().ok_or_else(|| Error::Jmap {
            method: expected_method.into(),
            error_type: "parse".into(),
            description: "Response is not an array".into(),
        })?;

        let method_name = arr.first().and_then(|v: &Value| v.as_str()).unwrap_or("");

        if method_name == "error" {
            let error_obj = arr.get(1).unwrap_or(&Value::Null);
            let error_type = error_obj
                .get("type")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("unknown");
            let description = error_obj
                .get("description")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("No description");
            return Err(Error::Jmap {
                method: expected_method.into(),
                error_type: error_type.into(),
                description: description.into(),
            });
        }

        let data = arr.get(1).ok_or_else(|| Error::Jmap {
            method: expected_method.into(),
            error_type: "parse".into(),
            description: "Missing response data".into(),
        })?;

        serde_json::from_value(data.clone()).map_err(|e| Error::Jmap {
            method: expected_method.into(),
            error_type: "parse".into(),
            description: e.to_string(),
        })
    }

    /// Fetch the mailbox list from the server, always. Takes `&self` so callers
    /// holding a shared client can use it concurrently.
    ///
    /// This is the primitive the GraphQL mailbox loader wraps: the loader has a
    /// request-scoped cache of its own, and a cache *here* would outlive the
    /// request — clients are pooled per token for the life of the process, so a
    /// folder created after start-up would never appear.
    #[instrument(skip(self))]
    pub async fn fetch_mailboxes(&self) -> Result<Vec<Mailbox>> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Mailbox/get",
                {
                    "accountId": account_id,
                    "properties": [
                        "id", "name", "parentId", "role",
                        "totalEmails", "unreadEmails",
                        "totalThreads", "unreadThreads", "sortOrder",
                        "isSubscribed", "myRights"
                    ]
                },
                "m0"
            ])])
            .await?;

        let resp: GetResponse<Mailbox> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Mailbox/get")?;

        Ok(resp.list)
    }

    /// Mailbox list, memoised for the life of this client.
    ///
    /// For the CLI, where the process is short-lived and several commands walk
    /// the folder tree in one run. Long-lived callers want
    /// [`Self::fetch_mailboxes`] instead.
    #[instrument(skip(self))]
    pub async fn list_mailboxes(&mut self) -> Result<Vec<Mailbox>> {
        if let Some(ref cached) = self.cached_mailboxes {
            return Ok(cached.clone());
        }
        let mailboxes = self.fetch_mailboxes().await?;
        self.cached_mailboxes = Some(mailboxes.clone());
        Ok(mailboxes)
    }

    pub async fn find_mailbox(&mut self, name: &str) -> Result<Mailbox> {
        let mailboxes = self.list_mailboxes().await?;
        let name_lower = name.to_lowercase();

        if let Some(m) = mailboxes
            .iter()
            .find(|m| m.name.to_lowercase() == name_lower)
        {
            return Ok(m.clone());
        }

        if let Some(m) = mailboxes
            .iter()
            .find(|m| m.role.as_deref().map(|r: &str| r.to_lowercase()) == Some(name_lower.clone()))
        {
            return Ok(m.clone());
        }

        Err(Error::MailboxNotFound(name.into()))
    }

    /// One window of an `Email/query` result.
    #[instrument(skip(self, query))]
    pub async fn query_emails(&self, query: EmailQuery) -> Result<EmailPage> {
        let account_id = self.account_id()?;

        let mut args = json!({
            "accountId": account_id,
            "filter": query.filter,
            "sort": query.sort,
            "limit": query.limit,
            "calculateTotal": query.calculate_total,
            "collapseThreads": query.collapse_threads
        });
        match query.start {
            QueryStart::Position(p) => args["position"] = json!(p),
            QueryStart::Anchor { ref id, offset } => {
                args["anchor"] = json!(id);
                args["anchorOffset"] = json!(offset);
            }
        }

        let mut method_calls = vec![json!(["Email/query", args, "q0"])];

        // Chain the summary fetch off the query with a JMAP back-reference, so a
        // page costs one HTTP round trip rather than two. Skipped entirely when
        // the caller only wants counts.
        if query.fetch_summaries {
            method_calls.push(json!([
                "Email/get",
                {
                    "accountId": account_id,
                    "#ids": {
                        "resultOf": "q0",
                        "name": "Email/query",
                        "path": "/ids"
                    },
                    "properties": EMAIL_SUMMARY_PROPERTIES
                },
                "g0"
            ]));
        }

        let responses = self.request(method_calls).await?;

        let query_resp: QueryResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/query")?;

        let mut emails = Vec::new();
        if query.fetch_summaries {
            let get_resp: GetResponse<Email> =
                Self::parse_response(responses.get(1).unwrap_or(&Value::Null), "Email/get")?;
            // `Email/get` makes no ordering guarantee, so restore the sort order
            // the query asked for rather than trusting the response order.
            let mut by_id: HashMap<&str, Email> = get_resp
                .list
                .iter()
                .map(|e| (e.id.as_str(), e.clone()))
                .collect();
            emails = query_resp
                .ids
                .iter()
                .filter_map(|id| by_id.remove(id.as_str()))
                .collect();
        }

        Ok(EmailPage {
            emails,
            ids: query_resp.ids,
            position: query_resp.position,
            total: query_resp.total,
            query_state: query_resp.query_state,
        })
    }

    #[instrument(skip(self))]
    pub async fn list_emails(&self, mailbox_id: &str, limit: u32) -> Result<Vec<Email>> {
        let page = self
            .query_emails(EmailQuery {
                filter: json!({ "inMailbox": mailbox_id }),
                ..EmailQuery::first(limit)
            })
            .await?;
        Ok(page.emails)
    }

    /// Fetch full content — bodies, attachment metadata, threading headers — for
    /// many emails in a **single** `Email/get` call.
    ///
    /// IDs that don't exist are simply absent from the result; the caller decides
    /// whether that is an error. This is the batch primitive behind the GraphQL
    /// email DataLoader.
    #[instrument(skip(self))]
    pub async fn get_emails(&self, ids: &[String]) -> Result<Vec<Email>> {
        self.get_email_records(ids, EMAIL_FULL_PROPERTIES, true)
            .await
    }

    /// Summary records for known IDs — the cheap counterpart to [`Self::get_emails`].
    ///
    /// `Email/query` already returns summaries for the page it matched; this is
    /// for the callers that arrive holding IDs from somewhere else, such as
    /// `Email/changes`.
    #[instrument(skip(self))]
    pub async fn get_email_summaries(&self, ids: &[String]) -> Result<Vec<Email>> {
        self.get_email_records(ids, EMAIL_SUMMARY_PROPERTIES, false)
            .await
    }

    async fn get_email_records(
        &self,
        ids: &[String],
        properties: &[&str],
        fetch_bodies: bool,
    ) -> Result<Vec<Email>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/get",
                {
                    "accountId": account_id,
                    "ids": ids,
                    "properties": properties,
                    "fetchTextBodyValues": fetch_bodies,
                    "fetchHTMLBodyValues": fetch_bodies
                },
                "g0"
            ])])
            .await?;

        let resp: GetResponse<Email> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;

        Ok(resp.list)
    }

    /// The account's current `Email` state string: the cursor
    /// [`Self::email_changes`] reads forward from.
    ///
    /// Fetched with an empty `ids` list, so the server returns the state and no
    /// mail.
    #[instrument(skip(self))]
    pub async fn email_state(&self) -> Result<String> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/get",
                { "accountId": account_id, "ids": [] },
                "s0"
            ])])
            .await?;

        #[derive(Deserialize)]
        struct StateOnly {
            state: String,
        }

        let resp: StateOnly =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;
        Ok(resp.state)
    }

    /// IDs of emails created since `since_state`, and the state they leave the
    /// caller at.
    ///
    /// Follows `hasMoreChanges` to the end, so the returned state is always
    /// current: a partial read would silently drop everything past the first
    /// page on the next call. Only creations are reported — a watcher wants
    /// arrivals, and updates would replay every flag change as news.
    ///
    /// Fails with a `cannotCalculateChanges` JMAP error when the server has
    /// discarded history back that far; the caller resyncs via
    /// [`Self::email_state`].
    #[instrument(skip(self))]
    pub async fn email_changes(&self, since_state: &str) -> Result<EmailChanges> {
        let account_id = self.account_id()?;
        let mut state = since_state.to_string();
        let mut created = Vec::new();

        loop {
            let responses = self
                .request(vec![json!([
                    "Email/changes",
                    {
                        "accountId": account_id,
                        "sinceState": state,
                        "maxChanges": CHANGES_PAGE
                    },
                    "c0"
                ])])
                .await?;

            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ChangesResponse {
                new_state: String,
                #[serde(default)]
                has_more_changes: bool,
                #[serde(default)]
                created: Vec<String>,
            }

            let resp: ChangesResponse =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/changes")?;

            created.extend(resp.created);
            state = resp.new_state;

            if !resp.has_more_changes {
                return Ok(EmailChanges {
                    new_state: state,
                    created,
                });
            }
        }
    }

    /// Open the JMAP push channel and return the live response to read frames
    /// from.
    ///
    /// `last_event_id` asks the server to replay from where a dropped
    /// connection left off. Missing it is not a correctness problem — the
    /// caller holds its own `Email` state and reconciles through
    /// [`Self::email_changes`] — but it saves a round trip.
    #[instrument(skip(self))]
    pub async fn open_event_stream(
        &self,
        ping: u32,
        last_event_id: Option<&str>,
    ) -> Result<reqwest::Response> {
        let template = self
            .session()?
            .event_source_url
            .as_deref()
            .ok_or_else(|| {
                Error::Config(
                    "Server advertises no eventSourceUrl for push. Use --poll to fall back to \
                     periodic checks."
                        .into(),
                )
            })?
            .to_string();

        let url = template
            .replace("{types}", "Email")
            .replace("{closeafter}", "no")
            .replace("{ping}", &ping.to_string());

        // The shared client caps every request at 30s; a push channel is meant
        // to stay open for days. A read timeout of a few ping intervals stands
        // in for it, so silence reads as a dead connection rather than an idle
        // one — the difference between reconnecting and hanging forever.
        let client = Client::builder()
            .read_timeout(Duration::from_secs(u64::from(ping) * 3))
            .build()?;

        let mut req = client
            .get(&url)
            .bearer_auth(&self.token)
            .header("Accept", "text/event-stream");
        if let Some(id) = last_event_id {
            req = req.header("Last-Event-ID", id);
        }

        debug!(url = %url, "Opening JMAP event source");
        let resp = req.send().await?;

        match resp.status().as_u16() {
            401 => return Err(Error::InvalidToken("Token expired or invalid")),
            429 => return Err(Error::RateLimited),
            500..=599 => return Err(Error::Server(format!("Server error: {}", resp.status()))),
            _ => {}
        }

        Ok(resp)
    }

    #[instrument(skip(self))]
    pub async fn get_email(&self, email_id: &str) -> Result<Email> {
        let ids = [email_id.to_string()];
        self.get_emails(&ids)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::EmailNotFound(email_id.into()))
    }

    /// Resolve thread IDs to their member email IDs in a single `Thread/get`.
    ///
    /// Threads that don't exist are absent from the map. This is the batch
    /// primitive behind the GraphQL thread DataLoader.
    #[instrument(skip(self))]
    pub async fn thread_email_ids(
        &self,
        thread_ids: &[String],
    ) -> Result<HashMap<String, Vec<String>>> {
        if thread_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Thread/get",
                {
                    "accountId": account_id,
                    "ids": thread_ids
                },
                "t0"
            ])])
            .await?;

        #[derive(Deserialize)]
        struct Thread {
            id: String,
            #[serde(rename = "emailIds")]
            email_ids: Vec<String>,
        }

        let resp: GetResponse<Thread> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Thread/get")?;

        Ok(resp.list.into_iter().map(|t| (t.id, t.email_ids)).collect())
    }

    /// Get all emails in a thread, with full content.
    #[instrument(skip(self))]
    pub async fn get_thread(&self, email_id: &str) -> Result<Vec<Email>> {
        let email = self.get_email(email_id).await?;
        let thread_id = email
            .thread_id
            .ok_or_else(|| Error::Config("Email has no thread ID".into()))?;

        let ids = self
            .thread_email_ids(std::slice::from_ref(&thread_id))
            .await?
            .remove(&thread_id)
            .ok_or_else(|| Error::Config("Thread not found".into()))?;

        self.get_emails(&ids).await
    }

    /// Search emails with full JMAP filter support
    #[instrument(skip(self, filter))]
    pub async fn search_emails_filtered(
        &self,
        filter: &SearchFilter,
        mailbox_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Email>> {
        let page = self
            .query_emails(EmailQuery {
                filter: search_filter_to_jmap(filter, mailbox_id),
                ..EmailQuery::first(limit)
            })
            .await?;
        Ok(page.emails)
    }

    #[instrument(skip(self))]
    pub async fn list_identities(&self) -> Result<Vec<Identity>> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Identity/get",
                { "accountId": account_id },
                "i0"
            ])])
            .await?;

        let resp: GetResponse<Identity> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Identity/get")?;

        Ok(resp.list)
    }

    async fn resolve_identity(&self, from: Option<&str>) -> Result<Identity> {
        let identities = self.list_identities().await?;
        pick_identity(identities, from)
    }

    /// Return the email address that would be used as the sender for a reply/
    /// send/forward — i.e. the resolved identity's email. Returns `None` if
    /// identity resolution fails, so callers (notably the MCP preview path)
    /// can still produce a useful preview without erroring out.
    pub async fn resolve_my_email(&self, from: Option<&str>) -> Option<String> {
        self.resolve_identity(from).await.ok().map(|i| i.email)
    }

    async fn prepare_compose(&mut self, from: Option<&str>, draft: bool) -> Result<ComposeContext> {
        if !draft {
            self.require_capability("urn:ietf:params:jmap:submission", "Email sending")?;
        }
        let account_id = self.account_id()?.to_string();
        let mailbox = if draft {
            self.find_mailbox("drafts").await?
        } else {
            self.find_mailbox("sent").await?
        };
        let identity = match self.resolve_identity(from).await {
            Ok(id) => Some(id),
            Err(_) if draft => None,
            Err(e) => return Err(e),
        };
        Ok(ComposeContext {
            account_id,
            mailbox,
            identity,
            draft,
        })
    }

    fn parse_email_create_response(responses: &[Value]) -> Result<String> {
        let email_resp: EmailSetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_created) = email_resp.not_created
            && let Some(err) = not_created.get("email")
        {
            let error_type = err
                .get("type")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("unknown");
            let description = err
                .get("description")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("Failed to create email");
            return Err(Error::Jmap {
                method: "Email/set".into(),
                error_type: error_type.into(),
                description: description.into(),
            });
        }

        // Check EmailSubmission/set response if present (index 1)
        if let Some(submission_resp) = responses.get(1) {
            let sub: EmailSetResponse =
                Self::parse_response(submission_resp, "EmailSubmission/set")?;
            if let Some(ref not_created) = sub.not_created
                && let Some(err) = not_created.get("submission")
            {
                let error_type = err
                    .get("type")
                    .and_then(|v: &Value| v.as_str())
                    .unwrap_or("unknown");
                let description = err
                    .get("description")
                    .and_then(|v: &Value| v.as_str())
                    .unwrap_or("Email created but submission failed");
                return Err(Error::Jmap {
                    method: "EmailSubmission/set".into(),
                    error_type: error_type.into(),
                    description: description.into(),
                });
            }
        }

        email_resp
            .created
            .and_then(|c: HashMap<String, Value>| c.get("email").cloned())
            .and_then(|d: Value| {
                d.get("id")
                    .and_then(|v: &Value| v.as_str())
                    .map(String::from)
            })
            .ok_or_else(|| Error::Jmap {
                method: "Email/set".into(),
                error_type: "unknown".into(),
                description: "No email ID returned".into(),
            })
    }

    /// Shared helper: build email_create map with common fields and submit it.
    /// Handles plain text, HTML, and attachment body structures.
    async fn create_and_submit_email(
        &self,
        ctx: &ComposeContext,
        draft: EmailDraft<'_>,
    ) -> Result<String> {
        fn addrs_json(addrs: &[EmailAddress]) -> Value {
            json!(
                addrs
                    .iter()
                    .map(|a| json!({"email": a.email, "name": a.name}))
                    .collect::<Vec<_>>()
            )
        }

        let mut email_create: HashMap<String, Value> = HashMap::new();
        ctx.apply_to_email(&mut email_create);
        email_create.insert("to".into(), addrs_json(draft.to));
        if !draft.cc.is_empty() {
            email_create.insert("cc".into(), addrs_json(draft.cc));
        }
        if !draft.bcc.is_empty() {
            email_create.insert("bcc".into(), addrs_json(draft.bcc));
        }
        email_create.insert("subject".into(), json!(draft.subject));

        // Upload attachments and collect blob IDs
        let mut uploaded_attachments: Vec<UploadedAttachment> = Vec::new();
        for att in draft.attachments {
            let blob_id = self.upload_blob(att.data, &att.content_type).await?;
            uploaded_attachments.push(UploadedAttachment {
                blob_id,
                filename: att.filename,
                content_type: att.content_type,
            });
        }

        apply_body_structure(
            &mut email_create,
            draft.body,
            draft.html_body,
            &uploaded_attachments,
        );

        if let Some(ref headers) = draft.threading {
            if !headers.in_reply_to.is_empty() {
                email_create.insert("inReplyTo".into(), json!(headers.in_reply_to));
            }
            if !headers.references.is_empty() {
                email_create.insert("references".into(), json!(headers.references));
            }
        }

        let responses = self.request(ctx.build_method_calls(email_create)).await?;
        let email_id = Self::parse_email_create_response(&responses)?;

        debug!(email_id = %email_id, draft = ctx.draft, "Email created successfully");
        Ok(email_id)
    }

    #[instrument(skip(self, body, params))]
    pub async fn send_email(
        &mut self,
        to: Vec<EmailAddress>,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
        params: ComposeParams<'_>,
    ) -> Result<String> {
        let ctx = self.prepare_compose(params.from, params.draft).await?;
        self.create_and_submit_email(
            &ctx,
            EmailDraft {
                to: &to,
                cc: &params.cc,
                bcc: &params.bcc,
                subject,
                body,
                html_body: params.html_body.as_deref(),
                attachments: params.attachments,
                threading: in_reply_to.map(|id| ThreadingHeaders {
                    in_reply_to: vec![id.to_string()],
                    references: vec![],
                }),
            },
        )
        .await
    }

    #[instrument(skip(self))]
    pub async fn move_email(&self, email_id: &str, mailbox_id: &str) -> Result<()> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/set",
                {
                    "accountId": account_id,
                    "update": {
                        (email_id): {
                            "mailboxIds": { (mailbox_id): true }
                        }
                    }
                },
                "m0"
            ])])
            .await?;

        let resp: SetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_updated) = resp.not_updated
            && let Some(err) = not_updated.get(email_id)
        {
            let error_type = err
                .get("type")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("unknown");
            let description = err
                .get("description")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("Failed to move email");
            return Err(Error::Jmap {
                method: "Email/set".into(),
                error_type: error_type.into(),
                description: description.into(),
            });
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn mark_spam(&mut self, email_id: &str) -> Result<()> {
        let junk = self.find_mailbox("junk").await?;
        self.move_email(email_id, &junk.id).await
    }

    /// Download a blob (attachment) by ID
    #[instrument(skip(self))]
    pub async fn download_blob(&self, blob_id: &str) -> Result<Vec<u8>> {
        let account_id = self.account_id()?;
        let session = self.session()?;

        // downloadUrl template: https://api.fastmail.com/jmap/download/{accountId}/{blobId}/{name}?accept={type}
        //
        // Single-pass substitution — chained .replace() calls could recursively
        // replace a value that happened to contain another template marker.
        let url = apply_url_template(
            &session.download_url,
            &[
                ("accountId", account_id),
                ("blobId", blob_id),
                ("name", "attachment"),
                ("type", "application/octet-stream"),
            ],
        );

        debug!(url = %url, "Downloading blob");
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await?;

        match resp.status().as_u16() {
            401 => return Err(Error::InvalidToken("Token expired or invalid")),
            404 => return Err(Error::Config(format!("Blob not found: {}", blob_id))),
            429 => return Err(Error::RateLimited),
            500..=599 => return Err(Error::Server(format!("Server error: {}", resp.status()))),
            _ => {}
        }

        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }

    /// Upload a blob (for attachments) and return the blobId
    #[instrument(skip(self, data))]
    pub async fn upload_blob(&self, data: Vec<u8>, content_type: &str) -> Result<String> {
        let account_id = self.account_id()?;
        let session = self.session()?;

        let url = apply_url_template(&session.upload_url, &[("accountId", account_id)]);

        debug!(url = %url, content_type = %content_type, size = data.len(), "Uploading blob");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header("Content-Type", content_type)
            .body(data)
            .send()
            .await?;

        match resp.status().as_u16() {
            200..=299 => {}
            401 => return Err(Error::InvalidToken("Token expired or invalid")),
            429 => return Err(Error::RateLimited),
            500..=599 => return Err(Error::Server(format!("Server error: {}", resp.status()))),
            code => {
                let text = resp.text().await.unwrap_or_default();
                return Err(Error::Server(format!("Upload failed ({}): {}", code, text)));
            }
        }

        let body: Value = resp.json().await?;
        body.get("blobId")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| Error::Server("Upload response missing blobId".into()))
    }

    /// Send a reply to an existing email with proper threading headers.
    ///
    /// The caller is responsible for computing `to` and `params.cc` — usually
    /// by calling [`expand_reply_recipients`] after resolving the sending
    /// identity with [`JmapClient::resolve_my_email`]. Keeping the expansion
    /// on the caller side means the MCP preview path and the send path use
    /// exactly the same recipient lists, so the preview cannot under-report
    /// or diverge from what will actually be sent.
    #[instrument(skip(self, body, params))]
    pub async fn reply_email(
        &mut self,
        original: &Email,
        body: &str,
        to: Vec<EmailAddress>,
        params: ComposeParams<'_>,
    ) -> Result<String> {
        let ctx = self.prepare_compose(params.from, params.draft).await?;
        let to_addrs = to;
        let cc_addrs = params.cc;

        // Build subject with Re: prefix if not already present
        let subject = if original
            .subject
            .as_ref()
            .is_some_and(|s| s.to_lowercase().starts_with("re:"))
        {
            original.subject.clone().unwrap_or_default()
        } else {
            format!("Re: {}", original.subject.as_deref().unwrap_or(""))
        };

        // Build References header: original references + original message-id
        let references: Vec<String> = {
            let mut refs = original.references.clone().unwrap_or_default();
            if let Some(ref msg_id) = original.message_id {
                for id in msg_id {
                    if !refs.contains(id) {
                        refs.push(id.clone());
                    }
                }
            }
            refs
        };

        self.create_and_submit_email(
            &ctx,
            EmailDraft {
                to: &to_addrs,
                cc: &cc_addrs,
                bcc: &params.bcc,
                subject: &subject,
                body,
                html_body: params.html_body.as_deref(),
                attachments: params.attachments,
                threading: Some(ThreadingHeaders {
                    in_reply_to: original.message_id.clone().unwrap_or_default(),
                    references,
                }),
            },
        )
        .await
    }

    /// Forward an email with proper attribution
    #[instrument(skip(self, body, params))]
    pub async fn forward_email(
        &mut self,
        original: &Email,
        to: Vec<EmailAddress>,
        body: &str,
        params: ComposeParams<'_>,
    ) -> Result<String> {
        let ctx = self.prepare_compose(params.from, params.draft).await?;

        // Build subject with Fwd: prefix if not already present
        let subject = if original
            .subject
            .as_ref()
            .is_some_and(|s| s.to_lowercase().starts_with("fwd:"))
        {
            original.subject.clone().unwrap_or_default()
        } else {
            format!("Fwd: {}", original.subject.as_deref().unwrap_or(""))
        };

        // Build forwarded body with attribution
        let original_body = original.text_content().unwrap_or_default();

        let sender = original
            .from
            .as_ref()
            .and_then(|f| f.first())
            .map(|a| a.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let date = original.received_at.as_deref().unwrap_or("unknown date");

        let full_body = format!(
            "{}\n\n---------- Forwarded message ---------\nFrom: {}\nDate: {}\nSubject: {}\n\n{}",
            body,
            sender,
            date,
            original.subject.as_deref().unwrap_or(""),
            original_body
        );

        self.create_and_submit_email(
            &ctx,
            EmailDraft {
                to: &to,
                cc: &params.cc,
                bcc: &params.bcc,
                subject: &subject,
                body: &full_body,
                html_body: params.html_body.as_deref(),
                attachments: params.attachments,
                threading: None,
            },
        )
        .await
    }

    #[instrument(skip(self))]
    pub async fn set_keywords(
        &self,
        email_id: &str,
        keywords: HashMap<String, bool>,
    ) -> Result<()> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/set",
                {
                    "accountId": account_id,
                    "update": {
                        (email_id): {
                            "keywords": keywords
                        }
                    }
                },
                "k0"
            ])])
            .await?;

        let resp: SetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_updated) = resp.not_updated
            && let Some(err) = not_updated.get(email_id)
        {
            let error_type = err
                .get("type")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("unknown");
            let description = err
                .get("description")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("Failed to update keywords");
            return Err(Error::Jmap {
                method: "Email/set".into(),
                error_type: error_type.into(),
                description: description.into(),
            });
        }

        Ok(())
    }

    /// List all masked email addresses
    #[instrument(skip(self))]
    pub async fn list_masked_emails(&self) -> Result<Vec<MaskedEmail>> {
        self.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email")?;
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "MaskedEmail/get",
                {
                    "accountId": account_id,
                    "ids": null
                },
                "me0"
            ])])
            .await?;

        let resp: GetResponse<MaskedEmail> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "MaskedEmail/get")?;

        Ok(resp.list)
    }

    /// Create a new masked email address
    #[instrument(skip(self))]
    pub async fn create_masked_email(
        &self,
        for_domain: Option<&str>,
        description: Option<&str>,
        email_prefix: Option<&str>,
    ) -> Result<MaskedEmail> {
        self.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email")?;
        let account_id = self.account_id()?;

        let mut create_obj: HashMap<String, Value> = HashMap::new();
        create_obj.insert("state".into(), json!("enabled"));

        if let Some(domain) = for_domain {
            create_obj.insert("forDomain".into(), json!(domain));
        }
        if let Some(desc) = description {
            create_obj.insert("description".into(), json!(desc));
        }
        if let Some(prefix) = email_prefix {
            create_obj.insert("emailPrefix".into(), json!(prefix));
        }

        let responses = self
            .request(vec![json!([
                "MaskedEmail/set",
                {
                    "accountId": account_id,
                    "create": { "new": create_obj }
                },
                "me0"
            ])])
            .await?;

        let resp: MaskedEmailCreateResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "MaskedEmail/set")?;

        if let Some(ref not_created) = resp.not_created
            && let Some(err) = not_created.get("new")
        {
            let error_type = err
                .get("type")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("unknown");
            let description = err
                .get("description")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("Failed to create masked email");
            return Err(Error::Jmap {
                method: "MaskedEmail/set".into(),
                error_type: error_type.into(),
                description: description.into(),
            });
        }

        resp.created
            .and_then(|mut c| c.remove("new"))
            .ok_or_else(|| Error::Jmap {
                method: "MaskedEmail/set".into(),
                error_type: "unknown".into(),
                description: "No masked email returned".into(),
            })
    }

    /// Update a masked email's state (enable/disable/delete)
    #[instrument(skip(self))]
    pub async fn update_masked_email(
        &self,
        id: &str,
        state: Option<&str>,
        for_domain: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        self.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email")?;
        let account_id = self.account_id()?;

        let mut update_obj: HashMap<String, Value> = HashMap::new();
        if let Some(s) = state {
            update_obj.insert("state".into(), json!(s));
        }
        if let Some(domain) = for_domain {
            update_obj.insert("forDomain".into(), json!(domain));
        }
        if let Some(desc) = description {
            update_obj.insert("description".into(), json!(desc));
        }

        let responses = self
            .request(vec![json!([
                "MaskedEmail/set",
                {
                    "accountId": account_id,
                    "update": { (id): update_obj }
                },
                "me0"
            ])])
            .await?;

        let resp: SetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "MaskedEmail/set")?;

        if let Some(ref not_updated) = resp.not_updated
            && let Some(err) = not_updated.get(id)
        {
            let error_type = err
                .get("type")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("unknown");
            let description = err
                .get("description")
                .and_then(|v: &Value| v.as_str())
                .unwrap_or("Failed to update masked email");
            return Err(Error::Jmap {
                method: "MaskedEmail/set".into(),
                error_type: error_type.into(),
                description: description.into(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_session(capabilities: Vec<&str>) -> Session {
        let mut caps = HashMap::new();
        for cap in capabilities {
            caps.insert(cap.to_string(), serde_json::json!({}));
        }

        let mut primary_accounts = HashMap::new();
        primary_accounts.insert(
            "urn:ietf:params:jmap:mail".to_string(),
            "test-account".to_string(),
        );

        Session {
            capabilities: caps,
            accounts: HashMap::new(),
            primary_accounts,
            username: "test@example.com".to_string(),
            api_url: "https://api.example.com/jmap".to_string(),
            download_url: "https://api.example.com/download".to_string(),
            upload_url: "https://api.example.com/upload".to_string(),
            event_source_url: None,
            state: None,
        }
    }

    #[test]
    fn test_apply_url_template_basic() {
        let result = apply_url_template(
            "https://api.example.com/{a}/{b}",
            &[("a", "hello"), ("b", "world")],
        );
        assert_eq!(result, "https://api.example.com/hello/world");
    }

    #[test]
    fn test_apply_url_template_no_cascade() {
        // A value that contains another template marker must not be re-substituted.
        let result = apply_url_template("https://x/{a}/{b}", &[("a", "{b}"), ("b", "LEAKED")]);
        assert_eq!(result, "https://x/{b}/LEAKED");
    }

    #[test]
    fn test_apply_url_template_unknown_placeholder_preserved() {
        let result = apply_url_template("/{known}/{other}", &[("known", "X")]);
        assert_eq!(result, "/X/{other}");
    }

    #[test]
    fn test_apply_url_template_no_placeholders() {
        let result = apply_url_template("https://api.example.com/v1", &[]);
        assert_eq!(result, "https://api.example.com/v1");
    }

    #[test]
    fn test_apply_url_template_unterminated_brace() {
        let result = apply_url_template("/path/{unterminated", &[]);
        assert_eq!(result, "/path/{unterminated");
    }

    #[test]
    fn test_require_capability_succeeds_when_present() {
        let mut client = JmapClient::new("test-token".to_string());
        client.session = Some(create_test_session(vec![
            "urn:ietf:params:jmap:core",
            "urn:ietf:params:jmap:mail",
            "urn:ietf:params:jmap:submission",
        ]));

        assert!(
            client
                .require_capability("urn:ietf:params:jmap:submission", "Email sending")
                .is_ok()
        );
    }

    #[test]
    fn test_require_capability_fails_when_missing() {
        let mut client = JmapClient::new("test-token".to_string());
        client.session = Some(create_test_session(vec![
            "urn:ietf:params:jmap:core",
            "urn:ietf:params:jmap:mail",
        ]));

        let result = client.require_capability("urn:ietf:params:jmap:submission", "Email sending");
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("urn:ietf:params:jmap:submission"));
        assert!(err_msg.contains("read-only"));
    }

    #[test]
    fn test_require_capability_fails_when_no_session() {
        let client = JmapClient::new("test-token".to_string());

        let result = client.require_capability("urn:ietf:params:jmap:submission", "Email sending");
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Authentication required"));
        assert!(!err_msg.contains("read-only"));
    }

    #[test]
    fn test_require_capability_works_for_masked_email() {
        let mut client = JmapClient::new("test-token".to_string());
        client.session = Some(create_test_session(vec![
            "urn:ietf:params:jmap:core",
            "urn:ietf:params:jmap:mail",
        ]));

        let result =
            client.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("maskedemail"));
    }

    fn test_identity(id: &str, email: &str, name: &str) -> Identity {
        Identity {
            id: id.to_string(),
            name: name.to_string(),
            email: email.to_string(),
            reply_to: None,
            bcc: None,
            html_signature: None,
            text_signature: None,
            may_delete: true,
        }
    }

    fn addr(email: &str) -> EmailAddress {
        EmailAddress {
            name: None,
            email: email.to_string(),
        }
    }

    fn reply_fixture(from: Vec<&str>, to: Vec<&str>, cc: Vec<&str>) -> Email {
        let mut email = Email {
            id: "test".into(),
            ..Default::default()
        };
        email.from = Some(from.iter().map(|e| addr(e)).collect());
        email.to = Some(to.iter().map(|e| addr(e)).collect());
        email.cc = Some(cc.iter().map(|e| addr(e)).collect());
        email
    }

    fn emails(addrs: &[EmailAddress]) -> Vec<String> {
        addrs.iter().map(|a| a.email.clone()).collect()
    }

    #[test]
    fn test_expand_reply_plain_does_not_expand() {
        let original = reply_fixture(
            vec!["sender@x"],
            vec!["recip1@x", "recip2@x"],
            vec!["cc1@x"],
        );
        let (to, cc) =
            expand_reply_recipients(&original, false, Some("me@x"), vec![addr("user@x")]);
        assert_eq!(emails(&to), vec!["sender@x"]);
        assert_eq!(emails(&cc), vec!["user@x"]);
    }

    #[test]
    fn test_expand_reply_prefers_reply_to_over_from() {
        // The shape that bounces in the wild: a branded From on a domain with
        // no MX record, and the real inbox in Reply-To.
        let mut original = reply_fixture(vec!["noreply@branded.invalid"], vec![], vec![]);
        original.reply_to = Some(vec![addr("support@real")]);

        let (to, _) = expand_reply_recipients(&original, false, Some("me@x"), vec![]);
        assert_eq!(emails(&to), vec!["support@real"]);
    }

    #[test]
    fn test_expand_reply_all_uses_reply_to_as_the_sender() {
        let mut original = reply_fixture(
            vec!["noreply@branded.invalid"],
            vec!["recip1@x", "me@x"],
            vec!["cc1@x"],
        );
        original.reply_to = Some(vec![addr("support@real")]);

        let (to, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
        // Reply-To replaces From, it does not join it: the branded address is
        // still undeliverable on a reply-all.
        assert_eq!(emails(&to), vec!["support@real", "recip1@x"]);
        assert_eq!(emails(&cc), vec!["cc1@x"]);
    }

    #[test]
    fn test_expand_reply_falls_back_to_from_when_reply_to_is_absent_or_empty() {
        let original = reply_fixture(vec!["sender@x"], vec![], vec![]);
        let (to, _) = expand_reply_recipients(&original, false, None, vec![]);
        assert_eq!(emails(&to), vec!["sender@x"]);

        // An empty list is not a Reply-To. Treating it as one would reply to
        // nobody.
        let mut empty = reply_fixture(vec!["sender@x"], vec![], vec![]);
        empty.reply_to = Some(vec![]);
        let (to, _) = expand_reply_recipients(&empty, false, None, vec![]);
        assert_eq!(emails(&to), vec!["sender@x"]);
    }

    #[test]
    fn test_expand_reply_all_adds_original_recipients() {
        let original = reply_fixture(
            vec!["sender@x"],
            vec!["recip1@x", "recip2@x"],
            vec!["cc1@x"],
        );
        let (to, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
        assert_eq!(emails(&to), vec!["sender@x", "recip1@x", "recip2@x"]);
        assert_eq!(emails(&cc), vec!["cc1@x"]);
    }

    #[test]
    fn test_expand_reply_all_filters_me_from_to() {
        let original = reply_fixture(
            vec!["sender@x"],
            vec!["recip1@x", "me@x", "recip2@x"],
            vec![],
        );
        let (to, _) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
        assert_eq!(emails(&to), vec!["sender@x", "recip1@x", "recip2@x"]);
    }

    #[test]
    fn test_expand_reply_all_filters_me_from_cc() {
        let original = reply_fixture(vec!["sender@x"], vec![], vec!["cc1@x", "me@x", "cc2@x"]);
        let (_, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
        assert_eq!(emails(&cc), vec!["cc1@x", "cc2@x"]);
    }

    #[test]
    fn test_expand_reply_all_case_insensitive_me() {
        let original = reply_fixture(vec!["sender@x"], vec!["ME@X"], vec!["me@X"]);
        let (to, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
        assert_eq!(emails(&to), vec!["sender@x"]);
        assert_eq!(emails(&cc), Vec::<String>::new());
    }

    #[test]
    fn test_expand_reply_dedupes_overlapping_user_cc_and_reply_all_to() {
        // The exact duplicate-send scenario from the bug report: user notices
        // preview is missing recipients, adds them as cc to "fix" the preview;
        // send path expands reply-all into To AND those addresses appear in CC.
        let original = reply_fixture(
            vec!["paul@x"],
            vec!["sher@x", "dylan@x", "anne@x", "leon@x"],
            vec![],
        );
        let user_cc = vec![addr("sher@x"), addr("anne@x"), addr("leon@x")];
        let (to, cc) = expand_reply_recipients(&original, true, Some("dylan@x"), user_cc);
        // Dylan filtered out; rest in To.
        assert_eq!(emails(&to), vec!["paul@x", "sher@x", "anne@x", "leon@x"]);
        // Nothing in CC — all user-supplied addresses were already in To.
        assert_eq!(emails(&cc), Vec::<String>::new());
    }

    #[test]
    fn test_expand_reply_dedupes_duplicates_in_original() {
        // Unusual but possible: original.from address also appears in
        // original.to (e.g. sender CC'd themselves).
        let original = reply_fixture(vec!["x@x"], vec!["x@x", "y@x"], vec![]);
        let (to, _) = expand_reply_recipients(&original, true, None, vec![]);
        assert_eq!(emails(&to), vec!["x@x", "y@x"]);
    }

    #[test]
    fn test_expand_reply_without_my_email_still_dedupes() {
        // Preview path when identity resolution fails: no "me" filter, but
        // dedup should still run.
        let original = reply_fixture(vec!["sender@x"], vec!["a@x", "a@x"], vec![]);
        let (to, _) = expand_reply_recipients(&original, true, None, vec![]);
        assert_eq!(emails(&to), vec!["sender@x", "a@x"]);
    }

    #[test]
    fn test_expand_reply_preserves_to_order() {
        let original = reply_fixture(
            vec!["first@x"],
            vec!["second@x", "third@x"],
            vec!["fourth@x", "fifth@x"],
        );
        let (to, cc) = expand_reply_recipients(&original, true, None, vec![]);
        assert_eq!(emails(&to), vec!["first@x", "second@x", "third@x"]);
        assert_eq!(emails(&cc), vec!["fourth@x", "fifth@x"]);
    }

    #[test]
    fn test_pick_identity_none_returns_first() {
        let identities = vec![
            test_identity("id1", "alice@example.com", "Alice"),
            test_identity("id2", "bob@example.com", "Bob"),
        ];
        let result = pick_identity(identities, None).unwrap();
        assert_eq!(result.email, "alice@example.com");
    }

    #[test]
    fn test_pick_identity_none_empty_list() {
        let result = pick_identity(vec![], None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Identity not found")
        );
    }

    #[test]
    fn test_pick_identity_matches_exact() {
        let identities = vec![
            test_identity("id1", "alice@example.com", "Alice"),
            test_identity("id2", "bob@example.com", "Bob"),
        ];
        let result = pick_identity(identities, Some("bob@example.com")).unwrap();
        assert_eq!(result.id, "id2");
    }

    #[test]
    fn test_pick_identity_case_insensitive() {
        let identities = vec![test_identity("id1", "Alice@Example.COM", "Alice")];
        let result = pick_identity(identities, Some("alice@example.com")).unwrap();
        assert_eq!(result.id, "id1");
    }

    #[test]
    fn test_pick_identity_not_found() {
        let identities = vec![test_identity("id1", "alice@example.com", "Alice")];
        let result = pick_identity(identities, Some("nobody@example.com"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nobody@example.com"));
        assert!(err.contains("list identities"));
    }

    // ============ Body structure tests ============

    #[test]
    fn test_body_structure_plain_text_only() {
        let mut email = HashMap::new();
        apply_body_structure(&mut email, "Hello world", None, &[]);

        // Should have textBody array, no htmlBody, no bodyStructure
        assert!(email.contains_key("textBody"));
        assert!(!email.contains_key("htmlBody"));
        assert!(!email.contains_key("bodyStructure"));

        let text_body = &email["textBody"];
        assert_eq!(text_body[0]["partId"], "textBody");
        assert_eq!(text_body[0]["type"], "text/plain");

        let body_values = &email["bodyValues"];
        assert_eq!(body_values["textBody"]["value"], "Hello world");
        assert_eq!(body_values["textBody"]["charset"], "utf-8");
    }

    #[test]
    fn test_body_structure_text_plus_html() {
        let mut email = HashMap::new();
        apply_body_structure(&mut email, "fallback", Some("<h1>Rich</h1>"), &[]);

        // Should have both textBody and htmlBody arrays, no bodyStructure
        assert!(email.contains_key("textBody"));
        assert!(email.contains_key("htmlBody"));
        assert!(!email.contains_key("bodyStructure"));

        assert_eq!(email["textBody"][0]["partId"], "textBody");
        assert_eq!(email["htmlBody"][0]["partId"], "htmlBody");
        assert_eq!(email["htmlBody"][0]["type"], "text/html");

        let body_values = &email["bodyValues"];
        assert_eq!(body_values["textBody"]["value"], "fallback");
        assert_eq!(body_values["htmlBody"]["value"], "<h1>Rich</h1>");
    }

    #[test]
    fn test_body_structure_text_with_attachment() {
        let mut email = HashMap::new();
        let attachments = vec![UploadedAttachment {
            blob_id: "Gblob123".into(),
            filename: "report.pdf".into(),
            content_type: "application/pdf".into(),
        }];
        apply_body_structure(&mut email, "See attached", None, &attachments);

        // Must use bodyStructure, NOT textBody/htmlBody
        assert!(email.contains_key("bodyStructure"));
        assert!(!email.contains_key("textBody"));
        assert!(!email.contains_key("htmlBody"));

        let structure = &email["bodyStructure"];
        assert_eq!(structure["type"], "multipart/mixed");

        let parts = structure["subParts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);

        // First part: plain text
        assert_eq!(parts[0]["partId"], "textBody");
        assert_eq!(parts[0]["type"], "text/plain");

        // Second part: attachment
        assert_eq!(parts[1]["blobId"], "Gblob123");
        assert_eq!(parts[1]["name"], "report.pdf");
        assert_eq!(parts[1]["type"], "application/pdf");
        assert_eq!(parts[1]["disposition"], "attachment");
    }

    #[test]
    fn test_body_structure_html_with_attachment() {
        let mut email = HashMap::new();
        let attachments = vec![UploadedAttachment {
            blob_id: "Gblob456".into(),
            filename: "_DSF1117.jpg".into(),
            content_type: "image/jpeg".into(),
        }];
        apply_body_structure(
            &mut email,
            "Fallback text",
            Some("<h1>Photo</h1>"),
            &attachments,
        );

        assert!(email.contains_key("bodyStructure"));
        assert!(!email.contains_key("textBody"));
        assert!(!email.contains_key("htmlBody"));

        let structure = &email["bodyStructure"];
        assert_eq!(structure["type"], "multipart/mixed");

        let parts = structure["subParts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);

        // First part: multipart/alternative with text + html
        assert_eq!(parts[0]["type"], "multipart/alternative");
        let alt_parts = parts[0]["subParts"].as_array().unwrap();
        assert_eq!(alt_parts.len(), 2);
        assert_eq!(alt_parts[0]["partId"], "textBody");
        assert_eq!(alt_parts[1]["partId"], "htmlBody");

        // Second part: attachment
        assert_eq!(parts[1]["blobId"], "Gblob456");
        assert_eq!(parts[1]["name"], "_DSF1117.jpg");

        // bodyValues should have both text and html
        let bv = &email["bodyValues"];
        assert_eq!(bv["textBody"]["value"], "Fallback text");
        assert_eq!(bv["htmlBody"]["value"], "<h1>Photo</h1>");
    }

    #[test]
    fn test_body_structure_multiple_attachments() {
        let mut email = HashMap::new();
        let attachments = vec![
            UploadedAttachment {
                blob_id: "Ga".into(),
                filename: "a.pdf".into(),
                content_type: "application/pdf".into(),
            },
            UploadedAttachment {
                blob_id: "Gb".into(),
                filename: "b.xlsx".into(),
                content_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                    .into(),
            },
        ];
        apply_body_structure(&mut email, "docs attached", None, &attachments);

        let parts = email["bodyStructure"]["subParts"].as_array().unwrap();
        assert_eq!(parts.len(), 3); // text + 2 attachments
        assert_eq!(parts[1]["blobId"], "Ga");
        assert_eq!(parts[2]["blobId"], "Gb");
    }

    // ============ upload_blob mock test ============

    /// Build a client pointed at a mock JMAP server.
    fn mock_client(uri: &str) -> JmapClient {
        let mut client = JmapClient::new("test-token".to_string());
        let mut session = create_test_session(vec![
            "urn:ietf:params:jmap:core",
            "urn:ietf:params:jmap:mail",
        ]);
        session.api_url = format!("{uri}/jmap");
        client.available_capabilities = session.capabilities.keys().cloned().collect();
        client.session = Some(session);
        client
    }

    /// One `methodResponses` envelope around a single method result.
    fn jmap_response(method: &str, result: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "methodResponses": [[method, result, "c0"]] })
    }

    #[tokio::test]
    async fn test_email_state_reads_state_without_fetching_mail() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(r#""ids":[]"#))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "Email/get",
                serde_json::json!({ "state": "state-42", "list": [], "notFound": [] }),
            )))
            .mount(&mock_server)
            .await;

        let client = mock_client(&mock_server.uri());
        assert_eq!(client.email_state().await.unwrap(), "state-42");
    }

    #[tokio::test]
    async fn test_email_changes_follows_has_more_changes() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Each page is matched by the state it was asked to start from, so the
        // test asserts the cursor actually advances rather than trusting order.
        Mock::given(method("POST"))
            .and(body_string_contains(r#""sinceState":"s0""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "Email/changes",
                serde_json::json!({
                    "oldState": "s0",
                    "newState": "s1",
                    "hasMoreChanges": true,
                    "created": ["e1", "e2"],
                    "updated": [],
                    "destroyed": []
                }),
            )))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(r#""sinceState":"s1""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "Email/changes",
                serde_json::json!({
                    "oldState": "s1",
                    "newState": "s2",
                    "hasMoreChanges": false,
                    "created": ["e3"],
                    "updated": [],
                    "destroyed": []
                }),
            )))
            .mount(&mock_server)
            .await;

        let client = mock_client(&mock_server.uri());
        let changes = client.email_changes("s0").await.unwrap();

        assert_eq!(changes.created, vec!["e1", "e2", "e3"]);
        assert_eq!(changes.new_state, "s2");
    }

    #[tokio::test]
    async fn test_email_changes_surfaces_cannot_calculate_changes() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "error",
                serde_json::json!({ "type": "cannotCalculateChanges" }),
            )))
            .mount(&mock_server)
            .await;

        let client = mock_client(&mock_server.uri());
        let err = client.email_changes("ancient").await.unwrap_err();

        // The watcher keys its resync off this type, so it has to survive the
        // trip through parse_response intact.
        assert!(
            matches!(&err, Error::Jmap { error_type, .. } if error_type == "cannotCalculateChanges"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_get_email_summaries_skips_body_values() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Bodies are the expensive half of Email/get; a watcher fetching them
        // by default would make every arrival cost a full document parse.
        Mock::given(method("POST"))
            .and(body_string_contains(r#""fetchTextBodyValues":false"#))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "Email/get",
                serde_json::json!({
                    "state": "s1",
                    "list": [{ "id": "e1", "subject": "hi" }],
                    "notFound": []
                }),
            )))
            .mount(&mock_server)
            .await;

        let client = mock_client(&mock_server.uri());
        let emails = client
            .get_email_summaries(&["e1".to_string()])
            .await
            .unwrap();

        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].subject.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn test_open_event_stream_fills_the_url_template() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // The session hands back a URI template; getting any placeholder wrong
        // fails silently as "push just never fires".
        Mock::given(method("GET"))
            .and(path("/jmap/event-source/"))
            .and(query_param("types", "Email"))
            .and(query_param("closeafter", "no"))
            .and(query_param("ping", "30"))
            .and(header("Authorization", "Bearer test-token"))
            .and(header("Last-Event-ID", "evt-7"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_string(": ping\n\n"),
            )
            .mount(&mock_server)
            .await;

        let mut client = mock_client(&mock_server.uri());
        client.session.as_mut().unwrap().event_source_url = Some(format!(
            "{}/jmap/event-source/?types={{types}}&closeafter={{closeafter}}&ping={{ping}}",
            mock_server.uri()
        ));

        let resp = client.open_event_stream(30, Some("evt-7")).await.unwrap();
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn test_open_event_stream_without_a_url_points_at_poll() {
        // create_test_session advertises no eventSourceUrl, standing in for a
        // server that does not offer push.
        let client = mock_client("https://api.example.com");
        let err = client.open_event_stream(30, None).await.unwrap_err();
        assert!(err.to_string().contains("--poll"), "unhelpful error: {err}");
    }

    #[tokio::test]
    async fn test_upload_blob_success() {
        use wiremock::matchers::{header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Mock the upload endpoint — matches what Fastmail returns
        Mock::given(method("POST"))
            .and(header("Content-Type", "image/jpeg"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accountId": "test-account",
                "blobId": "G31e09448268297247a1b215a4ce1e7bc7ee05699",
                "expires": "2026-04-12T15:35:44Z",
                "size": 958081,
                "type": "image/jpeg"
            })))
            .mount(&mock_server)
            .await;

        let mut client = JmapClient::new("test-token".to_string());
        let mut session = create_test_session(vec!["urn:ietf:params:jmap:core"]);
        session.upload_url = format!("{}/upload/{{accountId}}/", mock_server.uri());
        client.session = Some(session);

        let blob_id = client
            .upload_blob(b"fake image data".to_vec(), "image/jpeg")
            .await
            .unwrap();
        assert_eq!(blob_id, "G31e09448268297247a1b215a4ce1e7bc7ee05699");
    }

    #[tokio::test]
    async fn test_upload_blob_413_too_large() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(413).set_body_string("Request Entity Too Large"))
            .mount(&mock_server)
            .await;

        let mut client = JmapClient::new("test-token".to_string());
        let mut session = create_test_session(vec!["urn:ietf:params:jmap:core"]);
        session.upload_url = format!("{}/upload/{{accountId}}/", mock_server.uri());
        client.session = Some(session);

        let result = client
            .upload_blob(b"huge file".to_vec(), "application/pdf")
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("413"));
        assert!(err.contains("Too Large"));
    }

    #[tokio::test]
    async fn test_upload_blob_rate_limited() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let mut client = JmapClient::new("test-token".to_string());
        let mut session = create_test_session(vec!["urn:ietf:params:jmap:core"]);
        session.upload_url = format!("{}/upload/{{accountId}}/", mock_server.uri());
        client.session = Some(session);

        let result = client.upload_blob(b"data".to_vec(), "text/plain").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Rate limited"));
    }
}

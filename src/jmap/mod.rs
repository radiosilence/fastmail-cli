use crate::commands::SearchFilter;
use crate::error::{Error, Result};
use crate::models::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, instrument};

const SESSION_URL: &str = "https://api.fastmail.com/jmap/session";
const TIMEOUT: Duration = Duration::from_secs(30);

const DESIRED_CAPABILITIES: &[&str] = &[
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "https://www.fastmail.com/dev/maskedemail",
];

pub struct JmapClient {
    client: Client,
    token: String,
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

// Shared JMAP response types used across multiple methods
#[derive(Deserialize)]
struct GetResponse<T> {
    list: Vec<T>,
}

#[derive(Deserialize)]
struct GetResponseWithNotFound<T> {
    list: Vec<T>,
    #[serde(rename = "notFound")]
    not_found: Vec<String>,
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

fn pick_identity(identities: Vec<Identity>, from: Option<&str>) -> Result<Identity> {
    match from {
        Some(email) => identities
            .into_iter()
            .find(|i| i.email.eq_ignore_ascii_case(email))
            .ok_or_else(|| Error::IdentityNotFoundForEmail(email.to_string())),
        None => identities.into_iter().next().ok_or(Error::IdentityNotFound),
    }
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
            session: None,
            available_capabilities: Vec::new(),
            cached_mailboxes: None,
        }
    }

    #[instrument(skip(self))]
    pub async fn authenticate(&mut self) -> Result<&Session> {
        debug!("Fetching JMAP session");
        let resp = self
            .client
            .get(SESSION_URL)
            .bearer_auth(&self.token)
            .send()
            .await?;

        match resp.status().as_u16() {
            401 => return Err(Error::InvalidToken("Authentication failed".into())),
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
            401 => return Err(Error::InvalidToken("Token expired or invalid".into())),
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

    #[instrument(skip(self))]
    pub async fn list_mailboxes(&mut self) -> Result<Vec<Mailbox>> {
        if let Some(ref cached) = self.cached_mailboxes {
            return Ok(cached.clone());
        }

        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Mailbox/get",
                {
                    "accountId": account_id,
                    "properties": [
                        "id", "name", "parentId", "role",
                        "totalEmails", "unreadEmails",
                        "totalThreads", "unreadThreads", "sortOrder"
                    ]
                },
                "m0"
            ])])
            .await?;

        let resp: GetResponse<Mailbox> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Mailbox/get")?;

        self.cached_mailboxes = Some(resp.list.clone());
        Ok(resp.list)
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

    #[instrument(skip(self))]
    pub async fn list_emails(&self, mailbox_id: &str, limit: u32) -> Result<Vec<Email>> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![
                json!([
                    "Email/query",
                    {
                        "accountId": account_id,
                        "filter": { "inMailbox": mailbox_id },
                        "sort": [{"property": "receivedAt", "isAscending": false}],
                        "limit": limit
                    },
                    "q0"
                ]),
                json!([
                    "Email/get",
                    {
                        "accountId": account_id,
                        "#ids": {
                            "resultOf": "q0",
                            "name": "Email/query",
                            "path": "/ids"
                        },
                        "properties": [
                            "id", "threadId", "mailboxIds", "keywords",
                            "size", "receivedAt", "from", "to", "cc",
                            "subject", "preview", "hasAttachment"
                        ]
                    },
                    "g0"
                ]),
            ])
            .await?;

        let resp: GetResponse<Email> =
            Self::parse_response(responses.get(1).unwrap_or(&Value::Null), "Email/get")?;

        Ok(resp.list)
    }

    #[instrument(skip(self))]
    pub async fn get_email(&self, email_id: &str) -> Result<Email> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/get",
                {
                    "accountId": account_id,
                    "ids": [email_id],
                    "properties": [
                        "id", "blobId", "threadId", "mailboxIds", "keywords",
                        "size", "receivedAt", "messageId", "inReplyTo", "references",
                        "from", "to", "cc", "bcc", "replyTo", "subject", "sentAt",
                        "preview", "hasAttachment", "textBody", "htmlBody", "attachments",
                        "bodyValues"
                    ],
                    "fetchTextBodyValues": true,
                    "fetchHTMLBodyValues": true
                },
                "g0"
            ])])
            .await?;

        let resp: GetResponseWithNotFound<Email> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;

        if !resp.not_found.is_empty() {
            return Err(Error::EmailNotFound(email_id.into()));
        }

        resp.list
            .into_iter()
            .next()
            .ok_or_else(|| Error::EmailNotFound(email_id.into()))
    }

    /// Get all emails in a thread
    #[instrument(skip(self))]
    pub async fn get_thread(&self, email_id: &str) -> Result<Vec<Email>> {
        let account_id = self.account_id()?;

        // First get the email to find its threadId
        let email = self.get_email(email_id).await?;
        let thread_id = email
            .thread_id
            .ok_or_else(|| Error::Config("Email has no thread ID".into()))?;

        // Get the thread to find all email IDs
        let responses = self
            .request(vec![json!([
                "Thread/get",
                {
                    "accountId": account_id,
                    "ids": [thread_id]
                },
                "t0"
            ])])
            .await?;

        #[derive(Deserialize)]
        struct Thread {
            #[serde(rename = "emailIds")]
            email_ids: Vec<String>,
        }

        let thread_resp: GetResponse<Thread> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Thread/get")?;

        let thread = thread_resp
            .list
            .into_iter()
            .next()
            .ok_or_else(|| Error::Config("Thread not found".into()))?;

        // Now get all emails in the thread
        let responses = self
            .request(vec![json!([
                "Email/get",
                {
                    "accountId": account_id,
                    "ids": thread.email_ids,
                    "properties": [
                        "id", "threadId", "mailboxIds", "keywords",
                        "size", "receivedAt", "from", "to", "cc",
                        "subject", "preview", "hasAttachment", "textBody", "htmlBody", "bodyValues"
                    ],
                    "fetchTextBodyValues": true,
                    "fetchHTMLBodyValues": true
                },
                "e0"
            ])])
            .await?;

        let resp: GetResponse<Email> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;

        Ok(resp.list)
    }

    /// Search emails with full JMAP filter support
    #[instrument(skip(self, filter))]
    pub async fn search_emails_filtered(
        &self,
        filter: &SearchFilter,
        mailbox_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Email>> {
        let account_id = self.account_id()?;

        // Build JMAP filter object
        let mut jmap_filter = json!({});

        if let Some(ref text) = filter.text {
            jmap_filter["text"] = json!(text);
        }
        if let Some(ref from) = filter.from {
            jmap_filter["from"] = json!(from);
        }
        if let Some(ref to) = filter.to {
            jmap_filter["to"] = json!(to);
        }
        if let Some(ref cc) = filter.cc {
            jmap_filter["cc"] = json!(cc);
        }
        if let Some(ref bcc) = filter.bcc {
            jmap_filter["bcc"] = json!(bcc);
        }
        if let Some(ref subject) = filter.subject {
            jmap_filter["subject"] = json!(subject);
        }
        if let Some(ref body) = filter.body {
            jmap_filter["body"] = json!(body);
        }
        if let Some(mailbox) = mailbox_id {
            jmap_filter["inMailbox"] = json!(mailbox);
        }
        if filter.has_attachment {
            jmap_filter["hasAttachment"] = json!(true);
        }
        if let Some(min_size) = filter.min_size {
            jmap_filter["minSize"] = json!(min_size);
        }
        if let Some(max_size) = filter.max_size {
            jmap_filter["maxSize"] = json!(max_size);
        }
        if let Some(ref before) = filter.before {
            // Normalize date to ISO 8601 if needed
            let date = if before.contains('T') {
                before.clone()
            } else {
                format!("{}T00:00:00Z", before)
            };
            jmap_filter["before"] = json!(date);
        }
        if let Some(ref after) = filter.after {
            let date = if after.contains('T') {
                after.clone()
            } else {
                format!("{}T00:00:00Z", after)
            };
            jmap_filter["after"] = json!(date);
        }
        if filter.unread {
            jmap_filter["notKeyword"] = json!("$seen");
        }
        if filter.flagged {
            jmap_filter["hasKeyword"] = json!("$flagged");
        }

        let responses = self
            .request(vec![
                json!([
                    "Email/query",
                    {
                        "accountId": account_id,
                        "filter": jmap_filter,
                        "sort": [{"property": "receivedAt", "isAscending": false}],
                        "limit": limit
                    },
                    "q0"
                ]),
                json!([
                    "Email/get",
                    {
                        "accountId": account_id,
                        "#ids": {
                            "resultOf": "q0",
                            "name": "Email/query",
                            "path": "/ids"
                        },
                        "properties": [
                            "id", "threadId", "mailboxIds", "keywords",
                            "size", "receivedAt", "from", "to", "cc",
                            "subject", "preview", "hasAttachment"
                        ]
                    },
                    "g0"
                ]),
            ])
            .await?;

        let resp: GetResponse<Email> =
            Self::parse_response(responses.get(1).unwrap_or(&Value::Null), "Email/get")?;

        Ok(resp.list)
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
        let url = session
            .download_url
            .replace("{accountId}", account_id)
            .replace("{blobId}", blob_id)
            .replace("{name}", "attachment")
            .replace("{type}", "application/octet-stream");

        debug!(url = %url, "Downloading blob");
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await?;

        match resp.status().as_u16() {
            401 => return Err(Error::InvalidToken("Token expired or invalid".into())),
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

        let url = session.upload_url.replace("{accountId}", account_id);

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
            401 => return Err(Error::InvalidToken("Token expired or invalid".into())),
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

    /// Send a reply to an existing email with proper threading headers
    #[instrument(skip(self, body, params))]
    pub async fn reply_email(
        &mut self,
        original: &Email,
        body: &str,
        reply_all: bool,
        params: ComposeParams<'_>,
    ) -> Result<String> {
        let ctx = self.prepare_compose(params.from, params.draft).await?;

        let my_email = ctx
            .identity
            .as_ref()
            .map(|i| i.email.to_lowercase())
            .or_else(|| params.from.map(|f| f.to_lowercase()))
            .unwrap_or_default();

        // Build To: reply to sender, or if reply_all, include original recipients
        let mut to_addrs: Vec<EmailAddress> = original.from.clone().unwrap_or_default();

        if reply_all {
            // Add original To recipients (except ourselves)
            if let Some(ref orig_to) = original.to {
                for addr in orig_to {
                    if my_email.is_empty() || addr.email.to_lowercase() != my_email {
                        to_addrs.push(addr.clone());
                    }
                }
            }
        }

        // Build CC: include original CC recipients (if reply_all) plus any new CC
        let mut cc_addrs = params.cc;
        if reply_all && let Some(ref orig_cc) = original.cc {
            for addr in orig_cc {
                if my_email.is_empty() || addr.email.to_lowercase() != my_email {
                    cc_addrs.push(addr.clone());
                }
            }
        }

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
        let original_body = original.text_content().unwrap_or("");

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

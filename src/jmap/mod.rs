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

const CAPABILITIES: &[&str] = &[
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "https://www.fastmail.com/dev/maskedemail",
];

pub struct JmapClient {
    client: Client,
    token: String,
    session: Option<Session>,
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
        self.session = Some(session);
        Ok(self.session.as_ref().unwrap())
    }

    pub fn session(&self) -> Result<&Session> {
        self.session.as_ref().ok_or(Error::NotAuthenticated)
    }

    #[instrument(skip(self, method_calls))]
    async fn request(&self, method_calls: Vec<Value>) -> Result<Vec<Value>> {
        let session = self.session()?;
        let req = JmapRequest {
            using: CAPABILITIES.iter().map(|s| s.to_string()).collect(),
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

        let jmap_resp: JmapResponse = resp.json().await?;
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
    pub async fn list_mailboxes(&self) -> Result<Vec<Mailbox>> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct MailboxGetResponse {
            list: Vec<Mailbox>,
        }

        let resp: MailboxGetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Mailbox/get")?;

        Ok(resp.list)
    }

    pub async fn find_mailbox(&self, name: &str) -> Result<Mailbox> {
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
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct EmailGetResponse {
            list: Vec<Email>,
        }

        let resp: EmailGetResponse =
            Self::parse_response(responses.get(1).unwrap_or(&Value::Null), "Email/get")?;

        Ok(resp.list)
    }

    #[instrument(skip(self))]
    pub async fn get_email(&self, email_id: &str) -> Result<Email> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct EmailGetResponse {
            list: Vec<Email>,
            #[serde(rename = "notFound")]
            not_found: Vec<String>,
        }

        let resp: EmailGetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;

        if !resp.not_found.is_empty() {
            return Err(Error::EmailNotFound(email_id.into()));
        }

        resp.list
            .into_iter()
            .next()
            .ok_or_else(|| Error::EmailNotFound(email_id.into()))
    }

    /// Search emails with full JMAP filter support
    #[instrument(skip(self, filter))]
    pub async fn search_emails_filtered(
        &self,
        filter: &SearchFilter,
        mailbox_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Email>> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct EmailGetResponse {
            list: Vec<Email>,
        }

        let resp: EmailGetResponse =
            Self::parse_response(responses.get(1).unwrap_or(&Value::Null), "Email/get")?;

        Ok(resp.list)
    }

    #[instrument(skip(self))]
    pub async fn list_identities(&self) -> Result<Vec<Identity>> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

        let responses = self
            .request(vec![json!([
                "Identity/get",
                { "accountId": account_id },
                "i0"
            ])])
            .await?;

        #[derive(Deserialize)]
        struct IdentityGetResponse {
            list: Vec<Identity>,
        }

        let resp: IdentityGetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Identity/get")?;

        Ok(resp.list)
    }

    #[instrument(skip(self, body))]
    pub async fn send_email(
        &self,
        to: Vec<EmailAddress>,
        cc: Vec<EmailAddress>,
        bcc: Vec<EmailAddress>,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
    ) -> Result<String> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

        let identities = self.list_identities().await?;
        let identity = identities.first().ok_or(Error::IdentityNotFound)?;

        let drafts = self.find_mailbox("drafts").await?;
        let sent = self.find_mailbox("sent").await?;

        let mut email_create: HashMap<String, Value> = HashMap::new();
        email_create.insert("mailboxIds".into(), json!({ drafts.id.clone(): true }));
        email_create.insert(
            "from".into(),
            json!([{ "email": identity.email, "name": identity.name }]),
        );
        email_create.insert(
            "to".into(),
            json!(
                to.iter()
                    .map(|a| json!({"email": a.email, "name": a.name}))
                    .collect::<Vec<_>>()
            ),
        );
        if !cc.is_empty() {
            email_create.insert(
                "cc".into(),
                json!(
                    cc.iter()
                        .map(|a| json!({"email": a.email, "name": a.name}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if !bcc.is_empty() {
            email_create.insert(
                "bcc".into(),
                json!(
                    bcc.iter()
                        .map(|a| json!({"email": a.email, "name": a.name}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        email_create.insert("subject".into(), json!(subject));
        email_create.insert(
            "bodyValues".into(),
            json!({ "body": { "value": body, "charset": "utf-8" } }),
        );
        email_create.insert(
            "textBody".into(),
            json!([{ "partId": "body", "type": "text/plain" }]),
        );
        email_create.insert("keywords".into(), json!({ "$draft": true }));

        if let Some(reply_id) = in_reply_to {
            email_create.insert("inReplyTo".into(), json!([reply_id]));
        }

        let responses = self
            .request(vec![
                json!([
                    "Email/set",
                    {
                        "accountId": account_id,
                        "create": { "draft": email_create }
                    },
                    "e0"
                ]),
                json!([
                    "EmailSubmission/set",
                    {
                        "accountId": account_id,
                        "create": {
                            "submission": {
                                "identityId": identity.id,
                                "emailId": "#draft"
                            }
                        },
                        "onSuccessUpdateEmail": {
                            "#submission": {
                                "mailboxIds": { sent.id.clone(): true },
                                "keywords": { "$draft": null, "$seen": true }
                            }
                        }
                    },
                    "s0"
                ]),
            ])
            .await?;

        #[derive(Deserialize)]
        struct EmailSetResponse {
            created: Option<HashMap<String, Value>>,
            #[serde(rename = "notCreated")]
            not_created: Option<HashMap<String, Value>>,
        }

        let email_resp: EmailSetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_created) = email_resp.not_created
            && let Some(err) = not_created.get("draft")
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

        let email_id = email_resp
            .created
            .and_then(|c: HashMap<String, Value>| c.get("draft").cloned())
            .and_then(|d: Value| {
                d.get("id")
                    .and_then(|v: &Value| v.as_str())
                    .map(String::from)
            })
            .ok_or_else(|| Error::Jmap {
                method: "Email/set".into(),
                error_type: "unknown".into(),
                description: "No email ID returned".into(),
            })?;

        debug!(email_id = %email_id, "Email sent successfully");
        Ok(email_id)
    }

    #[instrument(skip(self))]
    pub async fn move_email(&self, email_id: &str, mailbox_id: &str) -> Result<()> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct SetResponse {
            #[serde(rename = "notUpdated")]
            not_updated: Option<HashMap<String, Value>>,
        }

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
    pub async fn mark_spam(&self, email_id: &str) -> Result<()> {
        let junk = self.find_mailbox("junk").await?;
        self.move_email(email_id, &junk.id).await
    }

    /// Download a blob (attachment) by ID
    #[instrument(skip(self))]
    pub async fn download_blob(&self, blob_id: &str) -> Result<Vec<u8>> {
        let session = self.session()?;
        let account_id = session
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

    /// Send a reply to an existing email with proper threading headers
    #[instrument(skip(self, body))]
    pub async fn reply_email(
        &self,
        original: &Email,
        body: &str,
        reply_all: bool,
        cc: Vec<EmailAddress>,
        bcc: Vec<EmailAddress>,
    ) -> Result<String> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

        let identities = self.list_identities().await?;
        let identity = identities.first().ok_or(Error::IdentityNotFound)?;
        let my_email = identity.email.to_lowercase();

        let drafts = self.find_mailbox("drafts").await?;
        let sent = self.find_mailbox("sent").await?;

        // Build To: reply to sender, or if reply_all, include original recipients
        let mut to_addrs: Vec<EmailAddress> = original.from.clone().unwrap_or_default();

        if reply_all {
            // Add original To recipients (except ourselves)
            if let Some(ref orig_to) = original.to {
                for addr in orig_to {
                    if addr.email.to_lowercase() != my_email {
                        to_addrs.push(addr.clone());
                    }
                }
            }
        }

        // Build CC: include original CC recipients (if reply_all) plus any new CC
        let mut cc_addrs = cc;
        if reply_all && let Some(ref orig_cc) = original.cc {
            for addr in orig_cc {
                if addr.email.to_lowercase() != my_email {
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

        let mut email_create: HashMap<String, Value> = HashMap::new();
        email_create.insert("mailboxIds".into(), json!({ drafts.id.clone(): true }));
        email_create.insert(
            "from".into(),
            json!([{ "email": identity.email, "name": identity.name }]),
        );
        email_create.insert(
            "to".into(),
            json!(
                to_addrs
                    .iter()
                    .map(|a| json!({"email": a.email, "name": a.name}))
                    .collect::<Vec<_>>()
            ),
        );
        if !cc_addrs.is_empty() {
            email_create.insert(
                "cc".into(),
                json!(
                    cc_addrs
                        .iter()
                        .map(|a| json!({"email": a.email, "name": a.name}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if !bcc.is_empty() {
            email_create.insert(
                "bcc".into(),
                json!(
                    bcc.iter()
                        .map(|a| json!({"email": a.email, "name": a.name}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        email_create.insert("subject".into(), json!(subject));
        email_create.insert(
            "bodyValues".into(),
            json!({ "body": { "value": body, "charset": "utf-8" } }),
        );
        email_create.insert(
            "textBody".into(),
            json!([{ "partId": "body", "type": "text/plain" }]),
        );
        email_create.insert("keywords".into(), json!({ "$draft": true }));

        // Threading headers
        if let Some(ref msg_id) = original.message_id {
            email_create.insert("inReplyTo".into(), json!(msg_id));
        }
        if !references.is_empty() {
            email_create.insert("references".into(), json!(references));
        }

        let responses = self
            .request(vec![
                json!([
                    "Email/set",
                    {
                        "accountId": account_id,
                        "create": { "draft": email_create }
                    },
                    "e0"
                ]),
                json!([
                    "EmailSubmission/set",
                    {
                        "accountId": account_id,
                        "create": {
                            "submission": {
                                "identityId": identity.id,
                                "emailId": "#draft"
                            }
                        },
                        "onSuccessUpdateEmail": {
                            "#submission": {
                                "mailboxIds": { sent.id.clone(): true },
                                "keywords": { "$draft": null, "$seen": true }
                            }
                        }
                    },
                    "s0"
                ]),
            ])
            .await?;

        #[derive(Deserialize)]
        struct EmailSetResponse {
            created: Option<HashMap<String, Value>>,
            #[serde(rename = "notCreated")]
            not_created: Option<HashMap<String, Value>>,
        }

        let email_resp: EmailSetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_created) = email_resp.not_created
            && let Some(err) = not_created.get("draft")
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

        let email_id = email_resp
            .created
            .and_then(|c: HashMap<String, Value>| c.get("draft").cloned())
            .and_then(|d: Value| {
                d.get("id")
                    .and_then(|v: &Value| v.as_str())
                    .map(String::from)
            })
            .ok_or_else(|| Error::Jmap {
                method: "Email/set".into(),
                error_type: "unknown".into(),
                description: "No email ID returned".into(),
            })?;

        debug!(email_id = %email_id, "Reply sent successfully");
        Ok(email_id)
    }

    /// Forward an email with proper attribution
    #[instrument(skip(self, body))]
    pub async fn forward_email(
        &self,
        original: &Email,
        to: Vec<EmailAddress>,
        body: &str,
        cc: Vec<EmailAddress>,
        bcc: Vec<EmailAddress>,
    ) -> Result<String> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

        let identities = self.list_identities().await?;
        let identity = identities.first().ok_or(Error::IdentityNotFound)?;

        let drafts = self.find_mailbox("drafts").await?;
        let sent = self.find_mailbox("sent").await?;

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
        let original_body = original
            .body_values
            .as_ref()
            .and_then(|bv| bv.values().next())
            .map(|v| v.value.as_str())
            .unwrap_or("");

        let sender = original
            .from
            .as_ref()
            .and_then(|f| f.first())
            .map(|a| {
                if let Some(ref name) = a.name {
                    format!("{} <{}>", name, a.email)
                } else {
                    a.email.clone()
                }
            })
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

        let mut email_create: HashMap<String, Value> = HashMap::new();
        email_create.insert("mailboxIds".into(), json!({ drafts.id.clone(): true }));
        email_create.insert(
            "from".into(),
            json!([{ "email": identity.email, "name": identity.name }]),
        );
        email_create.insert(
            "to".into(),
            json!(
                to.iter()
                    .map(|a| json!({"email": a.email, "name": a.name}))
                    .collect::<Vec<_>>()
            ),
        );
        if !cc.is_empty() {
            email_create.insert(
                "cc".into(),
                json!(
                    cc.iter()
                        .map(|a| json!({"email": a.email, "name": a.name}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if !bcc.is_empty() {
            email_create.insert(
                "bcc".into(),
                json!(
                    bcc.iter()
                        .map(|a| json!({"email": a.email, "name": a.name}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        email_create.insert("subject".into(), json!(subject));
        email_create.insert(
            "bodyValues".into(),
            json!({ "body": { "value": full_body, "charset": "utf-8" } }),
        );
        email_create.insert(
            "textBody".into(),
            json!([{ "partId": "body", "type": "text/plain" }]),
        );
        email_create.insert("keywords".into(), json!({ "$draft": true }));

        let responses = self
            .request(vec![
                json!([
                    "Email/set",
                    {
                        "accountId": account_id,
                        "create": { "draft": email_create }
                    },
                    "e0"
                ]),
                json!([
                    "EmailSubmission/set",
                    {
                        "accountId": account_id,
                        "create": {
                            "submission": {
                                "identityId": identity.id,
                                "emailId": "#draft"
                            }
                        },
                        "onSuccessUpdateEmail": {
                            "#submission": {
                                "mailboxIds": { sent.id.clone(): true },
                                "keywords": { "$draft": null, "$seen": true }
                            }
                        }
                    },
                    "s0"
                ]),
            ])
            .await?;

        #[derive(Deserialize)]
        struct EmailSetResponse {
            created: Option<HashMap<String, Value>>,
            #[serde(rename = "notCreated")]
            not_created: Option<HashMap<String, Value>>,
        }

        let email_resp: EmailSetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_created) = email_resp.not_created
            && let Some(err) = not_created.get("draft")
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

        let email_id = email_resp
            .created
            .and_then(|c: HashMap<String, Value>| c.get("draft").cloned())
            .and_then(|d: Value| {
                d.get("id")
                    .and_then(|v: &Value| v.as_str())
                    .map(String::from)
            })
            .ok_or_else(|| Error::Jmap {
                method: "Email/set".into(),
                error_type: "unknown".into(),
                description: "No email ID returned".into(),
            })?;

        debug!(email_id = %email_id, "Forward sent successfully");
        Ok(email_id)
    }

    #[allow(dead_code)]
    #[instrument(skip(self))]
    pub async fn set_keywords(
        &self,
        email_id: &str,
        keywords: HashMap<String, bool>,
    ) -> Result<()> {
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct SetResponse {
            #[serde(rename = "notUpdated")]
            not_updated: Option<HashMap<String, Value>>,
        }

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
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct MaskedEmailGetResponse {
            list: Vec<MaskedEmail>,
        }

        let resp: MaskedEmailGetResponse =
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
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct MaskedEmailSetResponse {
            created: Option<HashMap<String, MaskedEmail>>,
            #[serde(rename = "notCreated")]
            not_created: Option<HashMap<String, Value>>,
        }

        let resp: MaskedEmailSetResponse =
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
        let account_id = self
            .session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))?;

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

        #[derive(Deserialize)]
        struct SetResponse {
            #[serde(rename = "notUpdated")]
            not_updated: Option<HashMap<String, Value>>,
        }

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

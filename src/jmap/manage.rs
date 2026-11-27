//! The rest of RFC 8621 a mail agent wants: destroying email, scheduled
//! submissions, identities, importing and parsing messages, and search
//! snippets.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracing::instrument;

use super::JmapClient;
use crate::error::{Error, Result};
use crate::models::{Email, Identity};

/// IDs per `Email/set` destroy or `Email/get` of mailbox memberships.
const CHUNK: usize = 500;

const SUBMISSION: &str = "urn:ietf:params:jmap:submission";

/// What a bulk destroy did, as [`super::lists::UpdateOutcome`] does for updates.
#[derive(Debug, Default)]
pub struct DestroyOutcome {
    pub destroyed: usize,
    pub failed: Vec<(String, String)>,
}

/// A pending (or past) `EmailSubmission`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    pub id: String,
    pub email_id: String,
    #[serde(default)]
    pub identity_id: Option<String>,
    #[serde(default)]
    pub send_at: Option<String>,
    #[serde(default)]
    pub undo_status: Option<String>,
    #[serde(default)]
    pub envelope: Option<Value>,
}

impl Submission {
    /// The addresses the envelope delivers to, which include BCC recipients.
    pub fn recipients(&self) -> Vec<String> {
        self.envelope
            .as_ref()
            .and_then(|e| e.get("rcptTo"))
            .and_then(Value::as_array)
            .map(|r| {
                r.iter()
                    .filter_map(|a| a.get("email").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Fields to set on an identity. `None` leaves one alone; an empty string or
/// list clears it.
#[derive(Debug, Default, Clone)]
pub struct IdentityFields {
    pub name: Option<String>,
    pub reply_to: Option<Vec<crate::models::EmailAddress>>,
    pub bcc: Option<Vec<crate::models::EmailAddress>>,
    pub text_signature: Option<String>,
    pub html_signature: Option<String>,
}

impl IdentityFields {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.reply_to.is_none()
            && self.bcc.is_none()
            && self.text_signature.is_none()
            && self.html_signature.is_none()
    }

    fn to_json(&self) -> Map<String, Value> {
        let addrs = |list: &[crate::models::EmailAddress]| -> Value {
            if list.is_empty() {
                Value::Null
            } else {
                json!(
                    list.iter()
                        .map(|a| json!({ "name": a.name, "email": a.email }))
                        .collect::<Vec<_>>()
                )
            }
        };
        let mut out = Map::new();
        if let Some(v) = &self.name {
            out.insert("name".into(), json!(v));
        }
        if let Some(v) = &self.reply_to {
            out.insert("replyTo".into(), addrs(v));
        }
        if let Some(v) = &self.bcc {
            out.insert("bcc".into(), addrs(v));
        }
        if let Some(v) = &self.text_signature {
            out.insert("textSignature".into(), json!(v));
        }
        if let Some(v) = &self.html_signature {
            out.insert("htmlSignature".into(), json!(v));
        }
        out
    }
}

/// A search snippet: the subject and a body excerpt with the matched terms
/// wrapped in `<mark>`. Either is null when the search matched nothing there.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub email_id: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetOutcome {
    #[serde(default)]
    created: Option<HashMap<String, Value>>,
    #[serde(default)]
    not_created: Option<HashMap<String, Value>>,
    #[serde(default)]
    not_updated: Option<HashMap<String, Value>>,
    #[serde(default)]
    not_destroyed: Option<HashMap<String, Value>>,
}

fn set_error(method: &str, err: &Value, fallback: &str) -> Error {
    let mut description = err
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string();
    if let Some(props) = err.get("properties").and_then(Value::as_array) {
        let props: Vec<&str> = props.iter().filter_map(Value::as_str).collect();
        if !props.is_empty() {
            description.push_str(&format!(" (properties: {})", props.join(", ")));
        }
    }
    Error::Jmap {
        method: method.into(),
        error_type: err
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .into(),
        description,
    }
}

fn reason(err: &Value) -> String {
    err.get("description")
        .or_else(|| err.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("refused")
        .to_string()
}

/// Seconds since the Unix epoch of an RFC 3339 date-time with a `Z` or a
/// numeric offset, such as `2026-11-28T09:00:00+00:00`. A time without an
/// offset is refused: which zone it meant is a guess.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let mut d = date.splitn(3, '-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let (time, offset) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (t, off) = rest.split_at(at);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        // `+01:00`, and the ISO 8601 basic forms `+0100` and `+01`.
        let digits = &off[1..];
        let (h, m) = match (digits.split_once(':'), digits.len()) {
            (Some((h, m)), _) => (h, m),
            (None, 4) => digits.split_at(2),
            (None, 2) => (digits, "0"),
            _ => return None,
        };
        let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
        if h > 23 || m > 59 {
            return None;
        }
        (t, sign * (h * 3600 + m * 60))
    };
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: f64 = t.next().unwrap_or("0").parse().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=12).contains(&month)
        || !(1..=month_days).contains(&day)
        || hour > 23
        || minute > 59
        || second >= 61.0
    {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second as i64 - offset)
}

/// An epoch second as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl JmapClient {
    /// Check a requested send time against what the server allows, and give
    /// it back in UTC for the `HOLDUNTIL` parameter.
    pub fn validate_send_at(&self, send_at: &str) -> Result<String> {
        let session = self.session()?;
        let cap = session.capabilities.get(SUBMISSION).ok_or_else(|| {
            Error::Config("Scheduled sending needs the submission capability".into())
        })?;
        let max_delay = cap
            .get("maxDelayedSend")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let future_release = cap
            .get("submissionExtensions")
            .is_some_and(|e| e.get("FUTURERELEASE").is_some());
        if max_delay <= 0 || !future_release {
            return Err(Error::Config(
                "This server does not offer scheduled sending (no maxDelayedSend or FUTURERELEASE)"
                    .into(),
            ));
        }
        let at = parse_rfc3339(send_at).ok_or_else(|| {
            Error::Config(format!(
                "sendAt \"{send_at}\" is not an RFC 3339 time with an offset, \
                 e.g. 2026-11-28T09:00:00Z or 2026-11-28T09:00:00+01:00"
            ))
        })?;
        let delay = at - now_epoch();
        if delay <= 0 {
            return Err(Error::Config(format!(
                "sendAt {} is in the past",
                format_utc(at)
            )));
        }
        if delay > max_delay {
            return Err(Error::Config(format!(
                "sendAt {} is further ahead than the server allows ({} days)",
                format_utc(at),
                max_delay / 86_400
            )));
        }
        Ok(format_utc(at))
    }

    /// Destroy emails for good: removed from every mailbox, not moved to
    /// Trash. Refusals are collected rather than failing the batch.
    #[instrument(skip(self, ids))]
    pub async fn destroy_emails(&self, ids: &[String]) -> Result<DestroyOutcome> {
        let account_id = self.account_id()?;
        let mut outcome = DestroyOutcome::default();
        for chunk in ids.chunks(CHUNK) {
            let responses = self
                .request(vec![json!([
                    "Email/set",
                    { "accountId": account_id, "destroy": chunk },
                    "d0"
                ])])
                .await?;
            let resp: SetOutcome =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;
            let refused = resp.not_destroyed.unwrap_or_default();
            for id in chunk {
                match refused.get(id) {
                    Some(err) => outcome.failed.push((id.clone(), reason(err))),
                    None => outcome.destroyed += 1,
                }
            }
        }
        Ok(outcome)
    }

    /// Which mailboxes each email is in, for deciding whether emptying a
    /// folder destroys an email or only takes it out of that folder.
    #[instrument(skip(self, ids))]
    pub async fn email_mailbox_ids(&self, ids: &[String]) -> Result<HashMap<String, Vec<String>>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Membership {
            id: String,
            #[serde(default)]
            mailbox_ids: HashMap<String, bool>,
        }
        let account_id = self.account_id()?;
        let mut out = HashMap::new();
        for chunk in ids.chunks(CHUNK) {
            let responses = self
                .request(vec![json!([
                    "Email/get",
                    { "accountId": account_id, "ids": chunk, "properties": ["id", "mailboxIds"] },
                    "g0"
                ])])
                .await?;
            let got: super::GetResponse<Membership> =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;
            for m in got.list {
                out.insert(
                    m.id,
                    m.mailbox_ids
                        .into_iter()
                        .filter(|(_, on)| *on)
                        .map(|(id, _)| id)
                        .collect(),
                );
            }
        }
        Ok(out)
    }

    /// Submissions the server is still holding to send later, soonest first.
    #[instrument(skip(self))]
    pub async fn pending_submissions(&self) -> Result<Vec<Submission>> {
        self.require_capability(SUBMISSION, "Scheduled sending")?;
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![
                json!([
                    "EmailSubmission/query",
                    {
                        "accountId": account_id,
                        "filter": { "undoStatus": "pending" },
                        "sort": [{ "property": "sendAt", "isAscending": true }]
                    },
                    "q0"
                ]),
                json!([
                    "EmailSubmission/get",
                    {
                        "accountId": account_id,
                        "#ids": { "resultOf": "q0", "name": "EmailSubmission/query", "path": "/ids" }
                    },
                    "g0"
                ]),
            ])
            .await?;
        Self::parse_response::<serde::de::IgnoredAny>(
            responses.first().unwrap_or(&Value::Null),
            "EmailSubmission/query",
        )?;
        let got: super::GetResponse<Submission> = Self::parse_response(
            responses.get(1).unwrap_or(&Value::Null),
            "EmailSubmission/get",
        )?;
        Ok(got.list)
    }

    /// Stop a pending submission before it goes, and put its email back in
    /// Drafts as a draft so it can be edited or sent again.
    #[instrument(skip(self))]
    pub async fn cancel_submission(&self, submission_id: &str) -> Result<()> {
        self.require_capability(SUBMISSION, "Scheduled sending")?;
        let drafts = self.find_mailbox("drafts").await?;
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "EmailSubmission/set",
                {
                    "accountId": account_id,
                    "update": { (submission_id): { "undoStatus": "canceled" } },
                    "onSuccessUpdateEmail": {
                        (submission_id): {
                            "mailboxIds": { (drafts.id): true },
                            "keywords/$draft": true
                        }
                    }
                },
                "s0"
            ])])
            .await?;
        let resp: SetOutcome = Self::parse_response(
            responses.first().unwrap_or(&Value::Null),
            "EmailSubmission/set",
        )?;
        match resp.not_updated.as_ref().and_then(|m| m.get(submission_id)) {
            Some(err) => Err(set_error(
                "EmailSubmission/set",
                err,
                "Could not cancel the submission",
            )),
            None => Ok(()),
        }
    }

    /// Create a sending identity for an address the account may send from.
    #[instrument(skip(self, fields))]
    pub async fn create_identity(&self, email: &str, fields: &IdentityFields) -> Result<Identity> {
        self.require_capability(SUBMISSION, "Identities")?;
        let mut create = fields.to_json();
        create.insert("email".into(), json!(email));
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "Identity/set",
                { "accountId": account_id, "create": { "new": create } },
                "i0"
            ])])
            .await?;
        let resp: SetOutcome =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Identity/set")?;
        if let Some(err) = resp.not_created.as_ref().and_then(|m| m.get("new")) {
            return Err(set_error("Identity/set", err, "Could not create identity"));
        }
        let id = resp
            .created
            .as_ref()
            .and_then(|c| c.get("new"))
            .and_then(|n| n.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| set_error("Identity/set", &Value::Null, "No identity created"))?;
        self.list_identities()
            .await?
            .into_iter()
            .find(|i| i.id == id)
            .ok_or(Error::IdentityNotFound)
    }

    #[instrument(skip(self, fields))]
    pub async fn update_identity(&self, id: &str, fields: &IdentityFields) -> Result<()> {
        self.require_capability(SUBMISSION, "Identities")?;
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "Identity/set",
                { "accountId": account_id, "update": { (id): fields.to_json() } },
                "i0"
            ])])
            .await?;
        let resp: SetOutcome =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Identity/set")?;
        match resp.not_updated.as_ref().and_then(|m| m.get(id)) {
            Some(err) => Err(set_error("Identity/set", err, "Could not update identity")),
            None => Ok(()),
        }
    }

    #[instrument(skip(self))]
    pub async fn destroy_identity(&self, id: &str) -> Result<()> {
        self.require_capability(SUBMISSION, "Identities")?;
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "Identity/set",
                { "accountId": account_id, "destroy": [id] },
                "i0"
            ])])
            .await?;
        let resp: SetOutcome =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Identity/set")?;
        match resp.not_destroyed.as_ref().and_then(|m| m.get(id)) {
            Some(err) => Err(set_error("Identity/set", err, "Could not delete identity")),
            None => Ok(()),
        }
    }

    /// Import an RFC 5322 message already uploaded as a blob into mailboxes,
    /// returning the new email's ID. Nothing is sent.
    #[instrument(skip(self, keywords))]
    pub async fn import_email(
        &self,
        blob_id: &str,
        mailbox_ids: &[String],
        keywords: &[String],
        received_at: Option<&str>,
    ) -> Result<String> {
        let account_id = self.account_id()?;
        let mut import = Map::new();
        import.insert("blobId".into(), json!(blob_id));
        import.insert(
            "mailboxIds".into(),
            Value::Object(
                mailbox_ids
                    .iter()
                    .map(|m| (m.clone(), json!(true)))
                    .collect(),
            ),
        );
        import.insert(
            "keywords".into(),
            Value::Object(keywords.iter().map(|k| (k.clone(), json!(true))).collect()),
        );
        if let Some(at) = received_at {
            import.insert("receivedAt".into(), json!(at));
        }
        let responses = self
            .request(vec![json!([
                "Email/import",
                { "accountId": account_id, "emails": { "new": import } },
                "i0"
            ])])
            .await?;
        let resp: SetOutcome =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/import")?;
        if let Some(err) = resp.not_created.as_ref().and_then(|m| m.get("new")) {
            return Err(set_error(
                "Email/import",
                err,
                "Could not import the message",
            ));
        }
        resp.created
            .as_ref()
            .and_then(|c| c.get("new"))
            .and_then(|n| n.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| set_error("Email/import", &Value::Null, "No email imported"))
    }

    /// Parse message blobs (`message/rfc822` attachments, uploaded `.eml`
    /// files) as emails without importing them. A parsed email has no ID of
    /// its own, so it carries its blob ID as one.
    #[instrument(skip(self, blob_ids))]
    pub async fn parse_emails(&self, blob_ids: &[String]) -> Result<HashMap<String, Email>> {
        let account_id = self.account_id()?;
        let properties: Vec<&str> = super::EMAIL_FULL_PROPERTIES
            .iter()
            .copied()
            .filter(|p| {
                !matches!(
                    *p,
                    "id" | "threadId" | "mailboxIds" | "keywords" | "receivedAt"
                )
            })
            .collect();
        let responses = self
            .request(vec![json!([
                "Email/parse",
                {
                    "accountId": account_id,
                    "blobIds": blob_ids,
                    "properties": properties,
                    "fetchAllBodyValues": true
                },
                "p0"
            ])])
            .await?;
        #[derive(Deserialize)]
        struct Parsed {
            #[serde(default)]
            parsed: HashMap<String, Value>,
        }
        let resp: Parsed =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/parse")?;
        let mut out = HashMap::new();
        for (blob_id, mut email) in resp.parsed {
            if let Some(obj) = email.as_object_mut() {
                obj.insert("id".into(), json!(blob_id));
                // Not in the mailbox, so not unread: no keyword field should
                // suggest it needs attention.
                obj.insert("keywords".into(), json!({ "$seen": true }));
                obj.entry("blobId").or_insert_with(|| json!(blob_id));
            }
            let email: Email = serde_json::from_value(email).map_err(|e| Error::Jmap {
                method: "Email/parse".into(),
                error_type: "parse".into(),
                description: e.to_string(),
            })?;
            out.insert(blob_id, email);
        }
        Ok(out)
    }

    /// Highlighted excerpts of why each email matched `filter`.
    #[instrument(skip(self, filter, email_ids))]
    pub async fn search_snippets(
        &self,
        filter: &Value,
        email_ids: &[String],
    ) -> Result<Vec<Snippet>> {
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "SearchSnippet/get",
                { "accountId": account_id, "filter": filter, "emailIds": email_ids },
                "s0"
            ])])
            .await?;
        let got: super::GetResponse<Snippet> = Self::parse_response(
            responses.first().unwrap_or(&Value::Null),
            "SearchSnippet/get",
        )?;
        Ok(got.list)
    }

    /// Change a masked email's state and notes. `None` leaves a field alone.
    #[instrument(skip(self))]
    pub async fn edit_masked_email(
        &self,
        id: &str,
        for_domain: Option<&str>,
        description: Option<&str>,
        url: Option<&str>,
    ) -> Result<()> {
        self.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email")?;
        let account_id = self.account_id()?;
        let mut patch = Map::new();
        if let Some(v) = for_domain {
            patch.insert("forDomain".into(), json!(v));
        }
        if let Some(v) = description {
            patch.insert("description".into(), json!(v));
        }
        if let Some(v) = url {
            patch.insert(
                "url".into(),
                if v.is_empty() { Value::Null } else { json!(v) },
            );
        }
        let responses = self
            .request(vec![json!([
                "MaskedEmail/set",
                { "accountId": account_id, "update": { (id): patch } },
                "me0"
            ])])
            .await?;
        let resp: SetOutcome =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "MaskedEmail/set")?;
        match resp.not_updated.as_ref().and_then(|m| m.get(id)) {
            Some(err) => Err(set_error(
                "MaskedEmail/set",
                err,
                "Could not update masked email",
            )),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_through_utc() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        let at = parse_rfc3339("2026-11-28T09:30:00+01:00").unwrap();
        assert_eq!(format_utc(at), "2026-11-28T08:30:00Z");
        assert_eq!(
            format_utc(parse_rfc3339("2024-02-29T23:59:59-05:30").unwrap()),
            "2024-03-01T05:29:59Z"
        );
        assert_eq!(
            parse_rfc3339("2026-11-28T09:30:00.250Z"),
            parse_rfc3339("2026-11-28T09:30:00Z")
        );
    }

    #[test]
    fn a_time_without_an_offset_is_refused() {
        assert_eq!(parse_rfc3339("2026-11-28T09:30:00"), None);
        assert_eq!(parse_rfc3339("2026-11-28"), None);
        assert_eq!(parse_rfc3339("2026-13-28T09:30:00Z"), None);
        assert_eq!(parse_rfc3339("2026-02-29T09:30:00Z"), None);
        assert_eq!(parse_rfc3339("2026-11-28T09:30:00+2500"), None);
    }

    #[test]
    fn basic_offsets_read_as_hours_and_minutes() {
        let want = parse_rfc3339("2026-11-28T09:00:00+01:00");
        assert!(want.is_some());
        assert_eq!(parse_rfc3339("2026-11-28T09:00:00+0100"), want);
        assert_eq!(parse_rfc3339("2026-11-28T09:00:00+01"), want);
        assert_eq!(
            parse_rfc3339("2024-02-29T00:00:00Z")
                .map(format_utc)
                .as_deref(),
            Some("2024-02-29T00:00:00Z")
        );
    }

    #[test]
    fn identity_fields_clear_with_empty_lists() {
        let fields = IdentityFields {
            reply_to: Some(vec![]),
            text_signature: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(
            Value::Object(fields.to_json()),
            json!({ "replyTo": null, "textSignature": "" })
        );
    }
}

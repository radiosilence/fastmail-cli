use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub capabilities: HashMap<String, serde_json::Value>,
    pub accounts: HashMap<String, Account>,
    pub primary_accounts: HashMap<String, String>,
    pub username: String,
    pub api_url: String,
    pub download_url: String,
    pub upload_url: String,
    #[serde(default)]
    pub event_source_url: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

impl Session {
    pub fn primary_account_id(&self) -> Option<&str> {
        self.primary_accounts
            .get("urn:ietf:params:jmap:mail")
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub name: String,
    pub is_personal: bool,
    pub is_read_only: bool,
    #[serde(default)]
    pub account_capabilities: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    pub email: String,
}

impl std::fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(name) if !name.is_empty() => write!(f, "{} <{}>", name, self.email),
            _ => write!(f, "{}", self.email),
        }
    }
}

/// What the account may do in a mailbox (RFC 8621 §2). Fastmail returns this
/// per mailbox; it is the difference between "this move will work" and finding
/// out from a failed `Email/set`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxRights {
    #[serde(default)]
    pub may_read_items: bool,
    #[serde(default)]
    pub may_add_items: bool,
    #[serde(default)]
    pub may_remove_items: bool,
    #[serde(default)]
    pub may_set_seen: bool,
    #[serde(default)]
    pub may_set_keywords: bool,
    #[serde(default)]
    pub may_create_child: bool,
    #[serde(default)]
    pub may_rename: bool,
    #[serde(default)]
    pub may_delete: bool,
    #[serde(default)]
    pub may_submit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub total_emails: u32,
    #[serde(default)]
    pub unread_emails: u32,
    #[serde(default)]
    pub total_threads: u32,
    #[serde(default)]
    pub unread_threads: u32,
    #[serde(default)]
    pub sort_order: u32,
    #[serde(default)]
    pub is_subscribed: bool,
    #[serde(default)]
    pub my_rights: MailboxRights,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailBodyPart {
    pub part_id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type", default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub charset: Option<String>,
    #[serde(default)]
    pub disposition: Option<String>,
    #[serde(default)]
    pub cid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailBodyValue {
    pub value: String,
    #[serde(default)]
    pub is_encoding_problem: bool,
    #[serde(default)]
    pub is_truncated: bool,
}

/// A raw RFC 5322 header, as JMAP's `headers` property returns them: every
/// header on the message, in order, undecoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Email {
    pub id: String,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub mailbox_ids: HashMap<String, bool>,
    #[serde(default)]
    pub keywords: HashMap<String, bool>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub received_at: Option<String>,
    #[serde(default)]
    pub message_id: Option<Vec<String>>,
    #[serde(default)]
    pub in_reply_to: Option<Vec<String>>,
    #[serde(default)]
    pub references: Option<Vec<String>>,
    /// RFC 5322 Sender. Distinct from `from`: set when the message was sent by
    /// someone on the author's behalf.
    #[serde(default)]
    pub sender: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub from: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub to: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub cc: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub bcc: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub reply_to: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub has_attachment: bool,
    #[serde(default)]
    pub text_body: Option<Vec<EmailBodyPart>>,
    #[serde(default)]
    pub html_body: Option<Vec<EmailBodyPart>>,
    #[serde(default)]
    pub attachments: Option<Vec<EmailBodyPart>>,
    #[serde(default)]
    pub body_values: Option<HashMap<String, EmailBodyValue>>,
    #[serde(default)]
    pub headers: Option<Vec<EmailHeader>>,
}

impl Email {
    pub fn is_unread(&self) -> bool {
        !self.keywords.contains_key("$seen")
    }

    pub fn is_flagged(&self) -> bool {
        self.keywords.contains_key("$flagged")
    }

    pub fn is_draft(&self) -> bool {
        self.keywords.contains_key("$draft")
    }

    /// Every text part joined, not just the first.
    ///
    /// A multipart message can carry several text parts; returning only the
    /// leading one silently dropped the rest.
    pub fn text_content(&self) -> Option<String> {
        self.joined_body(self.text_body.as_deref())
    }

    /// Every HTML part joined. See [`Self::text_content`].
    pub fn html_content(&self) -> Option<String> {
        self.joined_body(self.html_body.as_deref())
    }

    fn joined_body(&self, parts: Option<&[EmailBodyPart]>) -> Option<String> {
        let body_values = self.body_values.as_ref()?;
        let joined: Vec<&str> = parts?
            .iter()
            .filter_map(|p| body_values.get(p.part_id.as_ref()?))
            .map(|v| v.value.as_str())
            .collect();
        (!joined.is_empty()).then(|| joined.join("\n"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub reply_to: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub bcc: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub text_signature: Option<String>,
    #[serde(default)]
    pub html_signature: Option<String>,
    #[serde(default)]
    pub may_delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskedEmail {
    pub id: String,
    pub email: String,
    /// One of: pending, enabled, disabled, deleted
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub for_domain: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub last_message_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Output<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl<T: Serialize> Output<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            message: None,
        }
    }

    pub fn success_msg(message: impl Into<String>) -> Self {
        Self {
            success: true,
            data: None,
            error: None,
            message: Some(message.into()),
        }
    }

    pub fn error(err: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(err.into()),
            message: None,
        }
    }

    pub fn print(&self) {
        match serde_json::to_string_pretty(self) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("{{\"success\":false,\"error\":\"Serialization failed: {e}\"}}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_address_display_with_name() {
        let addr = EmailAddress {
            name: Some("John Doe".to_string()),
            email: "john@example.com".to_string(),
        };
        assert_eq!(format!("{}", addr), "John Doe <john@example.com>");
    }

    #[test]
    fn test_email_address_display_without_name() {
        let addr = EmailAddress {
            name: None,
            email: "john@example.com".to_string(),
        };
        assert_eq!(format!("{}", addr), "john@example.com");
    }

    #[test]
    fn test_email_address_display_empty_name() {
        let addr = EmailAddress {
            name: Some("".to_string()),
            email: "john@example.com".to_string(),
        };
        assert_eq!(format!("{}", addr), "john@example.com");
    }

    fn body_part(part_id: &str) -> EmailBodyPart {
        EmailBodyPart {
            part_id: Some(part_id.to_string()),
            blob_id: None,
            size: 0,
            name: None,
            content_type: Some("text/plain".to_string()),
            charset: None,
            disposition: None,
            cid: None,
        }
    }

    fn body_value(value: &str) -> EmailBodyValue {
        EmailBodyValue {
            value: value.to_string(),
            is_encoding_problem: false,
            is_truncated: false,
        }
    }

    #[test]
    fn text_content_joins_every_part() {
        // A multipart message carries several text parts; taking only the first
        // silently dropped the rest.
        let email = Email {
            id: "e1".to_string(),
            text_body: Some(vec![body_part("1"), body_part("2"), body_part("3")]),
            body_values: Some(HashMap::from([
                ("1".to_string(), body_value("first")),
                ("2".to_string(), body_value("second")),
                ("3".to_string(), body_value("third")),
            ])),
            ..Default::default()
        };
        assert_eq!(
            email.text_content().as_deref(),
            Some("first\nsecond\nthird")
        );
    }

    #[test]
    fn body_content_is_none_when_there_are_no_parts() {
        let email = Email {
            id: "e1".to_string(),
            ..Default::default()
        };
        assert_eq!(email.text_content(), None);
        assert_eq!(email.html_content(), None);
    }

    #[test]
    fn body_parts_without_a_matching_value_are_skipped() {
        let email = Email {
            id: "e1".to_string(),
            text_body: Some(vec![body_part("1"), body_part("missing")]),
            body_values: Some(HashMap::from([("1".to_string(), body_value("only"))])),
            ..Default::default()
        };
        assert_eq!(email.text_content().as_deref(), Some("only"));
    }

    #[test]
    fn test_email_is_unread() {
        let mut email = Email {
            id: "test".to_string(),
            ..Default::default()
        };
        assert!(email.is_unread());
        email.keywords.insert("$seen".to_string(), true);
        assert!(!email.is_unread());
    }

    #[test]
    fn test_email_is_flagged() {
        let mut email = Email {
            id: "test".to_string(),
            ..Default::default()
        };
        assert!(!email.is_flagged());
        email.keywords.insert("$flagged".to_string(), true);
        assert!(email.is_flagged());
    }

    #[test]
    fn test_output_success() {
        let output: Output<&str> = Output::success("test data");
        assert!(output.success);
        assert_eq!(output.data, Some("test data"));
        assert!(output.error.is_none());
    }

    #[test]
    fn test_output_error() {
        let output: Output<()> = Output::error("something broke");
        assert!(!output.success);
        assert!(output.data.is_none());
        assert_eq!(output.error, Some("something broke".to_string()));
    }

    #[test]
    fn test_session_deserialize() {
        let json = r#"{
            "capabilities": {},
            "accounts": {},
            "primaryAccounts": {"urn:ietf:params:jmap:mail": "acc1"},
            "username": "test@example.com",
            "apiUrl": "https://api.example.com/jmap",
            "downloadUrl": "https://api.example.com/download",
            "uploadUrl": "https://api.example.com/upload"
        }"#;
        let session: Session = serde_json::from_str(json).unwrap();
        assert_eq!(session.username, "test@example.com");
        assert_eq!(session.primary_account_id(), Some("acc1"));
    }

    #[test]
    fn test_mailbox_deserialize() {
        let json = r#"{
            "id": "mb1",
            "name": "Inbox",
            "role": "inbox",
            "totalEmails": 100,
            "unreadEmails": 5
        }"#;
        let mailbox: Mailbox = serde_json::from_str(json).unwrap();
        assert_eq!(mailbox.id, "mb1");
        assert_eq!(mailbox.name, "Inbox");
        assert_eq!(mailbox.role, Some("inbox".to_string()));
        assert_eq!(mailbox.total_emails, 100);
        assert_eq!(mailbox.unread_emails, 5);
    }

    #[test]
    fn test_masked_email_deserialize() {
        let json = r#"{
            "id": "me123",
            "email": "abc123@mask.fastmail.com",
            "state": "enabled",
            "forDomain": "https://example.com",
            "description": "Test site",
            "createdBy": "fastmail-cli"
        }"#;
        let masked: MaskedEmail = serde_json::from_str(json).unwrap();
        assert_eq!(masked.id, "me123");
        assert_eq!(masked.email, "abc123@mask.fastmail.com");
        assert_eq!(masked.state, Some("enabled".to_string()));
        assert_eq!(masked.for_domain, Some("https://example.com".to_string()));
        assert_eq!(masked.description, Some("Test site".to_string()));
        assert_eq!(masked.created_by, Some("fastmail-cli".to_string()));
    }
}

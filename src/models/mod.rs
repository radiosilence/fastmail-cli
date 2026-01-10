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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[allow(dead_code)]
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

    pub fn sender_display(&self) -> String {
        self.from
            .as_ref()
            .and_then(|addrs| addrs.first())
            .map(|a| a.to_string())
            .unwrap_or_else(|| "(unknown)".into())
    }

    pub fn text_content(&self) -> Option<&str> {
        let body_values = self.body_values.as_ref()?;
        let text_body = self.text_body.as_ref()?;
        let part = text_body.first()?;
        let part_id = part.part_id.as_ref()?;
        body_values.get(part_id).map(|v| v.value.as_str())
    }

    pub fn html_content(&self) -> Option<&str> {
        let body_values = self.body_values.as_ref()?;
        let html_body = self.html_body.as_ref()?;
        let part = html_body.first()?;
        let part_id = part.part_id.as_ref()?;
        body_values.get(part_id).map(|v| v.value.as_str())
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
        println!("{}", serde_json::to_string_pretty(self).unwrap());
    }
}

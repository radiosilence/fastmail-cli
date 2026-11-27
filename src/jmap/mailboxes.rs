//! Mailbox management: `Mailbox/set` create, update and destroy (RFC 8621 §2.5).

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracing::instrument;

use super::JmapClient;
use crate::error::{Error, Result};

/// What a mailbox update changes. `None` leaves a property alone.
#[derive(Debug, Default, Clone)]
pub struct MailboxPatch {
    pub name: Option<String>,
    /// `Some(None)` moves the mailbox to the top level.
    pub parent_id: Option<Option<String>>,
    pub sort_order: Option<u32>,
    pub is_subscribed: Option<bool>,
}

impl MailboxPatch {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.parent_id.is_none()
            && self.sort_order.is_none()
            && self.is_subscribed.is_none()
    }

    fn to_json(&self) -> Value {
        let mut patch = Map::new();
        if let Some(name) = &self.name {
            patch.insert("name".into(), json!(name));
        }
        if let Some(parent) = &self.parent_id {
            patch.insert("parentId".into(), json!(parent));
        }
        if let Some(order) = self.sort_order {
            patch.insert("sortOrder".into(), json!(order));
        }
        if let Some(subscribed) = self.is_subscribed {
            patch.insert("isSubscribed".into(), json!(subscribed));
        }
        Value::Object(patch)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MailboxSetResponse {
    #[serde(default)]
    created: Option<HashMap<String, Value>>,
    #[serde(default)]
    not_created: Option<HashMap<String, Value>>,
    #[serde(default)]
    not_updated: Option<HashMap<String, Value>>,
    #[serde(default)]
    not_destroyed: Option<HashMap<String, Value>>,
}

/// A `SetError` as an [`Error::Jmap`], keeping the server's type, which is
/// what tells a caller why (`mailboxHasChild`, `mailboxHasEmail`,
/// `forbidden`, `invalidProperties`…).
fn set_error(err: &Value, fallback: &str) -> Error {
    let error_type = err.get("type").and_then(Value::as_str).unwrap_or("unknown");
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
        method: "Mailbox/set".into(),
        error_type: error_type.into(),
        description,
    }
}

impl JmapClient {
    async fn mailbox_set(&self, args: Map<String, Value>) -> Result<MailboxSetResponse> {
        let mut args = args;
        args.insert("accountId".into(), json!(self.account_id()?));
        let responses = self
            .request(vec![json!(["Mailbox/set", Value::Object(args), "mb0"])])
            .await?;
        Self::parse_response(responses.first().unwrap_or(&Value::Null), "Mailbox/set")
    }

    /// Create a mailbox, returning its ID.
    #[instrument(skip(self))]
    pub async fn create_mailbox(
        &self,
        name: &str,
        parent_id: Option<&str>,
        sort_order: Option<u32>,
        is_subscribed: Option<bool>,
    ) -> Result<String> {
        let mut create = Map::new();
        create.insert("name".into(), json!(name));
        create.insert("parentId".into(), json!(parent_id));
        if let Some(order) = sort_order {
            create.insert("sortOrder".into(), json!(order));
        }
        // Subscribed by default, as a folder made in Fastmail's own UI is, so
        // it shows up in IMAP clients too.
        create.insert("isSubscribed".into(), json!(is_subscribed.unwrap_or(true)));

        let mut args = Map::new();
        args.insert("create".into(), json!({ "new": create }));
        let resp = self.mailbox_set(args).await?;

        if let Some(err) = resp.not_created.as_ref().and_then(|m| m.get("new")) {
            return Err(set_error(err, "Failed to create mailbox"));
        }
        resp.created
            .as_ref()
            .and_then(|c| c.get("new"))
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Jmap {
                method: "Mailbox/set".into(),
                error_type: "unknown".into(),
                description: "The server created no mailbox".into(),
            })
    }

    /// Rename, move, reorder or (un)subscribe a mailbox.
    #[instrument(skip(self))]
    pub async fn update_mailbox(&self, id: &str, patch: &MailboxPatch) -> Result<()> {
        let mut args = Map::new();
        args.insert("update".into(), json!({ (id): patch.to_json() }));
        let resp = self.mailbox_set(args).await?;
        match resp.not_updated.as_ref().and_then(|m| m.get(id)) {
            Some(err) => Err(set_error(err, "Failed to update mailbox")),
            None => Ok(()),
        }
    }

    /// Delete a mailbox. Without `remove_emails` the server refuses one that
    /// still holds mail (`mailboxHasEmail`); with it, mail that is in no other
    /// mailbox is destroyed for good, not moved to Trash. A mailbox with
    /// children is always refused (`mailboxHasChild`).
    #[instrument(skip(self))]
    pub async fn destroy_mailbox(&self, id: &str, remove_emails: bool) -> Result<()> {
        let mut args = Map::new();
        args.insert("destroy".into(), json!([id]));
        args.insert("onDestroyRemoveEmails".into(), json!(remove_emails));
        let resp = self.mailbox_set(args).await?;
        match resp.not_destroyed.as_ref().and_then(|m| m.get(id)) {
            Some(err) => Err(set_error(err, "Failed to delete mailbox")),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_patch_changes_nothing() {
        assert!(MailboxPatch::default().is_empty());
        assert_eq!(MailboxPatch::default().to_json(), json!({}));
    }

    #[test]
    fn moving_to_the_top_level_sends_a_null_parent() {
        let patch = MailboxPatch {
            parent_id: Some(None),
            ..Default::default()
        };
        assert_eq!(patch.to_json(), json!({ "parentId": null }));
    }

    #[test]
    fn a_set_error_keeps_its_type_and_properties() {
        let err = set_error(
            &json!({ "type": "invalidProperties", "properties": ["name"] }),
            "nope",
        );
        let text = err.to_string();
        assert!(text.contains("invalidProperties"), "{text}");
        assert!(text.contains("properties: name"), "{text}");
    }
}

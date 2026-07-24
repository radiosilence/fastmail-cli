//! Composable email filters and sorting.
//!
//! JMAP's `Email/query` filter is natively a tree: a `FilterCondition` (a flat
//! bag of properties, implicitly AND-ed) or a `FilterOperator` (`AND`/`OR`/`NOT`
//! over nested filters) — RFC 8620 §5.5. [`EmailFilter`] mirrors that shape
//! rather than flattening it to a bag of scalar arguments, so callers can
//! express things like "from A or from B, but not in Archive".

use std::collections::{HashMap, HashSet};

use async_graphql::{Enum, InputObject};
use serde_json::{Value, json};

use crate::jmap::normalize_date;

/// A filter over emails.
///
/// Every scalar field set on one `EmailFilter` is combined with **AND**. Use
/// the `and` / `or` / `not` fields to build anything more interesting; they
/// nest arbitrarily.
///
/// ```graphql
/// # unread, from either address, but not in Archive
/// filter: {
///   unread: true
///   or:  [{ from: "alice@example.com" }, { from: "bob@example.com" }]
///   not: [{ inMailbox: "Archive" }]
/// }
/// ```
#[derive(InputObject, Default, Clone)]
#[graphql(name = "EmailFilter")]
pub struct EmailFilter {
    /// Full-text search across subject, body, and participants.
    pub text: Option<String>,
    /// Match the sender address or name.
    pub from: Option<String>,
    /// Match a recipient address or name.
    pub to: Option<String>,
    /// Match a CC recipient.
    pub cc: Option<String>,
    /// Match a BCC recipient.
    pub bcc: Option<String>,
    /// Match the subject line only.
    pub subject: Option<String>,
    /// Match the body only.
    pub body: Option<String>,
    /// Restrict to a mailbox, by name (`"Archive"`) or role (`"inbox"`).
    pub in_mailbox: Option<String>,
    /// Restrict to a mailbox by its ID, skipping name resolution.
    pub in_mailbox_id: Option<String>,
    /// Only emails that do (or don't) carry an attachment.
    pub has_attachment: Option<bool>,
    /// Minimum size in bytes.
    pub min_size: Option<u64>,
    /// Maximum size in bytes.
    pub max_size: Option<u64>,
    /// Received strictly before this date (`YYYY-MM-DD` or ISO 8601).
    pub before: Option<String>,
    /// Received at or after this date (`YYYY-MM-DD` or ISO 8601).
    pub after: Option<String>,
    /// Only unread emails when true; only read emails when false.
    pub unread: Option<bool>,
    /// Only flagged/starred emails when true; only unflagged when false.
    pub flagged: Option<bool>,
    /// Emails carrying this keyword, e.g. `$draft`, `$answered`.
    pub has_keyword: Option<String>,
    /// Emails not carrying this keyword.
    pub not_keyword: Option<String>,
    /// All of these must match, in addition to the fields above.
    pub and: Option<Vec<EmailFilter>>,
    /// At least one of these must match.
    pub or: Option<Vec<EmailFilter>>,
    /// None of these may match.
    pub not: Option<Vec<EmailFilter>>,
}

/// Properties an email query can be sorted by.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "EmailSortProperty")]
pub enum EmailSortProperty {
    ReceivedAt,
    SentAt,
    Size,
    Subject,
    From,
    To,
}

impl EmailSortProperty {
    fn as_jmap(self) -> &'static str {
        match self {
            Self::ReceivedAt => "receivedAt",
            Self::SentAt => "sentAt",
            Self::Size => "size",
            Self::Subject => "subject",
            Self::From => "from",
            Self::To => "to",
        }
    }
}

/// One sort comparator. Pass several to break ties, most significant first.
#[derive(InputObject, Copy, Clone)]
#[graphql(name = "EmailSort")]
pub struct EmailSort {
    pub property: EmailSortProperty,
    /// Ascending order. Defaults to false, i.e. newest/largest first.
    pub ascending: Option<bool>,
}

/// The default sort: newest first.
pub fn default_sort() -> Vec<Value> {
    vec![json!({ "property": "receivedAt", "isAscending": false })]
}

pub fn sort_to_jmap(sort: Option<&[EmailSort]>) -> Vec<Value> {
    match sort {
        Some(s) if !s.is_empty() => s
            .iter()
            .map(|s| {
                json!({
                    "property": s.property.as_jmap(),
                    "isAscending": s.ascending.unwrap_or(false)
                })
            })
            .collect(),
        _ => default_sort(),
    }
}

impl EmailFilter {
    /// Collect every mailbox name referenced anywhere in the tree, so they can
    /// all be resolved to IDs in one pass before conversion.
    pub fn mailbox_names(&self, out: &mut HashSet<String>) {
        if let Some(ref name) = self.in_mailbox {
            out.insert(name.clone());
        }
        for group in [&self.and, &self.or, &self.not].into_iter().flatten() {
            for child in group {
                child.mailbox_names(out);
            }
        }
    }

    /// Convert to a JMAP filter, using `mailboxes` to map names to IDs.
    ///
    /// Returns `None` for a filter that constrains nothing, so callers can omit
    /// the key entirely rather than sending an empty object.
    pub fn to_jmap(&self, mailboxes: &HashMap<String, String>) -> Option<Value> {
        let mut cond = serde_json::Map::new();

        let mut set = |key: &str, value: Value| {
            cond.insert(key.to_string(), value);
        };

        if let Some(ref v) = self.text {
            set("text", json!(v));
        }
        if let Some(ref v) = self.from {
            set("from", json!(v));
        }
        if let Some(ref v) = self.to {
            set("to", json!(v));
        }
        if let Some(ref v) = self.cc {
            set("cc", json!(v));
        }
        if let Some(ref v) = self.bcc {
            set("bcc", json!(v));
        }
        if let Some(ref v) = self.subject {
            set("subject", json!(v));
        }
        if let Some(ref v) = self.body {
            set("body", json!(v));
        }
        if let Some(ref v) = self.in_mailbox_id {
            set("inMailbox", json!(v));
        }
        if let Some(ref name) = self.in_mailbox
            && let Some(id) = mailboxes.get(name)
        {
            set("inMailbox", json!(id));
        }
        if let Some(v) = self.has_attachment {
            set("hasAttachment", json!(v));
        }
        if let Some(v) = self.min_size {
            set("minSize", json!(v));
        }
        if let Some(v) = self.max_size {
            set("maxSize", json!(v));
        }
        if let Some(ref v) = self.before {
            set("before", json!(normalize_date(v)));
        }
        if let Some(ref v) = self.after {
            set("after", json!(normalize_date(v)));
        }
        if let Some(ref v) = self.has_keyword {
            set("hasKeyword", json!(v));
        }
        if let Some(ref v) = self.not_keyword {
            set("notKeyword", json!(v));
        }

        // `unread`/`flagged` are keyword conditions. The negative cases need a
        // NOT wrapper, since one FilterCondition can't hold both a hasKeyword
        // and a notKeyword for different keywords.
        let mut extra: Vec<Value> = Vec::new();
        for (want, keyword) in [(self.unread, "$seen"), (self.flagged, "$flagged")] {
            let Some(want) = want else { continue };
            // unread == true  → NOT $seen;    unread == false → $seen
            // flagged == true → $flagged;     flagged == false → NOT $flagged
            let positive = if keyword == "$seen" { !want } else { want };
            let key = if positive { "hasKeyword" } else { "notKeyword" };
            if cond.contains_key(key) {
                extra.push(json!({ key: keyword }));
            } else {
                cond.insert(key.to_string(), json!(keyword));
            }
        }

        let mut conditions: Vec<Value> = Vec::new();
        if !cond.is_empty() {
            conditions.push(Value::Object(cond));
        }
        conditions.extend(extra);

        for child in self.and.iter().flatten() {
            conditions.extend(child.to_jmap(mailboxes));
        }

        for (key, group) in [("OR", &self.or), ("NOT", &self.not)] {
            let Some(group) = group else { continue };
            let nested: Vec<Value> = group.iter().filter_map(|c| c.to_jmap(mailboxes)).collect();
            if !nested.is_empty() {
                conditions.push(json!({ "operator": key, "conditions": nested }));
            }
        }

        match conditions.len() {
            0 => None,
            1 => conditions.pop(),
            _ => Some(json!({ "operator": "AND", "conditions": conditions })),
        }
    }
}

/// Combine an optional user filter with a constraint the resolver imposes
/// (for instance, "in this mailbox" on `Mailbox.emails`).
pub fn and_also(base: Option<Value>, extra: Value) -> Value {
    match base {
        Some(base) => json!({ "operator": "AND", "conditions": [base, extra] }),
        None => extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_mailboxes() -> HashMap<String, String> {
        HashMap::new()
    }

    fn filter(json: Value) -> EmailFilter {
        // Build via the same path GraphQL would, so the test exercises the real
        // field names rather than Rust struct literals.
        serde_json::from_value::<Raw>(json).unwrap().into()
    }

    /// Mirror of EmailFilter for terse test construction.
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct Raw {
        text: Option<String>,
        from: Option<String>,
        subject: Option<String>,
        in_mailbox: Option<String>,
        in_mailbox_id: Option<String>,
        unread: Option<bool>,
        flagged: Option<bool>,
        has_keyword: Option<String>,
        and: Option<Vec<Raw>>,
        or: Option<Vec<Raw>>,
        not: Option<Vec<Raw>>,
    }

    impl From<Raw> for EmailFilter {
        fn from(r: Raw) -> Self {
            EmailFilter {
                text: r.text,
                from: r.from,
                subject: r.subject,
                in_mailbox: r.in_mailbox,
                in_mailbox_id: r.in_mailbox_id,
                unread: r.unread,
                flagged: r.flagged,
                has_keyword: r.has_keyword,
                and: r.and.map(|v| v.into_iter().map(Into::into).collect()),
                or: r.or.map(|v| v.into_iter().map(Into::into).collect()),
                not: r.not.map(|v| v.into_iter().map(Into::into).collect()),
                ..Default::default()
            }
        }
    }

    #[test]
    fn empty_filter_constrains_nothing() {
        assert_eq!(EmailFilter::default().to_jmap(&no_mailboxes()), None);
    }

    #[test]
    fn scalars_collapse_into_one_condition() {
        let f = filter(json!({ "from": "a@b.com", "subject": "invoice" }));
        assert_eq!(
            f.to_jmap(&no_mailboxes()).unwrap(),
            json!({ "from": "a@b.com", "subject": "invoice" })
        );
    }

    #[test]
    fn or_becomes_a_filter_operator() {
        let f = filter(json!({ "or": [{ "from": "a@b.com" }, { "from": "c@d.com" }] }));
        assert_eq!(
            f.to_jmap(&no_mailboxes()).unwrap(),
            json!({
                "operator": "OR",
                "conditions": [{ "from": "a@b.com" }, { "from": "c@d.com" }]
            })
        );
    }

    #[test]
    fn scalars_and_operators_are_anded_together() {
        let f = filter(json!({
            "unread": true,
            "or": [{ "from": "a@b.com" }, { "from": "c@d.com" }],
            "not": [{ "subject": "spam" }]
        }));
        assert_eq!(
            f.to_jmap(&no_mailboxes()).unwrap(),
            json!({
                "operator": "AND",
                "conditions": [
                    { "notKeyword": "$seen" },
                    { "operator": "OR", "conditions": [
                        { "from": "a@b.com" }, { "from": "c@d.com" }] },
                    { "operator": "NOT", "conditions": [{ "subject": "spam" }] }
                ]
            })
        );
    }

    #[test]
    fn nesting_is_arbitrarily_deep() {
        let f = filter(json!({
            "or": [
                { "from": "a@b.com" },
                { "and": [{ "subject": "x" }, { "or": [{ "text": "p" }, { "text": "q" }] }] }
            ]
        }));
        let got = f.to_jmap(&no_mailboxes()).unwrap();
        assert_eq!(got["operator"], "OR");
        let inner = &got["conditions"][1];
        assert_eq!(inner["operator"], "AND");
        assert_eq!(inner["conditions"][1]["operator"], "OR");
    }

    #[test]
    fn unread_false_means_seen() {
        let f = filter(json!({ "unread": false }));
        assert_eq!(
            f.to_jmap(&no_mailboxes()).unwrap(),
            json!({ "hasKeyword": "$seen" })
        );
    }

    #[test]
    fn flagged_false_negates() {
        let f = filter(json!({ "flagged": false }));
        assert_eq!(
            f.to_jmap(&no_mailboxes()).unwrap(),
            json!({ "notKeyword": "$flagged" })
        );
    }

    #[test]
    fn keyword_collision_splits_into_a_separate_condition() {
        // hasKeyword is already taken by $draft, so flagged must not clobber it.
        let f = filter(json!({ "hasKeyword": "$draft", "flagged": true }));
        assert_eq!(
            f.to_jmap(&no_mailboxes()).unwrap(),
            json!({
                "operator": "AND",
                "conditions": [{ "hasKeyword": "$draft" }, { "hasKeyword": "$flagged" }]
            })
        );
    }

    #[test]
    fn mailbox_names_resolve_to_ids_at_every_depth() {
        let f = filter(json!({
            "inMailbox": "Inbox",
            "or": [{ "inMailbox": "Archive" }, { "not": [{ "inMailbox": "Trash" }] }]
        }));

        let mut names = HashSet::new();
        f.mailbox_names(&mut names);
        assert_eq!(
            names,
            ["Inbox", "Archive", "Trash"]
                .map(str::to_owned)
                .into_iter()
                .collect()
        );

        let resolved: HashMap<String, String> =
            [("Inbox", "mb1"), ("Archive", "mb2"), ("Trash", "mb3")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into_iter()
                .collect();
        let got = f.to_jmap(&resolved).unwrap();
        assert_eq!(got["conditions"][0]["inMailbox"], "mb1");
        assert_eq!(got["conditions"][1]["conditions"][0]["inMailbox"], "mb2");
    }

    #[test]
    fn empty_nested_groups_are_dropped() {
        let f = filter(json!({ "from": "a@b.com", "or": [{}, {}] }));
        assert_eq!(
            f.to_jmap(&no_mailboxes()).unwrap(),
            json!({ "from": "a@b.com" }),
            "an OR of empty filters constrains nothing and must not be emitted"
        );
    }

    #[test]
    fn sort_defaults_to_newest_first() {
        assert_eq!(sort_to_jmap(None), default_sort());
        assert_eq!(sort_to_jmap(Some(&[])), default_sort());
    }

    #[test]
    fn sort_maps_to_jmap_comparators() {
        let sort = [
            EmailSort {
                property: EmailSortProperty::From,
                ascending: Some(true),
            },
            EmailSort {
                property: EmailSortProperty::ReceivedAt,
                ascending: None,
            },
        ];
        assert_eq!(
            sort_to_jmap(Some(&sort)),
            vec![
                json!({ "property": "from", "isAscending": true }),
                json!({ "property": "receivedAt", "isAscending": false }),
            ]
        );
    }
}

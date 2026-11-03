//! Mailing lists and bulk changes: the account-wide scans behind
//! `mailingLists`, `sweepEmails` and the spam keywords.
//!
//! Fastmail does not expose its filter rules over JMAP (no
//! `urn:ietf:params:jmap:sieve` for API tokens), so nothing here touches rules.
//! These work on the mail that is already there.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::instrument;

use super::JmapClient;
use crate::error::{Error, Result};
use crate::models::EmailAddress;

/// Emails fetched per `Email/query` + `Email/get` round trip while scanning.
/// Only headers come back, so a page is small.
const SCAN_PAGE: usize = 500;

/// IDs per `Email/set` in a bulk update.
const UPDATE_CHUNK: usize = 500;

/// The properties a scan needs: who sent it and how to leave the list, no
/// bodies.
const SCAN_PROPERTIES: &[&str] = &[
    "id",
    "from",
    "to",
    "subject",
    "receivedAt",
    "keywords",
    "header:List-Id:asText",
    "header:List-Unsubscribe:asURLs",
    "header:List-Unsubscribe-Post:asText",
];

/// One email as a scan sees it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEmail {
    pub id: String,
    #[serde(default)]
    pub from: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub to: Option<Vec<EmailAddress>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub received_at: Option<String>,
    #[serde(default)]
    pub keywords: HashMap<String, bool>,
    #[serde(rename = "header:List-Id:asText", default)]
    pub list_id: Option<String>,
    #[serde(rename = "header:List-Unsubscribe:asURLs", default)]
    pub unsubscribe_urls: Option<Vec<String>>,
    #[serde(rename = "header:List-Unsubscribe-Post:asText", default)]
    pub unsubscribe_post: Option<String>,
}

/// How a message says it can be left, from its `List-Unsubscribe` and
/// `List-Unsubscribe-Post` headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeOptions {
    /// The email these came from.
    pub email_id: String,
    /// RFC 8058: the sender accepts a bare POST to the HTTPS link, no page to
    /// click through.
    pub one_click: bool,
    pub http: Vec<String>,
    pub mailto: Vec<String>,
}

impl UnsubscribeOptions {
    pub fn from_headers(email_id: &str, urls: &[String], post: Option<&str>) -> Self {
        let mut http = Vec::new();
        let mut mailto = Vec::new();
        for url in urls {
            let url = url.trim();
            let lower = url.to_ascii_lowercase();
            if lower.starts_with("https://") || lower.starts_with("http://") {
                http.push(url.to_string());
            } else if lower.starts_with("mailto:") {
                mailto.push(url.to_string());
            }
        }
        // RFC 8058 §3.1: the header must be exactly this key/value, and the
        // URI it applies to must be HTTPS.
        let one_click = post
            .is_some_and(|p| p.trim().eq_ignore_ascii_case("List-Unsubscribe=One-Click"))
            && http
                .iter()
                .any(|u| u.to_ascii_lowercase().starts_with("https://"));
        Self {
            email_id: email_id.to_string(),
            one_click,
            http,
            mailto,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.http.is_empty() && self.mailto.is_empty()
    }

    /// The HTTPS link RFC 8058 says to POST to.
    pub fn one_click_url(&self) -> Option<&str> {
        self.one_click
            .then(|| {
                self.http
                    .iter()
                    .find(|u| u.to_ascii_lowercase().starts_with("https://"))
            })
            .flatten()
            .map(String::as_str)
    }
}

impl ListEmail {
    pub fn unsubscribe(&self) -> UnsubscribeOptions {
        UnsubscribeOptions::from_headers(
            &self.id,
            self.unsubscribe_urls.as_deref().unwrap_or_default(),
            self.unsubscribe_post.as_deref(),
        )
    }
}

/// The URLs in a raw `List-Unsubscribe` value: `<mailto:a@b>, <https://…>`.
/// Folding whitespace inside the brackets is dropped.
pub fn parse_list_unsubscribe(raw: &str) -> Vec<String> {
    raw.split('<')
        .skip(1)
        .filter_map(|part| part.split_once('>').map(|(url, _)| url))
        .map(|url| url.split_whitespace().collect::<String>())
        .filter(|url| !url.is_empty())
        .collect()
}

/// Everything one sender has sent that carries an unsubscribe header.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailingList {
    /// Lower-cased sender address: what the mail is grouped by.
    pub sender_email: String,
    pub sender_name: Option<String>,
    pub count: u32,
    pub unread_count: u32,
    /// Distinct `List-Id` values seen, most recent first, at most five.
    pub list_ids: Vec<String>,
    pub latest_email_id: String,
    pub latest_subject: Option<String>,
    pub latest_received_at: Option<String>,
    pub first_received_at: Option<String>,
    /// From the newest email that offers a way out: older links are the
    /// likeliest to have expired.
    pub unsubscribe: Option<UnsubscribeOptions>,
}

/// The result of a scan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailingListReport {
    /// Emails read.
    pub scanned: u32,
    /// Emails matching, which is more than `scanned` when the limit cut it off.
    pub total: u32,
    pub lists: Vec<MailingList>,
}

/// Group scanned emails by sender. Expects newest first, as the scan sorts
/// them, so the first email seen for a sender is its latest.
pub fn group_by_sender(emails: impl IntoIterator<Item = ListEmail>) -> Vec<MailingList> {
    let mut order: Vec<String> = Vec::new();
    let mut lists: HashMap<String, MailingList> = HashMap::new();
    for email in emails {
        let sender = email.from.as_ref().and_then(|f| f.first());
        let key = sender
            .map(|a| a.email.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "(no sender)".to_string());
        let unread = !email.keywords.get("$seen").copied().unwrap_or(false);
        let list_id = email
            .list_id
            .as_deref()
            .map(normalise_list_id)
            .filter(|l| !l.is_empty());
        let options = email.unsubscribe();

        let entry = lists.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            MailingList {
                sender_email: key,
                sender_name: sender
                    .and_then(|a| a.name.clone())
                    .filter(|n| !n.is_empty()),
                count: 0,
                unread_count: 0,
                list_ids: Vec::new(),
                latest_email_id: email.id.clone(),
                latest_subject: email.subject.clone(),
                latest_received_at: email.received_at.clone(),
                first_received_at: None,
                unsubscribe: None,
            }
        });
        entry.count += 1;
        entry.unread_count += u32::from(unread);
        entry.first_received_at = email.received_at.clone();
        if let Some(id) = list_id
            && entry.list_ids.len() < 5
            && !entry.list_ids.contains(&id)
        {
            entry.list_ids.push(id);
        }
        if entry.unsubscribe.is_none() && !options.is_empty() {
            entry.unsubscribe = Some(options);
        }
    }
    let mut out: Vec<MailingList> = order.into_iter().filter_map(|k| lists.remove(&k)).collect();
    // Stable, so equal counts keep newest-first.
    out.sort_by_key(|l| std::cmp::Reverse(l.count));
    out
}

/// `"Weekly Deals <deals.example.com>"` → `deals.example.com`.
fn normalise_list_id(raw: &str) -> String {
    let raw = raw.trim();
    match (raw.rfind('<'), raw.rfind('>')) {
        (Some(start), Some(end)) if start < end => raw[start + 1..end].trim().to_lowercase(),
        _ => raw.to_lowercase(),
    }
}

/// What a bulk update did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcome {
    pub updated: u32,
    /// `(email id, reason)` for each one the server refused.
    pub failed: Vec<(String, String)>,
}

impl JmapClient {
    /// Every email matching `filter` (newest first, at most `limit`), with
    /// only the headers a mailing-list report needs, and the full match
    /// count.
    #[instrument(skip(self, filter))]
    pub async fn scan_list_emails(
        &self,
        filter: Value,
        limit: usize,
    ) -> Result<(Vec<ListEmail>, u32)> {
        let account_id = self.account_id()?;
        let mut out = Vec::new();
        let mut total = 0u32;
        while out.len() < limit {
            let page = SCAN_PAGE.min(limit - out.len());
            let mut responses = self
                .request(vec![
                    json!([
                        "Email/query",
                        {
                            "accountId": account_id,
                            "filter": filter,
                            "sort": [{ "property": "receivedAt", "isAscending": false }],
                            "position": out.len(),
                            "limit": page,
                            "calculateTotal": true
                        },
                        "q0"
                    ]),
                    json!([
                        "Email/get",
                        {
                            "accountId": account_id,
                            "#ids": { "resultOf": "q0", "name": "Email/query", "path": "/ids" },
                            "properties": SCAN_PROPERTIES
                        },
                        "g0"
                    ]),
                ])
                .await?;
            let query: super::QueryResponse =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/query")?;
            total = query.total.unwrap_or(0) as u32;
            let got: super::GetResponse<ListEmail> = Self::parse_response_owned(
                responses.get_mut(1).map(Value::take).unwrap_or_default(),
                "Email/get",
            )?;
            let mut by_id: HashMap<String, ListEmail> =
                got.list.into_iter().map(|e| (e.id.clone(), e)).collect();
            let fetched = query.ids.len();
            out.extend(query.ids.iter().filter_map(|id| by_id.remove(id)));
            if fetched < page {
                break;
            }
        }
        Ok((out, total))
    }

    /// The IDs of every email matching `filter`, newest first, at most
    /// `limit`, and the full match count.
    #[instrument(skip(self, filter))]
    pub async fn query_email_ids(&self, filter: Value, limit: usize) -> Result<(Vec<String>, u32)> {
        let account_id = self.account_id()?;
        let mut ids: Vec<String> = Vec::new();
        let mut total = 0u32;
        while ids.len() < limit {
            let page = 1000.min(limit - ids.len());
            let responses = self
                .request(vec![json!([
                    "Email/query",
                    {
                        "accountId": account_id,
                        "filter": filter,
                        "sort": [{ "property": "receivedAt", "isAscending": false }],
                        "position": ids.len(),
                        "limit": page,
                        "calculateTotal": true
                    },
                    "q0"
                ])])
                .await?;
            let query: super::QueryResponse =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/query")?;
            total = query.total.unwrap_or(0) as u32;
            let fetched = query.ids.len();
            ids.extend(query.ids);
            if fetched < page {
                break;
            }
        }
        Ok((ids, total))
    }

    /// The unsubscribe headers of one email.
    #[instrument(skip(self))]
    pub async fn get_list_email(&self, email_id: &str) -> Result<ListEmail> {
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "Email/get",
                { "accountId": account_id, "ids": [email_id], "properties": SCAN_PROPERTIES },
                "g0"
            ])])
            .await?;
        let got: super::GetResponse<ListEmail> = Self::parse_response_owned(
            responses.into_iter().next().unwrap_or_default(),
            "Email/get",
        )?;
        got.list
            .into_iter()
            .next()
            .ok_or_else(|| Error::EmailNotFound(email_id.to_string()))
    }

    /// Apply one JMAP patch (`{"keywords/$seen": true, …}`) to many emails,
    /// in chunks. Refusals are collected rather than failing the batch.
    #[instrument(skip(self, ids, patch))]
    pub async fn update_emails(&self, ids: &[String], patch: &Value) -> Result<UpdateOutcome> {
        let account_id = self.account_id()?;
        let mut outcome = UpdateOutcome::default();
        for chunk in ids.chunks(UPDATE_CHUNK) {
            let update: serde_json::Map<String, Value> =
                chunk.iter().map(|id| (id.clone(), patch.clone())).collect();
            let responses = self
                .request(vec![json!([
                    "Email/set",
                    { "accountId": account_id, "update": update },
                    "u0"
                ])])
                .await?;
            let resp: super::SetResponse =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;
            let refused = resp.not_updated.unwrap_or_default();
            for id in chunk {
                match refused.get(id) {
                    Some(err) => outcome.failed.push((
                        id.clone(),
                        err.get("description")
                            .or_else(|| err.get("type"))
                            .and_then(Value::as_str)
                            .unwrap_or("refused")
                            .to_string(),
                    )),
                    None => outcome.updated += 1,
                }
            }
        }
        Ok(outcome)
    }

    /// Move to Junk and mark `$junk`, which is what trains the filter.
    pub async fn mark_spam_many(&self, ids: &[String]) -> Result<UpdateOutcome> {
        let junk = self.find_mailbox("junk").await?;
        self.update_emails(ids, &spam_patch(&junk.id)).await
    }

    /// Move back to `mailbox_id` and mark `$notjunk`, so the filter learns
    /// it was wrong.
    pub async fn mark_not_spam_many(
        &self,
        ids: &[String],
        mailbox_id: &str,
    ) -> Result<UpdateOutcome> {
        self.update_emails(ids, &not_spam_patch(mailbox_id)).await
    }
}

/// Junk, with the keywords mail clients use to tell the filter.
pub fn spam_patch(junk_id: &str) -> Value {
    json!({
        "mailboxIds": { (junk_id): true },
        "keywords/$junk": true,
        "keywords/$notjunk": null,
    })
}

pub fn not_spam_patch(mailbox_id: &str) -> Value {
    json!({
        "mailboxIds": { (mailbox_id): true },
        "keywords/$junk": null,
        "keywords/$notjunk": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(id: &str, from: &str, at: &str, urls: &[&str], post: Option<&str>) -> ListEmail {
        ListEmail {
            id: id.into(),
            from: Some(vec![EmailAddress {
                name: Some("Shop".into()),
                email: from.into(),
            }]),
            to: None,
            subject: Some(format!("subject {id}")),
            received_at: Some(at.into()),
            keywords: HashMap::new(),
            list_id: Some("Deals <Deals.Example.com>".into()),
            unsubscribe_urls: Some(urls.iter().map(|s| s.to_string()).collect()),
            unsubscribe_post: post.map(str::to_string),
        }
    }

    #[test]
    fn one_click_needs_the_exact_post_header_and_an_https_link() {
        let urls = vec![
            "https://x.test/u".to_string(),
            "mailto:u@x.test".to_string(),
        ];
        let o = UnsubscribeOptions::from_headers("e", &urls, Some("List-Unsubscribe=One-Click"));
        assert!(o.one_click);
        assert_eq!(o.one_click_url(), Some("https://x.test/u"));
        assert_eq!(o.mailto, vec!["mailto:u@x.test"]);

        assert!(!UnsubscribeOptions::from_headers("e", &urls, None).one_click);
        assert!(!UnsubscribeOptions::from_headers("e", &urls, Some("yes")).one_click);
        let http_only = vec!["http://x.test/u".to_string()];
        let o =
            UnsubscribeOptions::from_headers("e", &http_only, Some("List-Unsubscribe=One-Click"));
        assert!(!o.one_click, "RFC 8058 one-click is HTTPS only");
        assert_eq!(o.one_click_url(), None);
    }

    #[test]
    fn groups_by_sender_keeping_the_newest_links() {
        let lists = group_by_sender(vec![
            email(
                "e3",
                "News@Shop.test",
                "2026-03",
                &["https://new.test"],
                None,
            ),
            email("e2", "other@x.test", "2026-02", &["mailto:a@x.test"], None),
            email(
                "e1",
                "news@shop.test",
                "2026-01",
                &["https://old.test"],
                None,
            ),
        ]);
        assert_eq!(lists.len(), 2);
        let shop = &lists[0];
        assert_eq!(shop.sender_email, "news@shop.test");
        assert_eq!(shop.count, 2);
        assert_eq!(shop.unread_count, 2);
        assert_eq!(shop.latest_email_id, "e3");
        assert_eq!(shop.first_received_at.as_deref(), Some("2026-01"));
        assert_eq!(shop.list_ids, vec!["deals.example.com"]);
        let unsub = shop.unsubscribe.as_ref().unwrap();
        assert_eq!(unsub.email_id, "e3");
        assert_eq!(unsub.http, vec!["https://new.test"]);
    }

    #[test]
    fn a_newest_email_without_links_falls_back_to_an_older_one() {
        let lists = group_by_sender(vec![
            email("e2", "a@x.test", "2026-02", &[], None),
            email("e1", "a@x.test", "2026-01", &["https://old.test"], None),
        ]);
        assert_eq!(lists[0].unsubscribe.as_ref().unwrap().email_id, "e1");
    }

    #[test]
    fn raw_list_unsubscribe_values_come_apart() {
        assert_eq!(
            parse_list_unsubscribe(
                " <mailto:u@x.test?subject=bye>,\r\n <https://x.test/\r\n u?id=1>"
            ),
            vec!["mailto:u@x.test?subject=bye", "https://x.test/u?id=1"]
        );
        assert!(parse_list_unsubscribe("garbage").is_empty());
    }

    #[test]
    fn spam_patches_set_the_training_keywords() {
        let p = spam_patch("J");
        assert_eq!(p["mailboxIds"], json!({ "J": true }));
        assert_eq!(p["keywords/$junk"], json!(true));
        assert!(p["keywords/$notjunk"].is_null());
        let p = not_spam_patch("I");
        assert_eq!(p["keywords/$notjunk"], json!(true));
        assert!(p.get("keywords/$junk").is_some_and(Value::is_null));
    }
}

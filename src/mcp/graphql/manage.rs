//! Mutations managing mail rather than composing it: keywords and folder
//! membership, destroying, scheduled sends, identities, masked email notes and
//! importing messages.

use async_graphql::{Context, InputObject, Object, Result, SimpleObject};
use serde_json::{Map, Value, json};

use super::SharedClient;
use super::lists::ConfirmAction;
use super::types::{
    GqlIdentity, GqlStatus, NonceStore, all_mailboxes, consume_nonce, issue_nonce, mailbox_path,
    resolve_mailbox,
};

/// Most emails one `updateEmails` or `destroyEmails` call takes.
const MAX_IDS: usize = 1000;
/// Most emails one `emptyMailbox` call clears; run it again for the rest.
const MAX_EMPTY: usize = 10_000;

/// Changes to make to emails. Every field is optional and they combine;
/// `moveTo` replaces an email's folders, so it does not mix with
/// `addToMailboxes` or `removeFromMailboxes`.
#[derive(InputObject, Default, Clone)]
pub struct EmailChanges {
    /// Put it in this folder only (by id, path, name or role).
    pub move_to: Option<String>,
    /// Also file it in these folders, keeping the ones it is in: labels.
    pub add_to_mailboxes: Option<Vec<String>>,
    /// Take it out of these folders. An email must stay in at least one.
    pub remove_from_mailboxes: Option<Vec<String>>,
    /// true marks read, false unread.
    pub read: Option<bool>,
    /// true flags (stars), false unflags.
    pub flagged: Option<bool>,
    /// Keywords to set, e.g. "$important" or your own like "receipts".
    pub add_keywords: Option<Vec<String>>,
    /// Keywords to clear.
    pub remove_keywords: Option<Vec<String>>,
}

impl EmailChanges {
    fn is_empty(&self) -> bool {
        self.move_to.is_none()
            && self.add_to_mailboxes.is_none()
            && self.remove_from_mailboxes.is_none()
            && self.read.is_none()
            && self.flagged.is_none()
            && self.add_keywords.is_none()
            && self.remove_keywords.is_none()
    }
}

/// A keyword as JMAP stores it (lowercase), or why it is not one. RFC 8621
/// §4.1.1: 1–255 printable ASCII characters, none of `( ) { ] % * " \`.
fn keyword(raw: &str) -> std::result::Result<String, String> {
    let k = raw.trim();
    let valid = !k.is_empty()
        && k.len() <= 255
        && k.bytes().all(|b| {
            (0x21..=0x7e).contains(&b)
                && !matches!(b, b'(' | b')' | b'{' | b']' | b'%' | b'*' | b'"' | b'\\')
        });
    if valid {
        Ok(k.to_lowercase())
    } else {
        Err(format!(
            "\"{raw}\" is not a keyword: use printable ASCII without spaces or ( ) {{ ] % * \" \\"
        ))
    }
}

/// A JSON Pointer path segment (RFC 6901), for a key inside a patch path.
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// The `Email/set` patch for `changes`, and what it does in words. The outer
/// result fails on a lookup error; the inner one on a change that cannot be
/// made, which the caller reports in its result.
pub(crate) async fn build_patch(
    ctx: &Context<'_>,
    changes: &EmailChanges,
) -> Result<std::result::Result<(Map<String, Value>, Vec<String>), String>> {
    let all = all_mailboxes(ctx).await?;
    let mut patch = Map::new();
    let mut doing = Vec::new();
    let has_labels = changes.add_to_mailboxes.is_some() || changes.remove_from_mailboxes.is_some();

    if let Some(target) = &changes.move_to {
        if has_labels {
            return Ok(Err(
                "moveTo replaces every folder; drop addToMailboxes and removeFromMailboxes, or moveTo."
                    .into(),
            ));
        }
        let m = match resolve_mailbox(&all, target) {
            Ok(m) => m,
            Err(e) => return Ok(Err(e)),
        };
        patch.insert("mailboxIds".into(), json!({ (m.id.clone()): true }));
        doing.push(format!("moved to {}", mailbox_path(&all, m)));
    }
    for (list, on, verb) in [
        (&changes.add_to_mailboxes, true, "added to"),
        (&changes.remove_from_mailboxes, false, "removed from"),
    ] {
        for name in list.iter().flatten() {
            let m = match resolve_mailbox(&all, name) {
                Ok(m) => m,
                Err(e) => return Ok(Err(e)),
            };
            let key = format!("mailboxIds/{}", escape(&m.id));
            if patch.contains_key(&key) {
                return Ok(Err(format!(
                    "{} is both added and removed",
                    mailbox_path(&all, m)
                )));
            }
            patch.insert(key, if on { json!(true) } else { Value::Null });
            doing.push(format!("{verb} {}", mailbox_path(&all, m)));
        }
    }

    let mut keywords: Vec<(String, bool)> = Vec::new();
    if let Some(read) = changes.read {
        keywords.push(("$seen".into(), read));
        doing.push(if read { "marked read" } else { "marked unread" }.into());
    }
    if let Some(flagged) = changes.flagged {
        keywords.push(("$flagged".into(), flagged));
        doing.push(if flagged { "flagged" } else { "unflagged" }.into());
    }
    for (list, on) in [
        (&changes.add_keywords, true),
        (&changes.remove_keywords, false),
    ] {
        for raw in list.iter().flatten() {
            match keyword(raw) {
                Ok(k) => {
                    doing.push(format!(
                        "keyword {k} {}",
                        if on { "set" } else { "cleared" }
                    ));
                    keywords.push((k, on));
                }
                Err(e) => return Ok(Err(e)),
            }
        }
    }
    for (k, on) in keywords {
        let key = format!("keywords/{}", escape(&k));
        let value = if on { json!(true) } else { Value::Null };
        if patch.get(&key).is_some_and(|prev| *prev != value) {
            return Ok(Err(format!("{k} is both set and cleared")));
        }
        patch.insert(key, value);
    }
    Ok(Ok((patch, doing)))
}

/// What a change over many emails did.
#[derive(SimpleObject)]
#[graphql(name = "BulkResult")]
pub struct GqlBulkResult {
    pub success: bool,
    /// Emails changed (or, from PREVIEW, that would be).
    pub affected: i32,
    /// Emails the server refused.
    pub failed: i32,
    pub message: Option<String>,
    /// From PREVIEW: pass it back with CONFIRM and the same arguments.
    pub confirmation_token: Option<String>,
    pub error: Option<String>,
}

impl GqlBulkResult {
    fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            affected: 0,
            failed: 0,
            message: None,
            confirmation_token: None,
            error: Some(error.into()),
        }
    }

    fn done(affected: usize, failed: &[(String, String)], message: String) -> Self {
        Self {
            success: failed.is_empty(),
            affected: affected as i32,
            failed: failed.len() as i32,
            message: Some(message),
            confirmation_token: None,
            error: failed
                .first()
                .map(|(id, why)| format!("{} refused, e.g. {id}: {why}", failed.len())),
        }
    }
}

/// What `importEmail` filed.
#[derive(SimpleObject)]
#[graphql(name = "ImportResult")]
pub struct GqlImportResult {
    pub success: bool,
    pub email_id: Option<String>,
    pub error: Option<String>,
}

fn status(result: crate::error::Result<()>, ok: String) -> GqlStatus {
    match result {
        Ok(()) => GqlStatus {
            success: true,
            message: Some(ok),
            error: None,
        },
        Err(e) => GqlStatus {
            success: false,
            message: None,
            error: Some(e.to_string()),
        },
    }
}

/// Unique, in order: the same email named twice is one email.
fn dedup(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// A few emails, one per line, for a preview.
async fn sample(client: &SharedClient, ids: &[String]) -> Result<String> {
    let shown: Vec<String> = ids.iter().take(8).cloned().collect();
    let mut text = String::new();
    for e in client.get_email_summaries(&shown).await? {
        text.push_str(&format!(
            "- {} | {} | {}\n",
            e.received_at
                .as_deref()
                .and_then(|d| d.get(..10))
                .unwrap_or("?"),
            e.from
                .as_ref()
                .and_then(|f| f.first())
                .map(|a| a.email.as_str())
                .unwrap_or("?"),
            e.subject.as_deref().unwrap_or("(no subject)")
        ));
    }
    if ids.len() > shown.len() {
        text.push_str(&format!("… and {} more\n", ids.len() - shown.len()));
    }
    Ok(text)
}

/// The identity a reference names: its id, or its email address when only
/// one identity has it.
async fn identity_named(client: &SharedClient, reference: &str) -> Result<crate::models::Identity> {
    let all = client.list_identities().await?;
    if let Some(i) = all.iter().find(|i| i.id == reference) {
        return Ok(i.clone());
    }
    let matching: Vec<_> = all
        .iter()
        .filter(|i| i.email.eq_ignore_ascii_case(reference))
        .collect();
    match matching.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(crate::error::Error::IdentityNotFoundForEmail(reference.into()).into()),
        many => Err(async_graphql::Error::new(format!(
            "{} identities send as {reference}: pass an id ({})",
            many.len(),
            many.iter()
                .map(|i| format!("{} \"{}\"", i.id, i.name))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn addresses(list: Option<String>) -> Option<Vec<crate::models::EmailAddress>> {
    list.map(|l| crate::util::parse_addresses(&l))
}

#[derive(Default)]
pub struct ManageMutation;

#[Object]
#[allow(clippy::too_many_arguments)]
impl ManageMutation {
    /// Change emails by ID: flag or unflag, mark read or unread, set or clear
    /// keywords, file into more folders or take out of some (labels), or move.
    /// Up to 1000 IDs. Reversible, so no PREVIEW; for everything a filter
    /// matches, use `sweepEmails`, which takes the same `changes`.
    async fn update_emails(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The emails to change")] email_ids: Vec<String>,
        changes: EmailChanges,
    ) -> Result<GqlBulkResult> {
        let client = ctx.data::<SharedClient>()?;
        let ids = dedup(email_ids);
        if ids.is_empty() || ids.len() > MAX_IDS {
            return Ok(GqlBulkResult::failure(format!(
                "Pass 1 to {MAX_IDS} email IDs."
            )));
        }
        if changes.is_empty() {
            return Ok(GqlBulkResult::failure("Nothing to change."));
        }
        let (patch, doing) = match build_patch(ctx, &changes).await? {
            Ok(p) => p,
            Err(e) => return Ok(GqlBulkResult::failure(e)),
        };
        let outcome = client.update_emails(&ids, &Value::Object(patch)).await?;
        Ok(GqlBulkResult::done(
            outcome.updated as usize,
            &outcome.failed,
            format!("{} emails {}", outcome.updated, doing.join(", ")),
        ))
    }

    /// Delete emails FOR GOOD: gone from every folder, not moved to Trash, and
    /// not recoverable. To bin something recoverably, `moveEmail` or
    /// `updateEmails` it to Trash instead. ALWAYS PREVIEW first, show the user
    /// what it lists, then CONFIRM with the token and the same IDs.
    async fn destroy_emails(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The emails to destroy")] email_ids: Vec<String>,
        #[graphql(desc = "PREVIEW first, then CONFIRM")] action: ConfirmAction,
        #[graphql(desc = "Token from PREVIEW — required for CONFIRM")] confirmation_token: Option<
            String,
        >,
    ) -> Result<GqlBulkResult> {
        let client = ctx.data::<SharedClient>()?;
        let nonce_store = ctx.data::<NonceStore>()?;
        let mut ids = dedup(email_ids);
        if ids.is_empty() || ids.len() > MAX_IDS {
            return Ok(GqlBulkResult::failure(format!(
                "Pass 1 to {MAX_IDS} email IDs."
            )));
        }
        ids.sort();
        let joined = ids.join(",");
        let params = ["destroy_emails", joined.as_str()];

        if action == ConfirmAction::Preview {
            let text = format!(
                "DESTROY PREVIEW — {} emails will be deleted PERMANENTLY, from every folder. \
                 This cannot be undone.\n\n{}\nUse action=CONFIRM with the token and the same IDs to proceed.",
                ids.len(),
                sample(client, &ids).await?
            );
            return Ok(GqlBulkResult {
                success: true,
                affected: ids.len() as i32,
                failed: 0,
                message: Some(text),
                confirmation_token: Some(
                    issue_nonce(nonce_store, client.tenant_key(), &params).await,
                ),
                error: None,
            });
        }
        if let Err(msg) = consume_nonce(
            nonce_store,
            client.tenant_key(),
            confirmation_token.as_deref(),
            &params,
        )
        .await
        {
            return Ok(GqlBulkResult::failure(msg));
        }
        let outcome = client.destroy_emails(&ids).await?;
        Ok(GqlBulkResult::done(
            outcome.destroyed,
            &outcome.failed,
            format!("Destroyed {} emails", outcome.destroyed),
        ))
    }

    /// Empty a folder, as "Empty Trash" does. Mail only in this folder is
    /// deleted FOR GOOD; mail also filed elsewhere is only taken out of it.
    /// Meant for Trash and Junk; any folder works. ALWAYS PREVIEW first, show
    /// the user, then CONFIRM with the token. CONFIRM empties what is there
    /// then, including mail that arrived since the preview. Up to 10000 emails
    /// a call; run it again for more.
    async fn empty_mailbox(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Which mailbox, by id, path, name or role (\"trash\", \"junk\")")]
        mailbox: String,
        #[graphql(desc = "PREVIEW first, then CONFIRM")] action: ConfirmAction,
        #[graphql(desc = "Token from PREVIEW — required for CONFIRM")] confirmation_token: Option<
            String,
        >,
    ) -> Result<GqlBulkResult> {
        let client = ctx.data::<SharedClient>()?;
        let nonce_store = ctx.data::<NonceStore>()?;
        let all = all_mailboxes(ctx).await?;
        let target = match resolve_mailbox(&all, &mailbox) {
            Ok(m) => m.clone(),
            Err(e) => return Ok(GqlBulkResult::failure(e)),
        };
        let path = mailbox_path(&all, &target);
        let params = ["empty_mailbox", target.id.as_str()];

        let (ids, total) = client
            .query_email_ids(json!({ "inMailbox": target.id }), MAX_EMPTY)
            .await?;
        let memberships = client.email_mailbox_ids(&ids).await?;
        let (only_here, elsewhere): (Vec<String>, Vec<String>) = ids.into_iter().partition(|id| {
            memberships
                .get(id)
                .is_none_or(|m| m.iter().all(|mb| *mb == target.id))
        });

        if action == ConfirmAction::Preview {
            let mut text = format!(
                "EMPTY MAILBOX PREVIEW — \"{path}\" holds {total} emails.\n\
                 {} are in no other folder and will be deleted PERMANENTLY (not recoverable).\n\
                 {} are also filed elsewhere and will only be taken out of \"{path}\".\n",
                only_here.len(),
                elsewhere.len()
            );
            if total as usize > only_here.len() + elsewhere.len() {
                text.push_str(
                    "Only the newest 10000 are handled per call; run it again for the rest.\n",
                );
            }
            if !only_here.is_empty() {
                text.push_str("\nTo be destroyed, newest first:\n");
                text.push_str(&sample(client, &only_here).await?);
            }
            text.push_str("\nUse action=CONFIRM with the token to proceed.");
            return Ok(GqlBulkResult {
                success: true,
                affected: (only_here.len() + elsewhere.len()) as i32,
                failed: 0,
                message: Some(text),
                confirmation_token: Some(
                    issue_nonce(nonce_store, client.tenant_key(), &params).await,
                ),
                error: None,
            });
        }
        if let Err(msg) = consume_nonce(
            nonce_store,
            client.tenant_key(),
            confirmation_token.as_deref(),
            &params,
        )
        .await
        {
            return Ok(GqlBulkResult::failure(msg));
        }
        let mut failed = Vec::new();
        let removed = if elsewhere.is_empty() {
            0
        } else {
            let patch = json!({ (format!("mailboxIds/{}", escape(&target.id))): null });
            let outcome = client.update_emails(&elsewhere, &patch).await?;
            failed.extend(outcome.failed);
            outcome.updated as usize
        };
        let destroyed = client.destroy_emails(&only_here).await?;
        failed.extend(destroyed.failed);
        super::mutation::forget_mailboxes(ctx)?;
        Ok(GqlBulkResult::done(
            destroyed.destroyed + removed as usize,
            &failed,
            format!(
                "Emptied \"{path}\": destroyed {}, took {removed} out that are filed elsewhere",
                destroyed.destroyed
            ),
        ))
    }

    /// Stop a scheduled send before it goes (find them with
    /// `scheduledEmails`). The email goes back to Drafts as a draft, to edit
    /// or send again.
    async fn cancel_scheduled_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The submission's id, from scheduledEmails")] submission_id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<SharedClient>()?;
        Ok(status(
            client.cancel_submission(&submission_id).await,
            "Cancelled; the email is back in Drafts.".into(),
        ))
    }

    /// Add a sending identity for an address the account may send as (one
    /// of its own addresses or aliases); the server refuses any other.
    async fn create_identity(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The address to send as")] email: String,
        #[graphql(desc = "Display name")] name: Option<String>,
        #[graphql(desc = "Reply-To addresses, comma-separated")] reply_to: Option<String>,
        #[graphql(desc = "Addresses to BCC on every send, comma-separated")] bcc: Option<String>,
        #[graphql(desc = "Plain-text signature")] text_signature: Option<String>,
        #[graphql(desc = "HTML signature")] html_signature: Option<String>,
    ) -> Result<GqlIdentity> {
        let client = ctx.data::<SharedClient>()?;
        let fields = crate::jmap::IdentityFields {
            name,
            reply_to: addresses(reply_to),
            bcc: addresses(bcc),
            text_signature,
            html_signature,
        };
        Ok(client.create_identity(&email, &fields).await?.into())
    }

    /// Change a sending identity: its display name, Reply-To, automatic BCC
    /// or signatures. Only what is passed changes; an empty string clears a
    /// field. The address itself cannot change.
    async fn update_identity(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The identity's id, or its address when only one sends as it")]
        identity: String,
        #[graphql(desc = "Display name")] name: Option<String>,
        #[graphql(desc = "Reply-To addresses, comma-separated; empty clears")] reply_to: Option<
            String,
        >,
        #[graphql(desc = "Addresses to BCC on every send, comma-separated; empty clears")]
        bcc: Option<String>,
        #[graphql(desc = "Plain-text signature")] text_signature: Option<String>,
        #[graphql(desc = "HTML signature")] html_signature: Option<String>,
    ) -> Result<GqlIdentity> {
        let client = ctx.data::<SharedClient>()?;
        let target = identity_named(client, &identity).await?;
        let fields = crate::jmap::IdentityFields {
            name,
            reply_to: addresses(reply_to),
            bcc: addresses(bcc),
            text_signature,
            html_signature,
        };
        if fields.is_empty() {
            return Err(async_graphql::Error::new(
                "Nothing to change: pass name, replyTo, bcc, textSignature or htmlSignature.",
            ));
        }
        client.update_identity(&target.id, &fields).await?;
        client
            .list_identities()
            .await?
            .into_iter()
            .find(|i| i.id == target.id)
            .map(Into::into)
            .ok_or_else(|| crate::error::Error::IdentityNotFound.into())
    }

    /// Remove a sending identity. Mail is untouched; the address just stops
    /// being one to send as here. Some identities cannot be removed
    /// (`mayDelete`).
    async fn delete_identity(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The identity's id, or its address when only one sends as it")]
        identity: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<SharedClient>()?;
        let target = identity_named(client, &identity).await?;
        Ok(status(
            client.destroy_identity(&target.id).await,
            format!("Removed identity {} <{}>", target.name, target.email),
        ))
    }

    /// Change a masked email's notes: what it is for, its domain and URL.
    /// Only what is passed changes; an empty url clears it.
    async fn update_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The masked email ID")] id: String,
        #[graphql(desc = "A note to remember what this is for")] description: Option<String>,
        #[graphql(desc = "The website/domain this masked email is for")] for_domain: Option<String>,
        #[graphql(desc = "The site's URL")] url: Option<String>,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<SharedClient>()?;
        if description.is_none() && for_domain.is_none() && url.is_none() {
            return Ok(status(
                Err(crate::error::Error::Config(
                    "Nothing to change: pass description, forDomain or url.".into(),
                )),
                String::new(),
            ));
        }
        Ok(status(
            client
                .edit_masked_email(
                    &id,
                    for_domain.as_deref(),
                    description.as_deref(),
                    url.as_deref(),
                )
                .await,
            format!("Masked email {id} updated."),
        ))
    }

    /// File a raw RFC 5322 message (an .eml file) into the mailbox as an
    /// email, without sending anything: restoring an export, or filing an
    /// attached email (its `Attachment.blobId`) as a message of its own.
    async fn import_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The message as base64; or pass blobId")] base64: Option<String>,
        #[graphql(desc = "A blob already uploaded, or an attachment's blobId")] blob_id: Option<
            String,
        >,
        #[graphql(desc = "Folders to file it in, by id, path, name or role (default inbox)")]
        mailboxes: Option<Vec<String>>,
        #[graphql(desc = "Mark it read (default true)")] read: Option<bool>,
        #[graphql(desc = "Keywords to set on it")] keywords: Option<Vec<String>>,
        #[graphql(desc = "Received date, RFC 3339 (default now)")] received_at: Option<String>,
    ) -> Result<GqlImportResult> {
        let client = ctx.data::<SharedClient>()?;
        let fail = |e: String| GqlImportResult {
            success: false,
            email_id: None,
            error: Some(e),
        };
        let blob = match (base64, blob_id) {
            (Some(b64), None) => {
                let bytes = match base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    b64.trim(),
                ) {
                    Ok(b) => b,
                    Err(e) => return Ok(fail(format!("base64 does not decode: {e}"))),
                };
                client.upload_blob(bytes, "message/rfc822").await?
            }
            (None, Some(id)) => id,
            _ => return Ok(fail("Pass base64 or blobId, one of them.".into())),
        };
        let all = all_mailboxes(ctx).await?;
        let mut mailbox_ids = Vec::new();
        for name in mailboxes.unwrap_or_else(|| vec!["inbox".into()]) {
            match resolve_mailbox(&all, &name) {
                Ok(m) => mailbox_ids.push(m.id.clone()),
                Err(e) => return Ok(fail(e)),
            }
        }
        let mut kws = Vec::new();
        if read.unwrap_or(true) {
            kws.push("$seen".to_string());
        }
        for k in keywords.iter().flatten() {
            match keyword(k) {
                Ok(k) => kws.push(k),
                Err(e) => return Ok(fail(e)),
            }
        }
        let received_at = match received_at {
            Some(at) => match crate::jmap::parse_rfc3339(&at) {
                Some(epoch) => Some(crate::jmap::format_utc(epoch)),
                None => {
                    return Ok(fail(format!(
                        "receivedAt \"{at}\" is not RFC 3339 with an offset"
                    )));
                }
            },
            None => None,
        };
        Ok(
            match client
                .import_email(&blob, &mailbox_ids, &kws, received_at.as_deref())
                .await
            {
                Ok(id) => GqlImportResult {
                    success: true,
                    email_id: Some(id),
                    error: None,
                },
                Err(e) => fail(e.to_string()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_lowercased_and_checked() {
        assert_eq!(keyword(" $Important ").unwrap(), "$important");
        assert_eq!(keyword("receipts/2024").unwrap(), "receipts/2024");
        assert!(keyword("two words").is_err());
        assert!(keyword("bad*").is_err());
        assert!(keyword("").is_err());
    }

    #[test]
    fn patch_paths_escape_pointer_characters() {
        assert_eq!(escape("a/b~c"), "a~1b~0c");
    }
}

//! GraphQL query resolvers

use async_graphql::{Context, Object, Result};

use super::loaders::{Emails, to_gql_error};
use super::types::*;

pub struct QueryRoot;

#[Object]
#[allow(clippy::too_many_arguments)]
impl QueryRoot {
    /// List all mailboxes (folders) with unread counts. Start here to discover available folders.
    async fn mailboxes(&self, ctx: &Context<'_>) -> Result<Vec<GqlMailbox>> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;
        let mut mailboxes = client.list_mailboxes().await?;
        mailboxes.sort_by(|a, b| match (&a.role, &b.role) {
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });
        Ok(mailboxes.into_iter().map(GqlMailbox::from).collect())
    }

    /// Look up a single mailbox by name or role. Navigate into it with
    /// `emails { ... }` or around the tree with `parent` / `children`.
    async fn mailbox(
        &self,
        ctx: &Context<'_>,
        #[graphql(
            desc = "Mailbox name (e.g., 'INBOX', 'Sent') or role (e.g., 'inbox', 'sent', 'drafts')"
        )]
        name: String,
    ) -> Result<Option<GqlMailbox>> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;
        match client.find_mailbox(&name).await {
            Ok(mb) => Ok(Some(GqlMailbox::from(mb))),
            Err(crate::error::Error::MailboxNotFound(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List emails in a specific mailbox/folder. Bodies, attachments and threads
    /// on the results resolve lazily, batched across the whole list.
    #[graphql(complexity = "clamp_limit(limit) as usize * child_complexity")]
    async fn emails(
        &self,
        ctx: &Context<'_>,
        #[graphql(
            desc = "Mailbox name (e.g., 'INBOX', 'Sent') or role (e.g., 'inbox', 'sent', 'drafts')"
        )]
        mailbox: String,
        #[graphql(desc = "Maximum number of emails to return (default 25, max 100)")] limit: Option<
            u32,
        >,
    ) -> Result<Vec<GqlEmail>> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;
        let mb = client.find_mailbox(&mailbox).await?;
        let emails = client.list_emails(&mb.id, clamp_limit(limit)).await?;
        Ok(emails.into_iter().map(GqlEmail::summary).collect())
    }

    /// Get a specific email by ID, with full content. Select
    /// `attachments { content { ... } }` to download attachment data in the same
    /// query, or `thread { emails { ... } }` for the whole conversation.
    async fn email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID (from emails or searchEmails queries)")] id: String,
    ) -> Result<Option<GqlEmail>> {
        let loader = ctx.data::<Emails>()?;
        Ok(loader
            .load_one(id)
            .await
            .map_err(to_gql_error)?
            .map(GqlEmail::full))
    }

    /// Get all emails in a thread/conversation with full content. Returns emails sorted
    /// oldest-first. Each email has full body and nested attachment access.
    async fn thread(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Any email ID in the thread")] email_id: String,
    ) -> Result<GqlThread> {
        let loader = ctx.data::<Emails>()?;
        let email = loader
            .load_one(email_id.clone())
            .await
            .map_err(to_gql_error)?
            .ok_or_else(|| async_graphql::Error::new(format!("Email {email_id} not found")))?;
        let thread_id = email
            .thread_id
            .ok_or_else(|| async_graphql::Error::new("Email has no thread ID"))?;
        GqlThread::load(ctx, thread_id).await
    }

    /// Search emails with flexible filters. Bodies, attachments and threads on
    /// the results resolve lazily, batched across the whole result set.
    #[graphql(complexity = "clamp_limit(limit) as usize * child_complexity")]
    async fn search_emails(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "General search — searches subject, body, from, and to fields")]
        query: Option<String>,
        #[graphql(desc = "Search sender address/name")] from: Option<String>,
        #[graphql(desc = "Search recipient address/name")] to: Option<String>,
        #[graphql(desc = "Search CC recipients")] cc: Option<String>,
        #[graphql(desc = "Search subject line only")] subject: Option<String>,
        #[graphql(desc = "Search email body only")] body: Option<String>,
        #[graphql(desc = "Limit search to a specific mailbox/folder")] mailbox: Option<String>,
        #[graphql(desc = "Only emails with attachments")] has_attachment: Option<bool>,
        #[graphql(desc = "Emails before this date (YYYY-MM-DD or ISO 8601)")] before: Option<
            String,
        >,
        #[graphql(desc = "Emails after this date (YYYY-MM-DD or ISO 8601)")] after: Option<String>,
        #[graphql(desc = "Only unread emails")] unread: Option<bool>,
        #[graphql(desc = "Only flagged/starred emails")] flagged: Option<bool>,
        #[graphql(desc = "Maximum number of results (default 25, max 100)")] limit: Option<u32>,
    ) -> Result<Vec<GqlEmail>> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;
        let limit = clamp_limit(limit);

        let filter = crate::commands::SearchFilter {
            text: query,
            from,
            to,
            cc,
            bcc: None,
            subject,
            body,
            mailbox: None,
            has_attachment: has_attachment.unwrap_or(false),
            min_size: None,
            max_size: None,
            before,
            after,
            unread: unread.unwrap_or(false),
            flagged: flagged.unwrap_or(false),
        };

        let mailbox_id = if let Some(ref name) = mailbox {
            client.find_mailbox(name).await.ok().map(|m| m.id)
        } else {
            None
        };

        let emails = client
            .search_emails_filtered(&filter, mailbox_id.as_deref(), limit)
            .await?;
        Ok(emails.into_iter().map(GqlEmail::summary).collect())
    }

    /// List attachment metadata for an email. Select `content` on each attachment to fetch data.
    /// Equivalent to `email(id: ...) { attachments { ... } }`.
    async fn attachments(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID")] email_id: String,
    ) -> Result<Vec<GqlAttachment>> {
        let loader = ctx.data::<Emails>()?;
        let email = loader
            .load_one(email_id.clone())
            .await
            .map_err(to_gql_error)?
            .ok_or_else(|| async_graphql::Error::new(format!("Email {email_id} not found")))?;
        Ok(attachments_of(&email))
    }

    /// Get a single attachment by blob ID. Select `content` to fetch its data.
    async fn attachment(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID the attachment belongs to")] email_id: String,
        #[graphql(desc = "The blob ID of the attachment (from attachments query)")] blob_id: String,
    ) -> Result<Option<GqlAttachment>> {
        let loader = ctx.data::<Emails>()?;
        let email = loader
            .load_one(email_id.clone())
            .await
            .map_err(to_gql_error)?
            .ok_or_else(|| async_graphql::Error::new(format!("Email {email_id} not found")))?;
        Ok(attachments_of(&email)
            .into_iter()
            .find(|a| a.blob_id == blob_id))
    }

    /// List all sender identities on the account. Includes signatures and default reply-to/bcc.
    async fn identities(&self, ctx: &Context<'_>) -> Result<Vec<GqlIdentity>> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let client = client.lock().await;
        let identities = client.list_identities().await?;
        Ok(identities.into_iter().map(GqlIdentity::from).collect())
    }

    /// List all masked email addresses.
    async fn masked_emails(&self, ctx: &Context<'_>) -> Result<Vec<GqlMaskedEmail>> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let client = client.lock().await;
        let mut masked = client.list_masked_emails().await?;
        masked.sort_by(|a, b| {
            let a_enabled = a.state.as_deref() == Some("enabled");
            let b_enabled = b.state.as_deref() == Some("enabled");
            match (a_enabled, b_enabled) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.email.cmp(&b.email),
            }
        });
        Ok(masked.into_iter().map(GqlMaskedEmail::from).collect())
    }

    /// Search contacts by name, email, or organization. Requires FASTMAIL_APP_PASSWORD.
    async fn contacts(
        &self,
        #[graphql(desc = "Search query — matches name, email, or organization")] query: String,
    ) -> Result<Vec<GqlContact>> {
        let config = crate::config::Config::load()?;
        let username = config.get_username().map_err(|_| {
            async_graphql::Error::new("Username not configured. Set FASTMAIL_USERNAME env var.")
        })?;
        let app_password = config.get_app_password().map_err(|_| {
            async_graphql::Error::new(
                "App password not configured. Set FASTMAIL_APP_PASSWORD env var (API tokens don't work for CardDAV).",
            )
        })?;

        let client = crate::carddav::CardDavClient::new(username, app_password);
        let contacts = client.search_contacts(&query).await?;
        Ok(contacts.into_iter().map(GqlContact::from).collect())
    }
}

//! GraphQL query resolvers

use async_graphql::{Context, Object, Result};

use super::connection::{EmailConnection, PageArgs, emails_connection, page_complexity};
use super::filter::{EmailFilter, EmailSort};
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

    /// Query emails with a composable filter, sorting and pagination.
    ///
    /// The filter is a tree: scalar fields on one filter object are AND-ed, and
    /// `and` / `or` / `not` nest arbitrarily, so a single query can express
    /// things like "unread, from either address, but not in Archive". Bodies,
    /// attachments and threads on the results resolve lazily and batched.
    ///
    /// Cursors are email IDs, so `after: "<last id you saw>"` resumes after that
    /// message and stays correct even if mail arrives in between. Select
    /// `totalCount` for the full match count — it is only computed when you ask
    /// for it.
    #[graphql(complexity = "page_complexity(first, last, child_complexity)")]
    async fn emails(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Which emails to match. Omit to match everything.")] filter: Option<
            EmailFilter,
        >,
        #[graphql(desc = "Sort order, most significant first. Defaults to newest first.")]
        sort: Option<Vec<EmailSort>>,
        #[graphql(desc = "Return one email per conversation instead of every message.")]
        collapse_threads: Option<bool>,
        after: Option<String>,
        before: Option<String>,
        first: Option<i32>,
        last: Option<i32>,
    ) -> Result<EmailConnection> {
        emails_connection(
            ctx,
            PageArgs {
                after,
                before,
                first,
                last,
            },
            filter,
            sort,
            None,
            collapse_threads.unwrap_or(false),
        )
        .await
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

    /// Search emails with flat filters.
    ///
    /// Superseded by `emails(filter: ...)`, which composes with `and`/`or`/`not`,
    /// sorts, and paginates. Kept as a shim: every argument here maps onto one
    /// leaf of an `EmailFilter`.
    #[graphql(
        deprecation = "Use `emails(filter: {...})` — it composes with and/or/not, sorts, and paginates.",
        complexity = "page_complexity(first, None, child_complexity)"
    )]
    #[allow(clippy::too_many_arguments)]
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
        #[graphql(desc = "Maximum number of results (default 25, max 100)")] first: Option<i32>,
    ) -> Result<EmailConnection> {
        let filter = EmailFilter {
            text: query,
            from,
            to,
            cc,
            subject,
            body,
            in_mailbox: mailbox,
            has_attachment,
            before,
            after,
            // The old arguments were `false`-means-unset, so only constrain on true.
            unread: unread.filter(|u| *u),
            flagged: flagged.filter(|f| *f),
            ..Default::default()
        };

        emails_connection(
            ctx,
            PageArgs {
                after: None,
                before: None,
                first,
                last: None,
            },
            Some(filter),
            None,
            None,
            false,
        )
        .await
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

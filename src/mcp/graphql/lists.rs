//! GraphQL types for mailing lists, unsubscribing and bulk sweeps.

use async_graphql::{Enum, SimpleObject};

use crate::jmap::lists::{MailingList, MailingListReport, UnsubscribeOptions};

/// How an email says it can be left, from `List-Unsubscribe` and
/// `List-Unsubscribe-Post`.
#[derive(SimpleObject, Clone)]
#[graphql(name = "UnsubscribeOptions")]
pub struct GqlUnsubscribeOptions {
    /// The email the links came from; pass it to `unsubscribe`.
    pub email_id: String,
    /// RFC 8058 one-click: `unsubscribe` can finish it with one POST, no
    /// browser and no mail sent.
    pub one_click: bool,
    /// Web links. Without `oneClick` these are pages that need a browser.
    pub http: Vec<String>,
    /// `mailto:` links; `unsubscribe` sends the request when there is no
    /// one-click link.
    pub mailto: Vec<String>,
}

impl From<UnsubscribeOptions> for GqlUnsubscribeOptions {
    fn from(o: UnsubscribeOptions) -> Self {
        Self {
            email_id: o.email_id,
            one_click: o.one_click,
            http: o.http,
            mailto: o.mailto,
        }
    }
}

/// Everything one sender has sent with an unsubscribe header: a newsletter,
/// a shop's marketing, a discussion list.
#[derive(SimpleObject)]
#[graphql(name = "MailingList")]
pub struct GqlMailingList {
    /// Lower-cased sender address, which the mail is grouped by.
    pub sender_email: String,
    pub sender_name: Option<String>,
    /// Emails from this sender among those scanned.
    pub count: i32,
    /// How many of them were never opened: a good sign it is unwanted.
    pub unread_count: i32,
    /// Distinct `List-Id` values, most recent first, at most five.
    pub list_ids: Vec<String>,
    pub latest_email_id: String,
    pub latest_subject: Option<String>,
    pub latest_received_at: Option<String>,
    pub first_received_at: Option<String>,
    /// From the newest email offering a way out, since older links expire.
    /// Null when none of them does.
    pub unsubscribe: Option<GqlUnsubscribeOptions>,
}

impl From<MailingList> for GqlMailingList {
    fn from(l: MailingList) -> Self {
        Self {
            sender_email: l.sender_email,
            sender_name: l.sender_name,
            count: l.count as i32,
            unread_count: l.unread_count as i32,
            list_ids: l.list_ids,
            latest_email_id: l.latest_email_id,
            latest_subject: l.latest_subject,
            latest_received_at: l.latest_received_at,
            first_received_at: l.first_received_at,
            unsubscribe: l.unsubscribe.map(Into::into),
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "MailingListReport")]
pub struct GqlMailingListReport {
    /// Emails read for this report.
    pub scanned: i32,
    /// Emails matching; more than `scanned` when `scanLimit` cut it short.
    pub total: i32,
    /// One per sender, most prolific first.
    pub lists: Vec<GqlMailingList>,
}

impl From<MailingListReport> for GqlMailingListReport {
    fn from(r: MailingListReport) -> Self {
        Self {
            scanned: r.scanned as i32,
            total: r.total as i32,
            lists: r.lists.into_iter().map(Into::into).collect(),
        }
    }
}

/// Two-step actions: look first, then act with the token the look returned.
#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum ConfirmAction {
    /// Show what would happen and return a confirmationToken. Changes nothing.
    Preview,
    /// Do it; needs the token from PREVIEW and the same arguments.
    Confirm,
}

/// How `unsubscribe` leaves (or would leave) a list.
#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum UnsubscribeMethod {
    /// RFC 8058 one-click POST.
    OneClick,
    /// An email to the list's `mailto:` address.
    Mailto,
    /// Only a web page is offered; see `manualUrls`.
    Manual,
}

impl UnsubscribeMethod {
    pub fn of(plan: &crate::unsubscribe::Plan) -> Self {
        match plan {
            crate::unsubscribe::Plan::OneClick(_) => Self::OneClick,
            crate::unsubscribe::Plan::Mailto(_) => Self::Mailto,
            crate::unsubscribe::Plan::Manual(_) => Self::Manual,
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "UnsubscribeResult")]
pub struct GqlUnsubscribeResult {
    pub success: bool,
    pub method: UnsubscribeMethod,
    /// PREVIEW: what CONFIRM will do. CONFIRM: what happened.
    pub message: String,
    pub confirmation_token: Option<String>,
    /// Links that need a browser to finish, when it could not be done here.
    pub manual_urls: Vec<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "SweepResult")]
pub struct GqlSweepResult {
    pub success: bool,
    /// Emails the filter matches in all.
    pub matched: i32,
    /// Emails changed (CONFIRM), or that would be (PREVIEW), up to `limit`.
    pub affected: i32,
    /// Emails the server refused to change.
    pub failed: i32,
    pub preview: Option<String>,
    pub confirmation_token: Option<String>,
    pub error: Option<String>,
}

//! Formatting helpers for MCP tool output

use crate::carddav::Contact;
use crate::models::{Email, EmailAddress, Mailbox, MaskedEmail};

pub fn format_address(addr: &EmailAddress) -> String {
    match &addr.name {
        Some(name) if !name.is_empty() => format!("{} <{}>", name, addr.email),
        _ => addr.email.clone(),
    }
}

pub fn format_address_list(addrs: Option<&Vec<EmailAddress>>) -> String {
    match addrs {
        None => "(none)".to_string(),
        Some(v) if v.is_empty() => "(none)".to_string(),
        Some(addrs) => addrs
            .iter()
            .map(format_address)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

pub fn format_mailbox(m: &Mailbox) -> String {
    let role = m
        .role
        .as_ref()
        .map(|r| format!(" [{}]", r))
        .unwrap_or_default();
    let unread = if m.unread_emails > 0 {
        format!(" ({} unread)", m.unread_emails)
    } else {
        String::new()
    };
    format!(
        "{}{}{} - {} emails (id: {})",
        m.name, role, unread, m.total_emails, m.id
    )
}

pub fn format_email_summary(e: &Email) -> String {
    let from = format_address_list(e.from.as_ref());
    let date = e.received_at.as_deref().unwrap_or("unknown");
    let attachment = if e.has_attachment {
        " [attachment]"
    } else {
        ""
    };
    let unread = if e.is_unread() { " [UNREAD]" } else { "" };

    format!(
        "{}{}\n\
        ID: {}\n\
        From: {}\n\
        Subject: {}\n\
        Date: {}\n\
        Preview: {}",
        unread,
        attachment,
        e.id,
        from,
        e.subject.as_deref().unwrap_or("(no subject)"),
        date,
        e.preview.as_deref().unwrap_or("")
    )
}

pub fn format_email_full(e: &Email) -> String {
    let from = format_address_list(e.from.as_ref());
    let to = format_address_list(e.to.as_ref());
    let cc = format_address_list(e.cc.as_ref());
    let date = e.received_at.as_deref().unwrap_or("unknown");

    // Get body text
    let body = e.text_content().unwrap_or("");

    format!(
        "ID: {}\n\
        Thread ID: {}\n\
        From: {}\n\
        To: {}\n\
        CC: {}\n\
        Subject: {}\n\
        Date: {}\n\
        Has Attachment: {}\n\n\
        --- Body ---\n\
        {}",
        e.id,
        e.thread_id.as_deref().unwrap_or("(none)"),
        from,
        to,
        cc,
        e.subject.as_deref().unwrap_or("(no subject)"),
        date,
        e.has_attachment,
        body
    )
}

pub fn format_masked_email(m: &MaskedEmail) -> String {
    let state = m.state.as_deref().unwrap_or("unknown");
    let state_indicator = match state {
        "enabled" => "[ENABLED]",
        "disabled" => "[DISABLED]",
        "pending" => "[PENDING]",
        "deleted" => "[DELETED]",
        _ => "[?]",
    };

    let mut lines = vec![
        format!("{} {}", state_indicator, m.email),
        format!("ID: {}", m.id),
    ];

    if let Some(ref domain) = m.for_domain {
        lines.push(format!("For: {}", domain));
    }
    if let Some(ref desc) = m.description {
        lines.push(format!("Description: {}", desc));
    }
    if let Some(ref last) = m.last_message_at {
        lines.push(format!("Last message: {}", last));
    }
    if let Some(ref created) = m.created_at {
        lines.push(format!("Created: {}", created));
    }

    lines.join("\n")
}

pub fn format_contact(c: &Contact) -> String {
    let mut lines = vec![format!("**{}**", c.name)];

    if !c.emails.is_empty() {
        let emails: Vec<String> = c
            .emails
            .iter()
            .map(|e| {
                let label = e
                    .label
                    .as_ref()
                    .map(|l| l.trim_end_matches(';').to_lowercase())
                    .filter(|l| !l.is_empty())
                    .map(|l| format!(" ({})", l))
                    .unwrap_or_default();
                format!("  {}{}", e.email, label)
            })
            .collect();
        lines.push(format!("Emails:\n{}", emails.join("\n")));
    }

    if !c.phones.is_empty() {
        let phones: Vec<String> = c
            .phones
            .iter()
            .map(|p| {
                let label = p
                    .label
                    .as_ref()
                    .map(|l| l.trim_end_matches(';').to_lowercase())
                    .filter(|l| !l.is_empty())
                    .map(|l| format!(" ({})", l))
                    .unwrap_or_default();
                format!("  {}{}", p.number, label)
            })
            .collect();
        lines.push(format!("Phones:\n{}", phones.join("\n")));
    }

    if let Some(ref org) = c.organization {
        let org = org.trim_end_matches(';').trim();
        if !org.is_empty() {
            lines.push(format!("Organization: {}", org));
        }
    }

    if let Some(ref title) = c.title {
        if !title.is_empty() {
            lines.push(format!("Title: {}", title));
        }
    }

    lines.join("\n")
}

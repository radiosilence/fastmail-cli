//! CardDAV client for Fastmail contacts
//!
//! Uses raw HTTP with reqwest since CardDAV is just WebDAV with vCard.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::error::{Error, Result};

const CARDDAV_BASE: &str = "https://carddav.fastmail.com";

// Per RFC 3986, these chars need escaping when interpolating into a URL path
// segment. `/` is the segment delimiter and must be escaped to stay in-segment.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

/// A contact parsed from vCard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Unique ID (from UID property)
    pub id: String,
    /// Full name (FN property)
    pub name: String,
    /// Email addresses
    pub emails: Vec<ContactEmail>,
    /// Phone numbers
    pub phones: Vec<ContactPhone>,
    /// Organization/company
    pub organization: Option<String>,
    /// Job title
    pub title: Option<String>,
    /// Notes
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactEmail {
    pub email: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactPhone {
    pub number: String,
    pub label: Option<String>,
}

/// Address book info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressBook {
    pub href: String,
    pub name: String,
}

/// Fields for creating or updating a contact.
/// All fields are optional for updates (only provided fields are changed).
#[derive(Debug, Clone, Default)]
pub struct ContactFields<'a> {
    pub name: Option<&'a str>,
    pub emails: Option<&'a [ContactEmail]>,
    pub phones: Option<&'a [ContactPhone]>,
    pub organization: Option<&'a str>,
    pub title: Option<&'a str>,
    pub notes: Option<&'a str>,
}

/// CardDAV client
pub struct CardDavClient {
    client: Client,
    username: String,
    app_password: String,
}

impl CardDavClient {
    pub fn new(username: String, app_password: String) -> Self {
        Self {
            client: Client::new(),
            username,
            app_password,
        }
    }

    /// Discover address books for the user
    #[instrument(skip(self))]
    pub async fn list_addressbooks(&self) -> Result<Vec<AddressBook>> {
        let encoded_user = utf8_percent_encode(&self.username, PATH_SEGMENT);
        let url = format!("{}/dav/addressbooks/user/{}/", CARDDAV_BASE, encoded_user);

        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
  </d:prop>
</d:propfind>"#;

        let response = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "application/xml")
            .header("Depth", "1")
            .body(body)
            .send()
            .await?;

        let status = response.status();
        let text: String = response.text().await?;

        debug!(status = %status, "PROPFIND response");

        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::Server(format!(
                "CardDAV PROPFIND failed: {} - {}",
                status, text
            )));
        }

        // Parse the multistatus XML response
        self.parse_addressbooks_response(&text)
    }

    fn parse_addressbooks_response(&self, xml: &str) -> Result<Vec<AddressBook>> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| Error::Server(format!("Failed to parse XML: {e}")))?;

        let dav_ns = "DAV:";
        let carddav_ns = "urn:ietf:params:xml:ns:carddav";
        let mut addressbooks = Vec::new();

        for response in doc
            .descendants()
            .filter(|n| n.has_tag_name((dav_ns, "response")))
        {
            let href = response
                .descendants()
                .find(|n| n.has_tag_name((dav_ns, "href")))
                .and_then(|n| n.text())
                .unwrap_or_default();

            // Check if this is an addressbook (has carddav:addressbook resourcetype)
            let is_addressbook = response
                .descendants()
                .any(|n| n.has_tag_name((carddav_ns, "addressbook")));

            if is_addressbook && !href.is_empty() {
                let displayname = response
                    .descendants()
                    .find(|n| n.has_tag_name((dav_ns, "displayname")))
                    .and_then(|n| n.text());

                let name = displayname.map(|s| s.to_string()).unwrap_or_else(|| {
                    href.split('/')
                        .rfind(|s| !s.is_empty())
                        .unwrap_or("Unknown")
                        .to_string()
                });

                // Skip the parent collection itself
                if !href.ends_with(&format!("{}/", self.username)) {
                    addressbooks.push(AddressBook {
                        href: href.to_string(),
                        name,
                    });
                }
            }
        }

        Ok(addressbooks)
    }

    /// List all contacts in an address book
    #[instrument(skip(self))]
    pub async fn list_contacts(&self, addressbook_href: &str) -> Result<Vec<Contact>> {
        let url = format!("{}{}", CARDDAV_BASE, addressbook_href);

        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<card:addressbook-query xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop>
    <d:getetag/>
    <card:address-data/>
  </d:prop>
</card:addressbook-query>"#;

        let response = self
            .client
            .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), &url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "application/xml")
            .header("Depth", "1")
            .body(body)
            .send()
            .await?;

        let status = response.status();
        let text: String = response.text().await?;

        debug!(status = %status, "REPORT response");

        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::Server(format!(
                "CardDAV REPORT failed: {} - {}",
                status, text
            )));
        }

        self.parse_contacts_response(&text)
    }

    fn parse_contacts_response(&self, xml: &str) -> Result<Vec<Contact>> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| Error::Server(format!("Failed to parse XML: {e}")))?;

        let dav_ns = "DAV:";
        let carddav_ns = "urn:ietf:params:xml:ns:carddav";
        let mut contacts = Vec::new();

        for response in doc
            .descendants()
            .filter(|n| n.has_tag_name((dav_ns, "response")))
        {
            if let Some(vcard_data) = response
                .descendants()
                .find(|n| n.has_tag_name((carddav_ns, "address-data")))
                .and_then(|n| n.text())
                && let Some(contact) = parse_vcard(vcard_data)
            {
                contacts.push(contact);
            }
        }

        contacts.sort_by_key(|c| c.name.to_lowercase());
        Ok(contacts)
    }

    /// Search contacts by name or email
    pub async fn search_contacts(&self, query: &str) -> Result<Vec<Contact>> {
        // Get all contacts from all addressbooks and filter
        let addressbooks = self.list_addressbooks().await?;
        let mut all_contacts = Vec::new();

        for ab in addressbooks {
            let contacts = self.list_contacts(&ab.href).await?;
            all_contacts.extend(contacts);
        }

        let query_lower = query.to_lowercase();
        let filtered: Vec<Contact> = all_contacts
            .into_iter()
            .filter(|c| {
                c.name.to_lowercase().contains(&query_lower)
                    || c.emails
                        .iter()
                        .any(|e| e.email.to_lowercase().contains(&query_lower))
                    || c.organization
                        .as_ref()
                        .is_some_and(|o| o.to_lowercase().contains(&query_lower))
            })
            .collect();

        Ok(filtered)
    }

    /// Get the first (default) address book href
    async fn default_addressbook(&self) -> Result<String> {
        let addressbooks = self.list_addressbooks().await?;
        addressbooks
            .into_iter()
            .next()
            .map(|ab| ab.href)
            .ok_or(Error::Server("No address books found".to_string()))
    }

    /// Find a contact's CardDAV href and current vCard data by UID.
    /// Returns (href, vcard_string) so we can PUT back to the same URL.
    async fn find_contact_href(&self, contact_id: &str) -> Result<Option<(String, String)>> {
        let addressbooks = self.list_addressbooks().await?;

        for ab in addressbooks {
            let url = format!("{}{}", CARDDAV_BASE, ab.href);

            let body = r#"<?xml version="1.0" encoding="utf-8"?>
<card:addressbook-query xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop>
    <d:getetag/>
    <card:address-data/>
  </d:prop>
</card:addressbook-query>"#;

            let response = self
                .client
                .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), &url)
                .basic_auth(&self.username, Some(&self.app_password))
                .header("Content-Type", "application/xml")
                .header("Depth", "1")
                .body(body)
                .send()
                .await?;

            let status = response.status();
            let text: String = response.text().await?;

            if !status.is_success() && status.as_u16() != 207 {
                continue;
            }

            let doc = roxmltree::Document::parse(&text)
                .map_err(|e| Error::Server(format!("Failed to parse XML: {e}")))?;

            let dav_ns = "DAV:";
            let carddav_ns = "urn:ietf:params:xml:ns:carddav";

            for response in doc
                .descendants()
                .filter(|n| n.has_tag_name((dav_ns, "response")))
            {
                let href = response
                    .descendants()
                    .find(|n| n.has_tag_name((dav_ns, "href")))
                    .and_then(|n| n.text())
                    .unwrap_or_default();

                if let Some(vcard_data) = response
                    .descendants()
                    .find(|n| n.has_tag_name((carddav_ns, "address-data")))
                    .and_then(|n| n.text())
                {
                    let unfolded = unfold_vcard(vcard_data);
                    for line in unfolded.lines() {
                        if line.starts_with("UID") && line.contains(':') {
                            let uid = line.split_once(':').map(|(_, v)| v).unwrap_or("");
                            if uid == contact_id {
                                return Ok(Some((href.to_string(), vcard_data.to_string())));
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    /// Create a new contact. Returns the created Contact.
    /// `fields.name` is required for creation.
    #[instrument(skip(self, fields))]
    pub async fn create_contact(&self, fields: &ContactFields<'_>) -> Result<Contact> {
        let name = fields.name.ok_or(Error::Server(
            "Name is required to create a contact".to_string(),
        ))?;
        let emails = fields.emails.unwrap_or(&[]);
        let phones = fields.phones.unwrap_or(&[]);

        let ab_href = self.default_addressbook().await?;
        let uid = generate_uid();
        let vcard = build_vcard(
            &uid,
            name,
            emails,
            phones,
            fields.organization,
            fields.title,
            fields.notes,
        );

        let url = format!("{}{}{}.vcf", CARDDAV_BASE, ab_href, uid);
        debug!(url = %url, "Creating contact");

        let response = self
            .client
            .put(&url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "text/vcard; charset=utf-8")
            .header("If-None-Match", "*")
            .body(vcard)
            .send()
            .await?;

        let status = response.status();
        let text: String = response.text().await?;

        if !status.is_success() && status.as_u16() != 201 && status.as_u16() != 204 {
            return Err(Error::Server(format!(
                "CardDAV PUT failed: {} - {}",
                status, text
            )));
        }

        Ok(Contact {
            id: uid,
            name: name.to_string(),
            emails: emails.to_vec(),
            phones: phones.to_vec(),
            organization: fields.organization.map(String::from),
            title: fields.title.map(String::from),
            notes: fields.notes.map(String::from),
        })
    }

    /// Update an existing contact. Merges provided fields with existing data.
    /// Returns the updated Contact.
    #[instrument(skip(self, fields))]
    pub async fn update_contact(
        &self,
        contact_id: &str,
        fields: &ContactFields<'_>,
    ) -> Result<Contact> {
        let (href, existing_vcard) = self
            .find_contact_href(contact_id)
            .await?
            .ok_or_else(|| Error::Server(format!("Contact not found: {contact_id}")))?;

        // Parse existing contact to merge with
        let existing = parse_vcard(&existing_vcard)
            .ok_or_else(|| Error::Server("Failed to parse existing contact".to_string()))?;

        let final_name = fields.name.unwrap_or(&existing.name);
        let owned_emails;
        let final_emails = match fields.emails {
            Some(e) => e,
            None => {
                owned_emails = existing.emails;
                &owned_emails
            }
        };
        let owned_phones;
        let final_phones = match fields.phones {
            Some(p) => p,
            None => {
                owned_phones = existing.phones;
                &owned_phones
            }
        };
        let final_org = fields.organization.or(existing.organization.as_deref());
        let final_title = fields.title.or(existing.title.as_deref());
        let final_notes = fields.notes.or(existing.notes.as_deref());

        let vcard = build_vcard(
            contact_id,
            final_name,
            final_emails,
            final_phones,
            final_org,
            final_title,
            final_notes,
        );

        let url = format!("{}{}", CARDDAV_BASE, href);
        debug!(url = %url, "Updating contact");

        let response = self
            .client
            .put(&url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "text/vcard; charset=utf-8")
            .body(vcard)
            .send()
            .await?;

        let status = response.status();
        let text: String = response.text().await?;

        if !status.is_success() && status.as_u16() != 204 {
            return Err(Error::Server(format!(
                "CardDAV PUT failed: {} - {}",
                status, text
            )));
        }

        Ok(Contact {
            id: contact_id.to_string(),
            name: final_name.to_string(),
            emails: final_emails.to_vec(),
            phones: final_phones.to_vec(),
            organization: final_org.map(String::from),
            title: final_title.map(String::from),
            notes: final_notes.map(String::from),
        })
    }

    /// Delete a contact by ID.
    #[instrument(skip(self))]
    pub async fn delete_contact(&self, contact_id: &str) -> Result<()> {
        let (href, _) = self
            .find_contact_href(contact_id)
            .await?
            .ok_or_else(|| Error::Server(format!("Contact not found: {contact_id}")))?;

        let url = format!("{}{}", CARDDAV_BASE, href);
        debug!(url = %url, "Deleting contact");

        let response = self
            .client
            .delete(&url)
            .basic_auth(&self.username, Some(&self.app_password))
            .send()
            .await?;

        let status = response.status();
        let text: String = response.text().await?;

        if !status.is_success() && status.as_u16() != 204 {
            return Err(Error::Server(format!(
                "CardDAV DELETE failed: {} - {}",
                status, text
            )));
        }

        Ok(())
    }
}

/// Unfold vCard lines per RFC 6350 §3.2: continuation lines start with a space or tab.
fn unfold_vcard(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    for line in raw.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line — append without the leading whitespace
            result.push_str(&line[1..]);
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result
}

/// Decode quoted-printable encoded value (basic implementation for vCard)
fn decode_qp(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded_bytes = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            if bytes[i + 1] == b'\r' || bytes[i + 1] == b'\n' {
                // Soft line break — skip
                i += 2;
                if i < bytes.len() && bytes[i] == b'\n' {
                    i += 1;
                }
            } else if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                decoded_bytes.push((hi * 16 + lo) as u8);
                i += 3;
            } else {
                decoded_bytes.push(b'=');
                i += 1;
            }
        } else {
            decoded_bytes.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded_bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Parse a vCard string into a Contact
fn parse_vcard(vcard_str: &str) -> Option<Contact> {
    let unfolded = unfold_vcard(vcard_str);
    let mut id = String::new();
    let mut name = String::new();
    let mut emails = Vec::new();
    let mut phones = Vec::new();
    let mut organization = None;
    let mut title = None;
    let mut notes = None;

    for line in unfolded.lines() {
        let line = line.trim();

        // Extract property value, handling optional parameters and QP encoding
        let extract_value = |line: &str| -> String {
            let value = line.split_once(':').map(|(_, v)| v).unwrap_or("");
            if line.to_uppercase().contains("ENCODING=QUOTED-PRINTABLE") {
                decode_qp(value)
            } else {
                value.to_string()
            }
        };

        if line.starts_with("UID") && line.contains(':') {
            id = extract_value(line);
        } else if line.starts_with("FN") && line.contains(':') {
            name = extract_value(line);
        } else if line.starts_with("EMAIL") {
            // EMAIL;TYPE=work:bob@example.com or EMAIL:bob@example.com
            let label = if line.contains("TYPE=") {
                line.split("TYPE=")
                    .nth(1)
                    .and_then(|s| s.split(':').next())
                    .map(|s| s.to_string())
            } else {
                None
            };
            let email = line.split(':').next_back().unwrap_or("").to_string();
            if !email.is_empty() {
                emails.push(ContactEmail { email, label });
            }
        } else if line.starts_with("TEL") {
            let label = if line.contains("TYPE=") {
                line.split("TYPE=")
                    .nth(1)
                    .and_then(|s| s.split(':').next())
                    .or_else(|| line.split("TYPE=").nth(1).and_then(|s| s.split(';').next()))
                    .map(|s| s.to_string())
            } else {
                None
            };
            let number = line.split(':').next_back().unwrap_or("").to_string();
            if !number.is_empty() {
                phones.push(ContactPhone { number, label });
            }
        } else if line.starts_with("ORG") && line.contains(':') {
            organization = Some(extract_value(line));
        } else if line.starts_with("TITLE") && line.contains(':') {
            title = Some(extract_value(line));
        } else if line.starts_with("NOTE") && line.contains(':') {
            notes = Some(extract_value(line));
        }
    }

    // Need at least a name
    if name.is_empty() {
        return None;
    }

    // Generate ID if not present
    if id.is_empty() {
        id = format!("{:x}", hash_id(&name));
    }

    Some(Contact {
        id,
        name,
        emails,
        phones,
        organization,
        title,
        notes,
    })
}

/// Simple SipHash-based hash for generating stable contact IDs
fn hash_id(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Generate a UUID-like UID for new contacts
fn generate_uid() -> String {
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Mix timestamp nanos with a counter for uniqueness
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    now.as_nanos().hash(&mut hasher);
    count.hash(&mut hasher);
    let hi = hasher.finish();
    // Second hash with different seed for lower bits
    now.as_nanos()
        .wrapping_mul(6364136223846793005)
        .hash(&mut hasher);
    let lo = hasher.finish();
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (hi >> 32) as u32,
        (hi >> 16) as u16,
        hi as u16,
        (lo >> 48) as u16,
        lo & 0xFFFF_FFFF_FFFF
    )
}

/// Build a vCard 3.0 string from contact fields
fn build_vcard(
    uid: &str,
    name: &str,
    emails: &[ContactEmail],
    phones: &[ContactPhone],
    organization: Option<&str>,
    title: Option<&str>,
    notes: Option<&str>,
) -> String {
    let mut lines = vec![
        "BEGIN:VCARD".to_string(),
        "VERSION:3.0".to_string(),
        format!("UID:{uid}"),
        format!("FN:{name}"),
    ];

    // N property — split FN into family/given (best-effort)
    let parts: Vec<&str> = name.splitn(2, ' ').collect();
    if parts.len() == 2 {
        lines.push(format!("N:{};{};;;", parts[1], parts[0]));
    } else {
        lines.push(format!("N:{name};;;;"));
    }

    for email in emails {
        if let Some(ref label) = email.label {
            lines.push(format!("EMAIL;TYPE={label}:{}", email.email));
        } else {
            lines.push(format!("EMAIL:{}", email.email));
        }
    }

    for phone in phones {
        if let Some(ref label) = phone.label {
            lines.push(format!("TEL;TYPE={label}:{}", phone.number));
        } else {
            lines.push(format!("TEL:{}", phone.number));
        }
    }

    if let Some(org) = organization {
        lines.push(format!("ORG:{org}"));
    }

    if let Some(t) = title {
        lines.push(format!("TITLE:{t}"));
    }

    if let Some(n) = notes {
        lines.push(format!("NOTE:{n}"));
    }

    lines.push("END:VCARD".to_string());
    lines.join("\r\n") + "\r\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unfold_vcard_lines() {
        // RFC 6350 §3.2: leading space/tab is the fold indicator and is consumed
        let input = "FN:John\n  Doe\nEMAIL:john@example.com";
        let result = unfold_vcard(input);
        assert_eq!(result, "FN:John Doe\nEMAIL:john@example.com");
    }

    #[test]
    fn test_unfold_tab_continuation() {
        let input = "FN:John\n\tDoe";
        let result = unfold_vcard(input);
        assert_eq!(result, "FN:JohnDoe");
    }

    #[test]
    fn test_decode_qp_basic() {
        assert_eq!(decode_qp("hello=20world"), "hello world");
        assert_eq!(decode_qp("caf=C3=A9"), "café");
    }

    #[test]
    fn test_decode_qp_soft_linebreak() {
        assert_eq!(decode_qp("hello=\nworld"), "helloworld");
    }

    #[test]
    fn test_parse_vcard_basic() {
        let vcard = "BEGIN:VCARD\nVERSION:3.0\nUID:abc123\nFN:Alice Smith\nEMAIL:alice@example.com\nEND:VCARD";
        let contact = parse_vcard(vcard).unwrap();
        assert_eq!(contact.id, "abc123");
        assert_eq!(contact.name, "Alice Smith");
        assert_eq!(contact.emails.len(), 1);
        assert_eq!(contact.emails[0].email, "alice@example.com");
    }

    #[test]
    fn test_parse_vcard_with_line_folding() {
        // Fold happens mid-value: "Very Long Name Here" folded after "Na"
        // Continuation line starts with space (fold indicator consumed)
        let vcard = "BEGIN:VCARD\nFN:Very Long Na\n me Here\nEMAIL:test@example.com\nEND:VCARD";
        let contact = parse_vcard(vcard).unwrap();
        assert_eq!(contact.name, "Very Long Name Here");
    }

    #[test]
    fn test_parse_vcard_with_params() {
        let vcard = "BEGIN:VCARD\nFN:Bob\nEMAIL;TYPE=work:bob@work.com\nTEL;TYPE=cell:+1234567890\nORG:Acme Inc\nTITLE:Engineer\nEND:VCARD";
        let contact = parse_vcard(vcard).unwrap();
        assert_eq!(contact.emails[0].email, "bob@work.com");
        assert_eq!(contact.emails[0].label, Some("work".to_string()));
        assert_eq!(contact.phones[0].number, "+1234567890");
        assert_eq!(contact.organization, Some("Acme Inc".to_string()));
        assert_eq!(contact.title, Some("Engineer".to_string()));
    }

    #[test]
    fn test_parse_vcard_generates_id_when_missing() {
        let vcard = "BEGIN:VCARD\nFN:No UID\nEND:VCARD";
        let contact = parse_vcard(vcard).unwrap();
        assert!(!contact.id.is_empty());
    }

    #[test]
    fn test_parse_vcard_returns_none_without_name() {
        let vcard = "BEGIN:VCARD\nUID:abc\nEMAIL:test@example.com\nEND:VCARD";
        assert!(parse_vcard(vcard).is_none());
    }

    #[test]
    fn test_build_vcard_basic() {
        let vcard = build_vcard(
            "test-uid-123",
            "Jane Doe",
            &[ContactEmail {
                email: "jane@example.com".to_string(),
                label: None,
            }],
            &[],
            Some("Acme Corp"),
            None,
            None,
        );
        assert!(vcard.contains("BEGIN:VCARD"));
        assert!(vcard.contains("VERSION:3.0"));
        assert!(vcard.contains("UID:test-uid-123"));
        assert!(vcard.contains("FN:Jane Doe"));
        assert!(vcard.contains("N:Doe;Jane;;;"));
        assert!(vcard.contains("EMAIL:jane@example.com"));
        assert!(vcard.contains("ORG:Acme Corp"));
        assert!(vcard.contains("END:VCARD"));
    }

    #[test]
    fn test_build_vcard_with_labels() {
        let vcard = build_vcard(
            "uid-456",
            "Bob",
            &[ContactEmail {
                email: "bob@work.com".to_string(),
                label: Some("work".to_string()),
            }],
            &[ContactPhone {
                number: "+1234567890".to_string(),
                label: Some("cell".to_string()),
            }],
            None,
            Some("Engineer"),
            Some("A note"),
        );
        assert!(vcard.contains("EMAIL;TYPE=work:bob@work.com"));
        assert!(vcard.contains("TEL;TYPE=cell:+1234567890"));
        assert!(vcard.contains("N:Bob;;;;"));
        assert!(vcard.contains("TITLE:Engineer"));
        assert!(vcard.contains("NOTE:A note"));
    }

    #[test]
    fn test_build_vcard_roundtrips() {
        let vcard = build_vcard(
            "roundtrip-uid",
            "Alice Smith",
            &[
                ContactEmail {
                    email: "alice@home.com".to_string(),
                    label: Some("home".to_string()),
                },
                ContactEmail {
                    email: "alice@work.com".to_string(),
                    label: Some("work".to_string()),
                },
            ],
            &[ContactPhone {
                number: "+9876543210".to_string(),
                label: None,
            }],
            Some("Widgets Inc"),
            Some("CEO"),
            Some("Important person"),
        );

        // parse_vcard expects \n line endings, build_vcard uses \r\n
        let unix_vcard = vcard.replace("\r\n", "\n");
        let contact = parse_vcard(&unix_vcard).expect("Should parse built vcard");
        assert_eq!(contact.id, "roundtrip-uid");
        assert_eq!(contact.name, "Alice Smith");
        assert_eq!(contact.emails.len(), 2);
        assert_eq!(contact.emails[0].email, "alice@home.com");
        assert_eq!(contact.emails[0].label, Some("home".to_string()));
        assert_eq!(contact.phones.len(), 1);
        assert_eq!(contact.phones[0].number, "+9876543210");
        assert_eq!(contact.organization, Some("Widgets Inc".to_string()));
        assert_eq!(contact.title, Some("CEO".to_string()));
        assert_eq!(contact.notes, Some("Important person".to_string()));
    }

    #[test]
    fn test_generate_uid_unique() {
        let uid1 = generate_uid();
        let uid2 = generate_uid();
        assert_ne!(uid1, uid2);
        // Should look UUID-ish
        assert_eq!(uid1.matches('-').count(), 4);
    }
}

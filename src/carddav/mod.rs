//! CardDAV client for Fastmail contacts
//!
//! Uses raw HTTP with reqwest since CardDAV is just WebDAV with vCard.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::error::{Error, Result};

const CARDDAV_BASE: &str = "https://carddav.fastmail.com";

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
        let url = format!("{}/dav/addressbooks/user/{}/", CARDDAV_BASE, self.username);

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
        let mut addressbooks = Vec::new();

        // Simple XML parsing - look for response elements with addressbook resourcetype
        for response in xml.split("<d:response>").skip(1) {
            let href = extract_xml_value(response, "d:href").unwrap_or_default();
            let displayname = extract_xml_value(response, "d:displayname");

            // Check if this is an addressbook (has carddav:addressbook resourcetype)
            if response.contains("addressbook") && !href.is_empty() {
                let name = displayname.unwrap_or_else(|| {
                    href.split('/')
                        .rfind(|s| !s.is_empty())
                        .unwrap_or("Unknown")
                        .to_string()
                });

                // Skip the parent collection itself
                if !href.ends_with(&format!("{}/", self.username)) {
                    addressbooks.push(AddressBook { href, name });
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
        let mut contacts = Vec::new();

        for response in xml.split("<d:response>").skip(1) {
            if let Some(vcard_data) = extract_xml_value(response, "card:address-data") {
                // Unescape XML entities
                let vcard_data = vcard_data
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&")
                    .replace("&quot;", "\"");

                if let Some(contact) = parse_vcard(&vcard_data) {
                    contacts.push(contact);
                }
            }
        }

        // Sort by name
        contacts.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

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
}

/// Extract value between XML tags (simple, non-recursive)
fn extract_xml_value(xml: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{}", tag);
    let close_tag = format!("</{}>", tag);

    let start = xml.find(&open_tag)?;
    let after_open = &xml[start..];

    // Find end of opening tag
    let tag_end = after_open.find('>')?;
    let content_start = start + tag_end + 1;

    // Find closing tag
    let close_start = xml[content_start..].find(&close_tag)?;

    Some(
        xml[content_start..content_start + close_start]
            .trim()
            .to_string(),
    )
}

/// Parse a vCard string into a Contact
fn parse_vcard(vcard_str: &str) -> Option<Contact> {
    // Simple manual vCard parsing since the vcard crate API is awkward
    let mut id = String::new();
    let mut name = String::new();
    let mut emails = Vec::new();
    let mut phones = Vec::new();
    let mut organization = None;
    let mut title = None;
    let mut notes = None;

    for line in vcard_str.lines() {
        let line = line.trim();

        if line.starts_with("UID:") {
            id = line.strip_prefix("UID:").unwrap_or("").to_string();
        } else if line.starts_with("FN:") {
            name = line.strip_prefix("FN:").unwrap_or("").to_string();
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
        } else if line.starts_with("ORG:") {
            organization = Some(line.strip_prefix("ORG:").unwrap_or("").to_string());
        } else if line.starts_with("TITLE:") {
            title = Some(line.strip_prefix("TITLE:").unwrap_or("").to_string());
        } else if line.starts_with("NOTE:") {
            notes = Some(line.strip_prefix("NOTE:").unwrap_or("").to_string());
        }
    }

    // Need at least a name
    if name.is_empty() {
        return None;
    }

    // Generate ID if not present
    if id.is_empty() {
        id = format!("{:x}", md5_hash(&name));
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

/// Simple hash for generating IDs
fn md5_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

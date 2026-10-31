//! Attachments on outgoing mail.
//!
//! The server usually runs remotely, so a client cannot name a file on its own
//! disk. Content arrives either inline as base64, or as the ID of a blob already
//! on the server: one put there by `uploadAttachment`, or an attachment of an
//! existing email.

use async_graphql::{InputObject, SimpleObject};
use base64::Engine;

use crate::jmap::{AttachmentContent, AttachmentData, JmapClient};

/// Total size allowed across one message's attachments, and for one upload.
/// Fastmail accepts 50 MB per message, but every byte of an inline attachment
/// passes through a tool argument as base64, a third larger again.
pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

/// A file to attach. Give exactly one of `base64` and `blobId`.
#[derive(InputObject)]
#[graphql(name = "AttachmentInput")]
pub struct AttachmentInput {
    /// File name the recipient sees.
    pub name: String,
    /// MIME type. Inferred from the name's extension when omitted.
    pub content_type: Option<String>,
    /// The file's bytes, base64-encoded.
    pub base64: Option<String>,
    /// A blob already on the server: from `uploadAttachment`, or an existing
    /// email's `Attachment.blobId`. Not re-uploaded.
    pub blob_id: Option<String>,
}

/// A blob put on the server by `uploadAttachment`, ready to attach by its ID.
#[derive(SimpleObject)]
#[graphql(name = "UploadedAttachment")]
pub struct GqlUploadedAttachment {
    pub blob_id: String,
    pub name: String,
    pub content_type: String,
    pub size: u64,
}

/// An attachment with its content resolved and measured.
pub struct Attachment {
    pub data: AttachmentData,
    pub size: usize,
}

/// Decode, measure and check a message's attachments against the size cap.
///
/// A blob is downloaded to learn its size: Fastmail does not offer JMAP's
/// `Blob/get`, and a preview that cannot say how large an attachment is cannot
/// be checked by the person approving it.
///
/// `measure_blobs` is false on CONFIRM and DRAFT: the token pins each blob's ID,
/// blobs are immutable, and PREVIEW already measured them against the cap.
pub async fn resolve(
    client: &JmapClient,
    inputs: Vec<AttachmentInput>,
    measure_blobs: bool,
) -> Result<Vec<Attachment>, String> {
    let mut total = 0;
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        let (name, content_type) = name_and_type(input.name, input.content_type)?;
        let (content, size) = match (input.base64, input.blob_id) {
            (Some(encoded), None) => {
                let bytes = decode(&name, &encoded, MAX_ATTACHMENT_BYTES - total)?;
                let size = bytes.len();
                (AttachmentContent::Bytes(bytes), size)
            }
            (None, Some(blob_id)) => {
                // A JMAP Id, which also keeps it from steering the download URL.
                if blob_id.is_empty()
                    || blob_id.len() > 255
                    || !blob_id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return Err(format!("Attachment '{name}': '{blob_id}' is not a blobId"));
                }
                let size = if measure_blobs {
                    client
                        .blob_size(&blob_id, MAX_ATTACHMENT_BYTES - total)
                        .await
                        .map_err(|e| format!("Attachment '{name}': {e}"))?
                } else {
                    0
                };
                (AttachmentContent::Blob(blob_id), size)
            }
            _ => {
                return Err(format!(
                    "Attachment '{name}': give exactly one of base64 and blobId"
                ));
            }
        };
        total += size;
        if total > MAX_ATTACHMENT_BYTES {
            return Err(over_cap(total));
        }
        out.push(Attachment {
            data: AttachmentData {
                filename: name,
                content_type,
                content,
            },
            size,
        });
    }
    Ok(out)
}

/// Validate an attachment's name and type, inferring the type from the name
/// when none is given. Control characters are refused: both appear in the
/// preview and in the token's fingerprint.
pub fn name_and_type(
    name: String,
    content_type: Option<String>,
) -> Result<(String, String), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Every attachment needs a name".into());
    }
    let content_type = content_type
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| crate::util::mime_from_filename(&name));
    if name
        .chars()
        .chain(content_type.chars())
        .any(char::is_control)
    {
        return Err(format!(
            "Attachment '{}': control characters in the name or type",
            name.escape_debug()
        ));
    }
    Ok((name, content_type))
}

/// Decode base64, refusing before decoding anything that cannot fit in `room`.
pub fn decode(name: &str, encoded: &str, room: usize) -> Result<Vec<u8>, String> {
    // Measured before copying, so an oversized argument is refused without
    // first being duplicated.
    let compact_len = encoded.bytes().filter(|b| !b.is_ascii_whitespace()).count();
    let decoded_len = compact_len / 4 * 3;
    if decoded_len.saturating_sub(2) > room {
        return Err(over_cap(MAX_ATTACHMENT_BYTES - room + decoded_len));
    }
    let compact: std::borrow::Cow<str> = if compact_len == encoded.len() {
        encoded.into()
    } else {
        encoded
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>()
            .into()
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|e| format!("Attachment '{name}' is not valid base64: {e}"))?;
    if bytes.len() > room {
        return Err(over_cap(MAX_ATTACHMENT_BYTES - room + bytes.len()));
    }
    Ok(bytes)
}

fn over_cap(total: usize) -> String {
    format!(
        "Attachments come to {}, over the {} limit for one message. \
         Send the larger files as links instead.",
        human_size(total),
        human_size(MAX_ATTACHMENT_BYTES)
    )
}

/// What a confirmation token vouches for: each attachment's name, type and
/// exact content. A blob's ID pins its content, since JMAP blobs are immutable.
pub fn fingerprint(attachments: &[Attachment]) -> Vec<String> {
    use sha2::{Digest, Sha256};
    attachments
        .iter()
        .map(|a| {
            // SHA-256 rather than the std hasher: the client chooses these
            // bytes, and a 64-bit hash with fixed keys is within reach of a
            // deliberate collision.
            let content = match &a.data.content {
                AttachmentContent::Bytes(bytes) => {
                    format!("bytes:{}", crate::util::hex(&Sha256::digest(bytes)))
                }
                AttachmentContent::Blob(id) => format!("blob:{id}"),
            };
            format!(
                "attachment\n{}\n{}\n{content}",
                a.data.filename, a.data.content_type
            )
        })
        .collect()
}

/// The attachment lines of a send preview, or nothing when there are none.
pub fn preview(attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nAttachments:");
    for a in attachments {
        out.push_str(&format!(
            "\n  {} ({}, {})",
            a.data.filename,
            a.data.content_type,
            human_size(a.size)
        ));
    }
    out
}

pub fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

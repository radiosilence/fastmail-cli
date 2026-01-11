use crate::models::EmailAddress;
use std::path::Path;

pub fn parse_addresses(input: &str) -> Vec<EmailAddress> {
    input
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(start) = s.find('<')
                && let Some(end) = s.find('>')
            {
                let name = s[..start].trim();
                let email = s[start + 1..end].trim();
                return EmailAddress {
                    name: if name.is_empty() {
                        None
                    } else {
                        Some(name.to_string())
                    },
                    email: email.to_string(),
                };
            }
            EmailAddress {
                name: None,
                email: s.to_string(),
            }
        })
        .collect()
}

// ============ Text Extraction ============

/// Extract text from attachment data using kreuzberg
/// Supports: PDF, DOC, DOCX, ODT, XLSX, XLS, ODS, PPTX, PPT, EPUB, RTF,
/// HTML, XML, JSON, YAML, CSV, TSV, TXT, MD, EML, MSG, and more
/// NOTE: Returns None for images - use existing image pipeline instead
pub async fn extract_text(bytes: &[u8], filename: &str) -> anyhow::Result<Option<String>> {
    use kreuzberg::{ExtractionConfig, extract_bytes};

    // Skip images - we have our own pipeline for those (resize + send to Claude)
    if is_image_extension(filename) {
        return Ok(None);
    }

    let mime_type = mime_from_filename(filename);
    let config = ExtractionConfig::default();

    match extract_bytes(bytes, &mime_type, &config).await {
        Ok(result) => {
            let content = result.content.trim();
            if content.is_empty() {
                Ok(None)
            } else {
                Ok(Some(content.to_string()))
            }
        }
        Err(e) => {
            tracing::debug!("kreuzberg extraction failed for {}: {}", filename, e);
            Ok(None)
        }
    }
}

/// Synchronous version for non-async contexts
/// NOTE: Returns None for images - use existing image pipeline instead
pub fn extract_text_sync(bytes: &[u8], filename: &str) -> anyhow::Result<Option<String>> {
    use kreuzberg::{ExtractionConfig, extract_bytes_sync};

    // Skip images - we have our own pipeline for those (resize + send to Claude)
    if is_image_extension(filename) {
        return Ok(None);
    }

    let mime_type = mime_from_filename(filename);
    let config = ExtractionConfig::default();

    match extract_bytes_sync(bytes, &mime_type, &config) {
        Ok(result) => {
            let content = result.content.trim();
            if content.is_empty() {
                Ok(None)
            } else {
                Ok(Some(content.to_string()))
            }
        }
        Err(e) => {
            tracing::debug!("kreuzberg extraction failed for {}: {}", filename, e);
            Ok(None)
        }
    }
}

/// Check if filename has an image extension (used to skip kreuzberg for images)
fn is_image_extension(filename: &str) -> bool {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" | "svg" | "heic"
    )
}

/// Infer MIME type from filename extension for documents
fn mime_from_filename(filename: &str) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        // Documents
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "odt" => "application/vnd.oasis.opendocument.text",
        "rtf" => "application/rtf",
        // Spreadsheets
        "xls" | "xla" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => "application/vnd.ms-excel.sheet.macroEnabled.12",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        "xlam" => "application/vnd.ms-excel.addin.macroEnabled.12",
        "xltm" => "application/vnd.ms-excel.template.macroEnabled.12",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        // Presentations
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ppsx" => "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
        // eBooks
        "epub" => "application/epub+zip",
        "fb2" => "application/x-fictionbook+xml",
        // Text & markup
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" | "xhtml" => "text/html",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "rst" => "text/x-rst",
        "org" => "text/x-org",
        // Email
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        // Archives
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "tgz" | "gz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        // Scientific & academic
        "bib" | "biblatex" => "application/x-bibtex",
        "ris" => "application/x-research-info-systems",
        "enw" => "application/x-endnote-refer",
        "csl" => "application/vnd.citationstyles.style+xml",
        "tex" | "latex" => "application/x-tex",
        "typst" => "application/x-typst",
        "jats" => "application/jats+xml",
        "ipynb" => "application/x-ipynb+json",
        "docbook" => "application/docbook+xml",
        // Documentation
        "opml" => "text/x-opml",
        "pod" => "text/x-pod",
        "mdoc" => "text/troff",
        "troff" => "text/troff",
        // Default - let kreuzberg figure it out
        _ => "application/octet-stream",
    }
    .to_string()
}

// ============ Image Processing ============

/// Parse a human-readable size string like "500K", "1M", "1.5MB" into bytes
pub fn parse_size(s: &str) -> Option<usize> {
    let s = s.trim().to_uppercase();
    let s = s.trim_end_matches('B'); // "1MB" -> "1M"

    if let Some(num_str) = s.strip_suffix('K') {
        num_str.parse::<f64>().ok().map(|n| (n * 1024.0) as usize)
    } else if let Some(num_str) = s.strip_suffix('M') {
        num_str
            .parse::<f64>()
            .ok()
            .map(|n| (n * 1024.0 * 1024.0) as usize)
    } else if let Some(num_str) = s.strip_suffix('G') {
        num_str
            .parse::<f64>()
            .ok()
            .map(|n| (n * 1024.0 * 1024.0 * 1024.0) as usize)
    } else {
        s.parse::<usize>().ok()
    }
}

/// Check if content is an image based on MIME type or file extension
pub fn is_image(content_type: &str, filename: &str) -> bool {
    if content_type.starts_with("image/") {
        return true;
    }
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff"
    )
}

/// Infer MIME type from filename extension (JMAP often returns application/octet-stream)
pub fn infer_image_mime(filename: &str) -> Option<&'static str> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())?
        .to_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tiff" | "tif" => Some("image/tiff"),
        _ => None,
    }
}

/// Default max size for MCP (Claude's ~1MB base64 limit means raw < 700KB)
pub const MCP_IMAGE_MAX_BYTES: usize = 700 * 1024;

/// Resize image if needed to stay under a size limit
/// Returns (processed_bytes, mime_type)
pub fn resize_image(
    data: &[u8],
    content_type: &str,
    max_bytes: usize,
) -> Result<(Vec<u8>, String), String> {
    use image::ImageFormat;
    use std::io::Cursor;

    // If already small enough, return as-is
    if data.len() <= max_bytes {
        return Ok((data.to_vec(), content_type.to_string()));
    }

    // Determine format
    let format = match content_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => return Err(format!("Unsupported image format: {}", content_type)),
    };

    // Load image
    let img = image::load_from_memory_with_format(data, format)
        .map_err(|e| format!("Failed to load image: {}", e))?;

    // Resize to fit - scale down proportionally
    let (width, height) = (img.width(), img.height());
    let scale = (max_bytes as f64 / data.len() as f64).sqrt();
    let new_width = ((width as f64 * scale) as u32).max(1);
    let new_height = ((height as f64 * scale) as u32).max(1);

    let resized = img.resize(new_width, new_height, image::imageops::FilterType::Lanczos3);

    // Encode as JPEG for better compression
    let mut output = Vec::new();
    resized
        .write_to(&mut Cursor::new(&mut output), ImageFormat::Jpeg)
        .map_err(|e| format!("Failed to encode image: {}", e))?;

    Ok((output, "image/jpeg".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_email() {
        let result = parse_addresses("test@example.com");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "test@example.com");
        assert!(result[0].name.is_none());
    }

    #[test]
    fn test_parse_multiple_emails() {
        let result = parse_addresses("a@example.com, b@example.com");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "a@example.com");
        assert_eq!(result[1].email, "b@example.com");
    }

    #[test]
    fn test_parse_email_with_name() {
        let result = parse_addresses("John Doe <john@example.com>");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "john@example.com");
        assert_eq!(result[0].name, Some("John Doe".to_string()));
    }

    #[test]
    fn test_parse_mixed_formats() {
        let result = parse_addresses("plain@example.com, Named User <named@example.com>");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "plain@example.com");
        assert!(result[0].name.is_none());
        assert_eq!(result[1].email, "named@example.com");
        assert_eq!(result[1].name, Some("Named User".to_string()));
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_addresses("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_whitespace_handling() {
        let result = parse_addresses("  spaced@example.com  ,  other@example.com  ");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "spaced@example.com");
        assert_eq!(result[1].email, "other@example.com");
    }

    #[test]
    fn test_parse_angle_brackets_no_name() {
        let result = parse_addresses("<bare@example.com>");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "bare@example.com");
        assert!(result[0].name.is_none());
    }
}

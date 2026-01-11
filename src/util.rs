use crate::models::EmailAddress;
use std::path::Path;
use std::process::Command;

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

/// Extract text from attachment data based on content type and filename
pub fn extract_text(
    bytes: &[u8],
    content_type: &str,
    filename: &str,
) -> anyhow::Result<Option<String>> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Plain text
    if content_type.starts_with("text/") || ext == "txt" || ext == "md" || ext == "csv" {
        return Ok(Some(String::from_utf8_lossy(bytes).to_string()));
    }

    // PDF - use pdf-extract (pure Rust)
    if content_type == "application/pdf" || ext == "pdf" {
        return Ok(pdf_extract::extract_text_from_mem(bytes).ok());
    }

    // DOCX - use docx-lite (pure Rust)
    if content_type == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        || ext == "docx"
    {
        let temp_path =
            std::env::temp_dir().join(format!("fastmail-cli-{}.docx", std::process::id()));
        std::fs::write(&temp_path, bytes)?;
        let result = docx_lite::extract_text(&temp_path).ok();
        let _ = std::fs::remove_file(&temp_path);
        return Ok(result);
    }

    // DOC (old format) - try textutil (macOS), antiword, or catdoc
    if content_type == "application/msword" || ext == "doc" {
        let temp_path =
            std::env::temp_dir().join(format!("fastmail-cli-{}.doc", std::process::id()));
        std::fs::write(&temp_path, bytes)?;
        let result = extract_with_textutil(&temp_path)
            .or_else(|_| extract_with_command(&temp_path, "antiword", &[]))
            .or_else(|_| extract_with_command(&temp_path, "catdoc", &[]));
        let _ = std::fs::remove_file(&temp_path);
        return result;
    }

    // RTF - use pandoc
    if content_type == "application/rtf" || ext == "rtf" {
        let temp_path =
            std::env::temp_dir().join(format!("fastmail-cli-{}.rtf", std::process::id()));
        std::fs::write(&temp_path, bytes)?;
        let result = extract_with_command(&temp_path, "pandoc", &["-t", "plain"]);
        let _ = std::fs::remove_file(&temp_path);
        return result;
    }

    // Unknown format
    Ok(None)
}

fn extract_with_command(
    path: &Path,
    cmd: &str,
    extra_args: &[&str],
) -> anyhow::Result<Option<String>> {
    let mut args: Vec<&str> = vec![path.to_str().unwrap_or("")];
    args.extend(extra_args);

    let output = Command::new(cmd)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(Some(String::from_utf8_lossy(&o.stdout).to_string())),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

/// Use macOS textutil to convert doc to txt
fn extract_with_textutil(path: &Path) -> anyhow::Result<Option<String>> {
    let output = Command::new("textutil")
        .args(["-convert", "txt", "-stdout"])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(Some(String::from_utf8_lossy(&o.stdout).to_string())),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
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

use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;
use std::path::Path;
use std::process::Command;

pub async fn download_attachment(
    email_id: &str,
    output_dir: Option<&str>,
    format: Option<&str>,
) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let email = client.get_email(email_id).await?;

    let attachments = email.attachments.as_ref();
    if attachments.is_none() || attachments.unwrap().is_empty() {
        Output::<()>::error("No attachments found").print();
        return Ok(());
    }

    // JSON format - extract text and return structured data
    if format == Some("json") {
        let mut results: Vec<AttachmentContent> = Vec::new();

        for attachment in attachments.unwrap() {
            let blob_id = match &attachment.blob_id {
                Some(id) => id,
                None => continue,
            };

            let filename = attachment
                .name
                .clone()
                .unwrap_or_else(|| format!("{}.bin", blob_id));

            let content_type = attachment.content_type.clone().unwrap_or_default();
            let bytes = client.download_blob(blob_id).await?;

            let text = extract_text(&bytes, &content_type, &filename)?;

            results.push(AttachmentContent {
                filename,
                content_type,
                size: bytes.len(),
                text,
            });
        }

        Output::success(results).print();
        return Ok(());
    }

    // Default: download to files
    let out_dir = output_dir.unwrap_or(".");
    let mut downloaded: Vec<String> = Vec::new();

    for attachment in attachments.unwrap() {
        let blob_id = match &attachment.blob_id {
            Some(id) => id,
            None => continue,
        };

        let filename = attachment
            .name
            .clone()
            .unwrap_or_else(|| format!("{}.bin", blob_id));

        let bytes = client.download_blob(blob_id).await?;

        let path = Path::new(out_dir).join(&filename);
        std::fs::write(&path, &bytes)?;

        downloaded.push(path.to_string_lossy().to_string());
    }

    #[derive(serde::Serialize)]
    struct DownloadResponse {
        files: Vec<String>,
    }

    Output::success(DownloadResponse { files: downloaded }).print();

    Ok(())
}

#[derive(serde::Serialize)]
struct AttachmentContent {
    filename: String,
    content_type: String,
    size: usize,
    text: Option<String>,
}

fn extract_text(
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
        // Try textutil (macOS) first, then antiword, then catdoc
        let result = extract_with_textutil(&temp_path)
            .or_else(|_| extract_with_command_file(&temp_path, "antiword", &[]))
            .or_else(|_| extract_with_command_file(&temp_path, "catdoc", &[]));
        let _ = std::fs::remove_file(&temp_path);
        return result;
    }

    // RTF - use unrtf or pandoc
    if content_type == "application/rtf" || ext == "rtf" {
        let temp_path =
            std::env::temp_dir().join(format!("fastmail-cli-{}.rtf", std::process::id()));
        std::fs::write(&temp_path, bytes)?;
        let result = extract_with_command_file(&temp_path, "pandoc", &["-t", "plain"]);
        let _ = std::fs::remove_file(&temp_path);
        return result;
    }

    // Images - no OCR support currently, return None
    // Unknown format
    Ok(None)
}

fn extract_with_command_file(
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
        Err(_) => Ok(None), // Command not available
    }
}

/// Use macOS textutil to convert doc to txt
fn extract_with_textutil(path: &Path) -> anyhow::Result<Option<String>> {
    let output_path = path.with_extension("txt");

    let output = Command::new("textutil")
        .args(["-convert", "txt", "-stdout"])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    let _ = std::fs::remove_file(&output_path); // cleanup if textutil wrote a file

    match output {
        Ok(o) if o.status.success() => Ok(Some(String::from_utf8_lossy(&o.stdout).to_string())),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

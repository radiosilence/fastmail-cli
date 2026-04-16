use crate::jmap::authenticated_client;
use crate::models::Output;
use crate::util::{
    extract_text, infer_image_mime, is_image, parse_size, resize_image, sanitize_filename,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

pub async fn download_attachment(
    email_id: &str,
    output_dir: Option<&str>,
    format: Option<&str>,
    max_size: Option<&str>,
) -> anyhow::Result<()> {
    let max_bytes = max_size.and_then(parse_size);
    let client = authenticated_client().await?;

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

            let fallback = format!("{}.bin", blob_id);
            let raw_name = attachment.name.as_deref().unwrap_or("");
            let filename = sanitize_filename(raw_name, &fallback);

            let content_type = attachment.content_type.clone().unwrap_or_default();
            let bytes = client.download_blob(blob_id).await?;

            let text = extract_text(&bytes, &filename).await?;

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

        let fallback = format!("{}.bin", blob_id);
        let raw_name = attachment.name.as_deref().unwrap_or("");
        let filename = sanitize_filename(raw_name, &fallback);

        let content_type = attachment
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");

        let bytes = client.download_blob(blob_id).await?;

        // Resize images if --max-size specified
        let (final_bytes, final_filename) = if let Some(max) = max_bytes {
            let mime = if is_image(content_type, &filename) {
                infer_image_mime(&filename).unwrap_or(content_type)
            } else {
                content_type
            };

            if is_image(mime, &filename) {
                match resize_image(&bytes, mime, max) {
                    Ok((resized, new_mime)) => {
                        // Update extension if format changed (e.g., PNG -> JPEG)
                        let new_filename = if new_mime == "image/jpeg"
                            && !filename.to_lowercase().ends_with(".jpg")
                            && !filename.to_lowercase().ends_with(".jpeg")
                        {
                            let stem = Path::new(&filename)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or(&filename);
                            format!("{}.jpg", stem)
                        } else {
                            filename.clone()
                        };
                        (resized, new_filename)
                    }
                    Err(_) => (bytes, filename.clone()),
                }
            } else {
                (bytes, filename.clone())
            }
        } else {
            (bytes, filename.clone())
        };

        let path = Path::new(out_dir).join(&final_filename);
        // create_new(true) uses O_EXCL/CREATE_NEW — fails if the target exists,
        // including through a symlink. Prevents silent overwrite and TOCTOU
        // attacks where an attacker pre-creates a symlink at the target path.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| {
                anyhow::anyhow!("Failed to write attachment to {}: {}", path.display(), e)
            })?;
        file.write_all(&final_bytes)?;

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

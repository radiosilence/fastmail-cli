use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;
use crate::util::{extract_text, infer_image_mime, is_image, parse_size, resize_image};
use std::path::Path;

pub async fn download_attachment(
    email_id: &str,
    output_dir: Option<&str>,
    format: Option<&str>,
    max_size: Option<&str>,
) -> anyhow::Result<()> {
    let max_bytes = max_size.and_then(parse_size);
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
        std::fs::write(&path, &final_bytes)?;

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

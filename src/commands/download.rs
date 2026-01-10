use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;
use std::path::Path;

pub async fn download_attachment(email_id: &str, output_dir: Option<&str>) -> anyhow::Result<()> {
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

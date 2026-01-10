use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::{EmailAddress, Output};

fn parse_addresses(input: &str) -> Vec<EmailAddress> {
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

pub async fn send(
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
    reply_to: Option<&str>,
) -> anyhow::Result<()> {
    let config = Config::load()?;
    let token = config.get_token()?;

    let mut client = JmapClient::new(token.to_string());
    client.authenticate().await?;

    let to_addrs = parse_addresses(to);
    let cc_addrs = cc.map(parse_addresses).unwrap_or_default();
    let bcc_addrs = bcc.map(parse_addresses).unwrap_or_default();

    let email_id = client
        .send_email(to_addrs, cc_addrs, bcc_addrs, subject, body, reply_to)
        .await?;

    #[derive(serde::Serialize)]
    struct SendResponse {
        email_id: String,
    }

    Output::success(SendResponse { email_id }).print();

    Ok(())
}

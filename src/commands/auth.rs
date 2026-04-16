use crate::config::Config;
use crate::jmap::JmapClient;
use crate::models::Output;
use std::io::{self, BufRead, IsTerminal, Write};

pub async fn auth(token: &str) -> anyhow::Result<()> {
    let mut client = JmapClient::new(token.to_string());
    let session = client.authenticate().await?;

    let mut config = Config::load()?;
    config.set_token(token.to_string());
    config.save()?;

    Output::<()>::success_msg(format!("Authenticated as {}", session.username)).print();

    Ok(())
}

/// Read a token from stdin — used when the user runs `auth` without a positional arg.
/// Keeping the token off the command line avoids exposing it in `ps`, shell history,
/// and the process environment visible to other local users.
pub fn read_token_from_stdin() -> anyhow::Result<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        eprint!("Paste your Fastmail API token and press Enter: ");
        io::stderr().flush().ok();
    }
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let token = line.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("No token provided on stdin");
    }
    Ok(token)
}

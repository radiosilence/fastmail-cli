//! MCP (Model Context Protocol) server for Fastmail
//!
//! Provides CLI usage instructions to Claude and other LLMs via MCP,
//! allowing them to use `fastmail-cli` directly through bash commands.

use rmcp::{
    ServerHandler,
    model::{Implementation, ServerCapabilities, ServerInfo},
};

#[derive(Clone)]
pub struct FastmailMcp;

const CLI_INSTRUCTIONS: &str = r#"You have access to `fastmail-cli`, a CLI for Fastmail's JMAP API. All commands output JSON.

## Quick Reference

### Reading
```
fastmail-cli list mailboxes                           # folders + unread counts
fastmail-cli list emails [--mailbox NAME] [--limit N] # list emails (default: INBOX, 50)
fastmail-cli list identities                          # sender addresses (for --from)
fastmail-cli get EMAIL_ID                             # full email + thread context
fastmail-cli thread EMAIL_ID                          # all emails in conversation
fastmail-cli search [filters] [--limit N]             # search (default: 50)
```

### Search Filters (all ANDed)
`--text` (full-text), `--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`, `--mailbox`, `--has-attachment`, `--min-size`, `--max-size`, `--before YYYY-MM-DD`, `--after YYYY-MM-DD`, `--unread`, `--flagged`

### Sending
```
fastmail-cli send --to ADDRS --subject TEXT --body TEXT [--cc ADDRS] [--bcc ADDRS] [--from IDENTITY]
fastmail-cli reply EMAIL_ID --body TEXT [--all] [--cc ADDRS] [--bcc ADDRS] [--from IDENTITY]
fastmail-cli forward EMAIL_ID --to ADDRS [--body TEXT] [--cc ADDRS] [--bcc ADDRS] [--from IDENTITY]
```
Multiple recipients: comma-separated. Use `list identities` to find valid `--from` values.

### Actions
```
fastmail-cli move EMAIL_ID --to MAILBOX    # Archive, Trash, etc.
fastmail-cli mark-read EMAIL_ID [--unread] # mark read/unread
fastmail-cli spam EMAIL_ID -y              # mark spam + train filter (DESTRUCTIVE - always confirm with user first!)
```

### Attachments
```
fastmail-cli download EMAIL_ID [--output DIR] [--format json] [--max-size 500K]
```
`--format json` extracts text from PDFs, DOCX, etc. (56 formats). `--max-size` resizes images.

### Contacts (requires FASTMAIL_APP_PASSWORD, not API token)
```
fastmail-cli contacts list
fastmail-cli contacts search QUERY         # search by name/email/org
```

### Masked Email
```
fastmail-cli masked list
fastmail-cli masked create [--domain URL] [--description TEXT] [--prefix PREFIX]
fastmail-cli masked enable ID
fastmail-cli masked disable ID
fastmail-cli masked delete ID -y           # permanent! confirm with user first
```

## Safety Rules
- NEVER send/reply/forward without showing the user what will be sent and getting explicit approval
- NEVER mark as spam without user confirmation (trains the filter - affects future mail)
- NEVER delete masked emails without user confirmation (permanent)
- When composing, show the user: recipients, subject, body before running the send command
- Parse JSON output to extract email IDs, thread info, etc. for follow-up commands

## Output Format
All commands return: `{"success": bool, "data": ..., "message": "...", "error": "..."}`
Use `jq` for extraction, e.g.: `fastmail-cli list mailboxes | jq '.data[]'`

## Common Patterns
1. Check inbox: `fastmail-cli list emails --mailbox INBOX --limit 10`
2. Read email: `fastmail-cli get EMAIL_ID` (includes full thread)
3. Find emails: `fastmail-cli search --from "someone" --after 2024-01-01`
4. Reply flow: read email → compose reply → show user → `fastmail-cli reply EMAIL_ID --body "..."`
5. Attachment text: `fastmail-cli download EMAIL_ID --format json`
"#;

impl ServerHandler for FastmailMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().build(),
            server_info: Implementation {
                name: "fastmail-cli".to_string(),
                title: Some("Fastmail CLI Assistant".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                icons: None,
                website_url: Some("https://github.com/radiosilence/fastmail-cli".to_string()),
            },
            instructions: Some(CLI_INSTRUCTIONS.to_string()),
        }
    }
}

/// Run the MCP server with stdio transport
pub async fn run_server() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let service = FastmailMcp;
    let server = service
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    server
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;

    Ok(())
}

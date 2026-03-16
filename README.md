# fastmail-cli

CLI for Fastmail's JMAP API. Read, search, send, and manage emails from your terminal or AI assistant.

## Features

| Feature               | Description                                                            |
| --------------------- | ---------------------------------------------------------------------- |
| **Email**             | List, search, read, send, reply, forward, threads, identity selection  |
| **Mailboxes**         | List folders, move emails, mark spam/read                              |
| **Contacts**          | Search contacts via CardDAV                                            |
| **Attachments**       | Download files, extract text, resize images                            |
| **Text Extraction**   | 56 formats via [kreuzberg](https://github.com/kreuzberg-dev/kreuzberg) |
| **Image Resizing**    | `--max-size` to resize images on download                              |
| **Masked Email**      | Create, list, enable/disable aliases                                   |
| **MCP Server**        | Claude integration via CLI instructions (no tool overhead)             |
| **Shell Completions** | Bash, Zsh, Fish, PowerShell                                            |
| **JSON Output**       | All commands output JSON for scripting                                 |

## Quick Start

### Installation

#### From GitHub Releases (recommended for mise)

```bash
# Add to mise config
mise use -g github:radiosilence/fastmail-cli
```

#### From Source

```bash
cargo install --git https://github.com/radiosilence/fastmail-cli
```

### Authentication

1. Generate an API token at [Fastmail Settings > Privacy & Security > Integrations > API tokens](https://app.fastmail.com/settings/security/tokens)
2. Auth with the CLI:

```bash
fastmail-cli auth YOUR_TOKEN
```

Token is stored in `~/.config/fastmail-cli/config.toml` with 0600 permissions.

### Configuration

Credentials can be set via environment variables or config file. Env vars take precedence.

**Environment variables:**

```bash
export FASTMAIL_API_TOKEN="fmu1-..."      # Required for JMAP (email)
export FASTMAIL_USERNAME="you@fastmail.com"  # Required for CardDAV (contacts)
export FASTMAIL_APP_PASSWORD="xxxx..."    # Required for CardDAV (contacts)
```

**Config file** (`~/.config/fastmail-cli/config.toml`):

```toml
[core]
api_token = "fmu1-..."

[contacts]
username = "you@fastmail.com"
app_password = "xxxx..."
```

The `auth` command only sets `[core].api_token`. For contacts, add `[contacts]` section manually or use env vars.

## Usage

All output is JSON for easy scripting with `jq`.

### List Mailboxes

```bash
fastmail-cli list mailboxes
```

### List Emails

```bash
# Default: INBOX, 50 emails
fastmail-cli list emails

# Specific mailbox and limit
fastmail-cli list emails --mailbox Sent --limit 10
```

### Get Email Details

```bash
fastmail-cli get EMAIL_ID
```

### Search

Search uses JMAP filter flags (all filters are ANDed together):

```bash
# Full-text search
fastmail-cli search --text "meeting notes"

# Filter by header fields
fastmail-cli search --from "alice@example.com"
fastmail-cli search --to "bob" --subject "project"

# Filter by mailbox
fastmail-cli search --mailbox Sent --limit 10

# Attachments and size
fastmail-cli search --has-attachment
fastmail-cli search --min-size 1000000  # > 1MB

# Date range (ISO 8601)
fastmail-cli search --after 2024-01-01 --before 2024-12-31

# Status filters
fastmail-cli search --unread
fastmail-cli search --flagged

# Combine filters
fastmail-cli search --from "boss" --has-attachment --after 2024-06-01 --limit 20
```

Available flags: `--text`, `--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`, `--mailbox`, `--has-attachment`, `--min-size`, `--max-size`, `--before`, `--after`, `--unread`, `--flagged`

### List Identities

View available sender identities (useful for `--from`):

```bash
fastmail-cli list identities
```

### Send Email

```bash
fastmail-cli send \
  --to "alice@example.com, bob@example.com" \
  --subject "Hello" \
  --body "Message body here"

# With CC/BCC
fastmail-cli send \
  --to "alice@example.com" \
  --cc "bob@example.com" \
  --bcc "secret@example.com" \
  --subject "Hello" \
  --body "Message"

# Send from a specific identity/alias
fastmail-cli send \
  --to "alice@example.com" \
  --from "alias@yourdomain.com" \
  --subject "Hello" \
  --body "Message"
```

### Move Email

```bash
fastmail-cli move EMAIL_ID --to Archive
fastmail-cli move EMAIL_ID --to Trash
```

### Mark as Spam

```bash
# Requires confirmation
fastmail-cli spam EMAIL_ID

# Skip confirmation
fastmail-cli spam EMAIL_ID -y
```

### Mark as Read/Unread

```bash
# Mark as read
fastmail-cli mark-read EMAIL_ID

# Mark as unread
fastmail-cli mark-read EMAIL_ID --unread
```

### Download Attachments

```bash
# Download to current directory
fastmail-cli download EMAIL_ID

# Download to specific directory
fastmail-cli download EMAIL_ID --output ~/Downloads

# Extract text content as JSON (PDF, DOCX, DOC, TXT)
fastmail-cli download EMAIL_ID --format json

# Resize images to max 500KB
fastmail-cli download EMAIL_ID --max-size 500K
```

Text extraction uses [kreuzberg](https://github.com/kreuzberg-dev/kreuzberg) and supports 56 formats:

- **Documents**: PDF, DOC, DOCX, ODT, RTF
- **Spreadsheets**: XLS, XLSX, ODS, CSV, TSV
- **Presentations**: PPT, PPTX
- **eBooks**: EPUB, FB2
- **Markup**: HTML, XML, Markdown, RST, Org
- **Data**: JSON, YAML, TOML
- **Email**: EML, MSG
- **Archives**: ZIP, TAR, GZ, 7z
- **Academic**: BibTeX, LaTeX, Typst, Jupyter notebooks

### Reply to Email

```bash
# Reply to sender only
fastmail-cli reply EMAIL_ID --body "Thanks for your message"

# Reply all
fastmail-cli reply EMAIL_ID --body "Thanks everyone" --all

# Reply with additional CC/BCC
fastmail-cli reply EMAIL_ID --body "Response" --cc "boss@example.com"

# Reply from a specific identity
fastmail-cli reply EMAIL_ID --body "Thanks" --from "alias@yourdomain.com"
```

### Forward Email

```bash
fastmail-cli forward EMAIL_ID \
  --to "colleague@example.com" \
  --body "FYI - see below"

# Forward from a specific identity
fastmail-cli forward EMAIL_ID \
  --to "colleague@example.com" \
  --from "alias@yourdomain.com" \
  --body "FYI"
```

### Shell Completions

```bash
# Bash
fastmail-cli completions bash >> ~/.bashrc

# Zsh
fastmail-cli completions zsh >> ~/.zshrc

# Fish
fastmail-cli completions fish > ~/.config/fish/completions/fastmail-cli.fish
```

### Contacts

Search your Fastmail contacts via CardDAV. Requires an app password (API tokens don't work for CardDAV).

```bash
# Set credentials
export FASTMAIL_USERNAME="you@fastmail.com"
export FASTMAIL_APP_PASSWORD="your-app-password"

# List all contacts
fastmail-cli contacts list

# Search by name, email, or organization
fastmail-cli contacts search "alice"
```

Generate an app password at [Fastmail Settings > Privacy & Security > Integrations > App passwords](https://app.fastmail.com/settings/security/devicekeys).

### Masked Email

Create disposable email addresses for signups. Requires Fastmail's masked email feature.

```bash
# List all masked emails
fastmail-cli masked list

# Create a new masked email
fastmail-cli masked create --domain "https://example.com" --description "Example Site"

# Create with custom prefix
fastmail-cli masked create --prefix "shopping" --description "Shopping sites"

# Enable/disable a masked email
fastmail-cli masked enable MASKED_EMAIL_ID
fastmail-cli masked disable MASKED_EMAIL_ID

# Delete (requires confirmation)
fastmail-cli masked delete MASKED_EMAIL_ID -y
```

## Output Format

All commands output JSON with this structure:

```json
{
  "success": true,
  "data": { ... },
  "message": "optional status message",
  "error": "error message if success=false"
}
```

### Parsing with jq

```bash
# Get unread count for INBOX
fastmail-cli list mailboxes | jq '.data[] | select(.role == "inbox") | .unreadEmails'

# List email subjects
fastmail-cli list emails | jq '.data.emails[].subject'

# Get email body
fastmail-cli get EMAIL_ID | jq -r '.data.bodyValues | to_entries[0].value.value'
```

## MCP Server (Claude Integration)

The MCP server provides Claude with instructions for using the CLI directly via bash — no MCP tools, no schema overhead. Claude runs `fastmail-cli` commands and parses JSON output.

```bash
fastmail-cli mcp
```

Configure in Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "fastmail": {
      "command": "mise",
      "args": ["x", "--", "fastmail-cli", "mcp"],
      "env": {
        "FASTMAIL_API_TOKEN": "your-token-here",
        "FASTMAIL_USERNAME": "you@fastmail.com",
        "FASTMAIL_APP_PASSWORD": "your-app-password"
      }
    }
  }
}
```

Username and app password are optional — only needed for contact search (CardDAV requires app password, API tokens don't work).

The MCP server injects a condensed CLI reference that teaches Claude how to list/search/read/send/reply/forward emails, manage attachments, contacts, and masked emails — all through the CLI's JSON interface. This approach is lightweight (no tool schemas to inflate context) and lets Claude use the full power of the CLI including `jq` for parsing.

## Debug Logging

Enable debug output with `RUST_LOG`:

```bash
RUST_LOG=debug fastmail-cli list mailboxes
```

## JMAP API

This CLI uses Fastmail's JMAP implementation. Capabilities are filtered dynamically based on your API token's permissions — read-only tokens work fine for listing/reading, while send and masked email operations require appropriate capabilities.

For more on JMAP: [jmap.io](https://jmap.io/)

## License

MIT

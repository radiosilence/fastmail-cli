# fastmail-cli

CLI for Fastmail's JMAP API. Read, search, send, and manage emails from your terminal.

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

Token is stored in `~/.fastmail-cli/config.json` with 0600 permissions.

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

```bash
fastmail-cli search "from:alice subject:meeting"
fastmail-cli search "has:attachment" --limit 20
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

## Debug Logging

Enable debug output with `RUST_LOG`:

```bash
RUST_LOG=debug fastmail-cli list mailboxes
```

## JMAP API

This CLI uses Fastmail's JMAP implementation. Key capabilities:
- `urn:ietf:params:jmap:core`
- `urn:ietf:params:jmap:mail`
- `urn:ietf:params:jmap:submission`

For more on JMAP: [jmap.io](https://jmap.io/)

## License

MIT

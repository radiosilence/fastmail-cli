---
name: fastmail
description: Complete reference for fastmail-cli — all commands, flags, config, and common patterns
---

# fastmail-cli — Complete Reference

fastmail-cli is a Rust CLI for Fastmail via JMAP (email) and CardDAV (contacts). All output is JSON: `{"success": true, "data": {...}}`.

## Setup

```bash
fastmail auth fmu1-YOUR-TOKEN
```

Config lives at `~/.config/fastmail-cli/config.toml`:
```toml
[core]
api_token = "fmu1-..."

[contacts]
username = "you@fastmail.com"
app_password = "xxxx..."
```

Or via env: `FASTMAIL_API_TOKEN`, `FASTMAIL_USERNAME`, `FASTMAIL_APP_PASSWORD`

Debug: `RUST_LOG=debug fastmail [cmd]`

---

## Command Reference

### List

```bash
fastmail list emails [-m MAILBOX] [-l LIMIT]     # default: INBOX, 50
fastmail list mailboxes
fastmail list identities                          # sender aliases for --from
```

### Get & Thread

```bash
fastmail get EMAIL_ID                            # full email with body
fastmail thread EMAIL_ID                         # entire conversation
```

### Search

```bash
fastmail search [OPTIONS]
  --text/-t STR       # full-text (from/to/subject/body)
  --from/--to/--cc/--bcc/--subject/--body STR
  --mailbox/-m STR
  --before/--after STR  # ISO 8601: 2024-01-15
  --unread --flagged --has-attachment
  --min-size/--max-size BYTES
  --limit/-l N        # default 50
```

### Compose

```bash
fastmail send --to ADDR --subject SUBJ --body BODY [--cc] [--bcc] [--from IDENTITY] [--draft]
fastmail reply EMAIL_ID --body BODY [--all] [--cc] [--bcc] [--from IDENTITY] [--draft]
fastmail forward EMAIL_ID --to ADDR [--body STR] [--cc] [--bcc] [--from IDENTITY] [--draft]
```

### Manage

```bash
fastmail move EMAIL_ID --to MAILBOX
fastmail mark-read EMAIL_ID [--unread]
fastmail spam EMAIL_ID [-y]
```

### Attachments

```bash
fastmail download EMAIL_ID [-o OUTPUT_DIR] [-f raw|json] [--max-size 1M]
```

### Masked Email

```bash
fastmail masked list
fastmail masked create [--domain URL] [--description STR] [--prefix STR]
fastmail masked enable/disable/delete ID [-y]
```

### Contacts

```bash
fastmail contacts list
fastmail contacts search QUERY    # name, email, or org
```

### Other

```bash
fastmail completions bash|zsh|fish|powershell
fastmail mcp    # start MCP server for Claude Desktop
```

---

## Common Patterns

```bash
# Find unread emails from a sender
fastmail search --from boss@company.com --unread

# Get a thread then reply
fastmail thread abc123
fastmail reply abc123 --body "Thanks, will do." --from work@me.com

# Save draft instead of sending
fastmail send --to x@y.com --subject "Draft" --body "..." --draft

# Download all attachments from an email
fastmail download abc123 -o ~/Downloads

# Move to folder after reading
fastmail move abc123 --to "Archive"
```

---

## Subcommand Skills

- `/fastmail/search` — search workflows and filter combinations
- `/fastmail/compose` — send, reply, forward, drafts, identities
- `/fastmail/conversations` — threading, listing, reading
- `/fastmail/attachments` — downloading and extracting attachments
- `/fastmail/masked` — masked email management
- `/fastmail/contacts` — contact search

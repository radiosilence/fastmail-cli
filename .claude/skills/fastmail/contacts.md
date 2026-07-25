---
name: fastmail/contacts
description: fastmail contacts — CardDAV setup, list and search contacts
---

# fastmail-cli — Contacts

Contacts use CardDAV and require separate credentials from the JMAP API token.

## Configuration

```toml
# ~/.config/fastmail-cli/config.toml
[contacts]
username = "you@fastmail.com"
app_password = "your-app-password"
```

Or via env:
```bash
FASTMAIL_USERNAME="you@fastmail.com"
FASTMAIL_APP_PASSWORD="your-app-password"
```

Generate an app password at: Fastmail Settings → Privacy & Security → App Passwords

## Commands

```bash
# List all contacts
fastmail contacts list

# Search by name, email, or organization
fastmail contacts search "Alice"
fastmail contacts search "acme.com"
fastmail contacts search "ACME Corp"
```

## Typical Patterns

```bash
# Find email address before composing
fastmail contacts search "Bob Smith" | jq '.data[0].emails[0].value'

# Verify who someone is before replying
fastmail contacts search "bob@unknown.com"

# Find all contacts at a company
fastmail contacts search "bigcorp.com"
```

## Notes

- `contacts list` returns all contacts — can be large. Prefer `contacts search` for targeted lookups.
- Contact data includes name, emails, phone numbers, organization, and notes where available.
- Read-only via CLI — create/edit contacts through Fastmail web or a CardDAV client.

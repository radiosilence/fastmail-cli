---
name: fastmail/compose
description: fastmail send, reply, forward, draft — flags, identities, and compose patterns
---

# fastmail-cli — Compose (Send / Reply / Forward / Draft)

## Identities

Before composing, check available sender identities:

```bash
fastmail list identities
```

Use the identity email string with `--from` on any compose command.

---

## Send

```bash
fastmail send \
  --to "alice@example.com,bob@example.com" \
  --subject "Subject line" \
  --body "Plain text body" \
  [--cc "cc@example.com"] \
  [--bcc "bcc@example.com"] \
  [--from "alias@yourdomain.com"] \
  [--draft]
```

- `--to`, `--subject`, `--body` are required.
- Multiple recipients: comma-separated string.
- `--draft` saves to Drafts instead of sending.

## Reply

```bash
fastmail reply EMAIL_ID \
  --body "Reply text" \
  [--all] \
  [--cc "extra@example.com"] \
  [--bcc "hidden@example.com"] \
  [--from "alias@yourdomain.com"] \
  [--draft]
```

- `EMAIL_ID` is the email you're replying to (from `list`, `search`, or `thread`).
- `--all` replies to all recipients (reply-all).
- Threading headers (`In-Reply-To`, `References`) are set automatically.

## Forward

```bash
fastmail forward EMAIL_ID \
  --to "recipient@example.com" \
  [--body "Here's that email I mentioned..."] \
  [--cc "cc@example.com"] \
  [--bcc "bcc@example.com"] \
  [--from "alias@yourdomain.com"] \
  [--draft]
```

- `--body` is optional — text appears before the forwarded content.

---

## Common Patterns

```bash
# Reply from a specific alias
fastmail list identities
fastmail reply abc123 --body "On it." --from work-alias@mydomain.com

# Reply-all and BCC someone for records
fastmail reply abc123 --body "Thanks all." --all --bcc archive@mydomain.com

# Save a draft to review before sending
fastmail send --to x@y.com --subject "Careful email" --body "..." --draft

# Forward with context note
fastmail forward abc123 --to manager@company.com --body "FYI, see below."

# Quick reply inline
fastmail reply $(fastmail search --from boss@co.com --unread | jq -r '.data[0].id') \
  --body "Done."
```

---

## Notes

- Body is plain text only.
- For HTML or complex formatting, compose in Fastmail web and use `--draft` to stage.
- `--from` must match an identity returned by `list identities` — arbitrary addresses won't work.

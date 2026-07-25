---
name: fastmail/conversations
description: fastmail list, get, thread — reading emails, conversations, mark-read, and triage
---

# fastmail-cli — Conversations & Email Reading

## Listing Emails

```bash
fastmail list emails [-m MAILBOX] [-l LIMIT]
```

- Default mailbox: `INBOX`, default limit: `50`
- Returns email summaries (id, subject, from, date, flags)

```bash
# List a different folder
fastmail list emails --mailbox "Sent"
fastmail list emails --mailbox "Archive" --limit 100

# See all folders first
fastmail list mailboxes
```

## Reading a Single Email

```bash
fastmail get EMAIL_ID
```

Returns full email: headers, body (plain + HTML), attachment metadata.

## Reading a Full Thread/Conversation

```bash
fastmail thread EMAIL_ID
```

- Provide **any** email ID in the thread — returns all messages in chronological order.
- Ideal for understanding full context before replying.

## Typical Read Workflow

```bash
# 1. List inbox
fastmail list emails

# 2. Get a specific email by ID
fastmail get abc123

# 3. Get full thread for context
fastmail thread abc123

# 4. Mark as read when done
fastmail mark-read abc123

# 5. Reply or move
fastmail reply abc123 --body "Got it, thanks."
fastmail move abc123 --to "Archive"
```

## Mark Read / Unread

```bash
fastmail mark-read EMAIL_ID          # mark as read
fastmail mark-read EMAIL_ID --unread # mark as unread
```

## Triage

```bash
# Move to folder
fastmail move EMAIL_ID --to "Work/Projects"

# Mark as spam (prompts confirmation)
fastmail spam EMAIL_ID

# Skip confirmation
fastmail spam EMAIL_ID -y
```

## Tips

- Use `search` to find emails, then `thread` to get full context — this is the most useful combo for agents.
- `thread` is cheaper than running multiple `get` calls for each message in a conversation.
- IDs are stable — safe to store and reference later.

---
name: fastmail/masked
description: fastmail masked — create, list, enable, disable, and delete masked email addresses
---

# fastmail-cli — Masked Email

Masked emails are Fastmail's disposable address feature — each masked address forwards to your real inbox and can be disabled or deleted independently.

## Commands

```bash
fastmail masked list
fastmail masked create [--domain URL] [--description STR] [--prefix STR]
fastmail masked enable ID
fastmail masked disable ID
fastmail masked delete ID [-y]
```

## Create a Masked Email

```bash
# Basic (auto-generated address)
fastmail masked create

# With context metadata
fastmail masked create \
  --domain "https://example.com" \
  --description "Example site signup" \
  --prefix "example_shop"
```

- `--prefix`: custom address prefix, max 64 chars, `a-z`, `0-9`, `_` only
- `--domain`: the site it's for (metadata only, not enforced)
- `--description`: human-readable label

## Manage Existing Masked Addresses

```bash
# See all masked addresses with their IDs and status
fastmail masked list

# Temporarily stop forwarding (keep address, bounce/drop inbound)
fastmail masked disable MASKED_ID

# Re-enable
fastmail masked enable MASKED_ID

# Permanently delete
fastmail masked delete MASKED_ID
fastmail masked delete MASKED_ID -y   # skip confirmation
```

## Typical Patterns

```bash
# Create a throwaway for a signup
fastmail masked create --description "Newsletter signup" --prefix "news_acme"

# Getting spam? Disable immediately
fastmail masked list  # find the ID
fastmail masked disable abc-masked-id

# Clean up old ones
fastmail masked list | jq '.data[] | select(.description | test("old")) | .id'
fastmail masked delete OLD_ID -y
```

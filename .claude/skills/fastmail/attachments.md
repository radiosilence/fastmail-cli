---
name: fastmail/attachments
description: fastmail download — save attachments or extract text content from emails
---

# fastmail-cli — Attachments

## Download Attachments

```bash
fastmail download EMAIL_ID [OPTIONS]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Output directory (default: current dir) |
| `--format` | `-f` | `raw` (save files) or `json` (extract text content) |
| `--max-size` | | Max image size before resizing (e.g. `500K`, `1M`) |

## Examples

```bash
# Download all attachments to current directory
fastmail download abc123

# Download to specific folder
fastmail download abc123 -o ~/Downloads/invoices

# Extract text content as JSON (good for parsing docs/PDFs)
fastmail download abc123 -f json

# Resize large images during download
fastmail download abc123 --max-size 800K -o ~/Downloads
```

## Format Details

**`raw`** (default): Saves attachment files to disk as-is.

**`json`**: Extracts text from attachments using kreuzberg (supports 56+ formats including PDF, DOCX, XLSX, images with OCR, etc.). Returns structured JSON with content and detected language — useful for agents that need to read document contents without saving files.

## Workflow: Find Emails with Attachments Then Download

```bash
# Find emails with attachments
fastmail search --has-attachment --from invoices@vendor.com

# Download attachments from a specific email
fastmail download EMAIL_ID -o ~/Documents/invoices

# Or extract text for processing
fastmail download EMAIL_ID -f json | jq '.data[].content'
```

## Tips

- `get EMAIL_ID` includes attachment metadata (names, sizes, content types) without downloading — check what's there before downloading.
- Use `-f json` when you need to read document content programmatically; use `raw` when you need the actual files.
- `--max-size` is useful for emails with many large images where you only need thumbnails.

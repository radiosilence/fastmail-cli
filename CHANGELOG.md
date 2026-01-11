# Changelog

## [1.5.0] - 2026-01-11

### Added

- Contacts support via CardDAV (`contacts list`, `contacts search`)
- `search_contacts` MCP tool for Claude to look up email addresses by name
- `FASTMAIL_USERNAME` and `FASTMAIL_APP_PASSWORD` env vars for CardDAV auth

### Notes

- CardDAV requires an app password - Fastmail's API tokens only work for JMAP
- Generate app password at Fastmail Settings > Privacy & Security > Integrations > App passwords

## [1.4.1] - 2026-01-11

### Fixed

- Sending emails no longer leaves a draft behind - emails are created directly in Sent folder

## [1.4.0] - 2026-01-11

### Added

- MCP server (`fastmail-cli mcp`) for Claude Desktop integration
- 16 MCP tools: email CRUD, search, attachments, masked emails
- `mark-read` command to mark emails as read/unread
- `--max-size` flag for download command (resize images)
- `FASTMAIL_API_TOKEN` env var support (works for both CLI and MCP)
- Automatic image resizing for MCP attachments (stays under Claude's 1MB limit)
- Automatic text extraction for MCP attachments (PDF, DOCX, DOC)

### Changed

- Consolidated text extraction and image processing into shared utilities
- Removed tesseract/OCR dependency (send images to Claude instead)

## [1.3.0] - 2026-01-11

### Added

- `thread` command to view all emails in a conversation
- Full JMAP filter support for search command
- Search flags: `--text`, `--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`
- Search flags: `--mailbox`, `--has-attachment`, `--min-size`, `--max-size`
- Search flags: `--before`, `--after`, `--unread`, `--flagged`, `--pinned`
- `--pinned` shortcut for `--flagged --mailbox INBOX`

### Changed

- Search now uses explicit flags instead of query string parsing

## [1.2.0] - 2026-01-11

### Added

- Image OCR via tesseract (jpg, png, gif, tiff, webp, bmp)
- `--format json` for attachment text extraction
- PDF extraction via `pdf-extract` (pure Rust)
- DOCX extraction via `docx-lite` (pure Rust)
- DOC extraction via `textutil` (macOS) / `antiword` / `catdoc`

## [1.1.0] - 2026-01-11

### Added

- Feature table in README

## [1.0.0] - 2026-01-11

### Added

- Masked email support (`masked list`, `create`, `enable`, `disable`, `delete`)
- `https://www.fastmail.com/dev/maskedemail` JMAP capability

## [0.4.0] - 2026-01-11

### Added

- `reply` command with proper threading (In-Reply-To, References headers)
- `forward` command with message attribution
- `--all` flag for reply-all
- CC/BCC support on reply and forward

## [0.3.0] - 2026-01-10

### Added

- Shell completions (bash, zsh, fish, powershell)
- `completions` command

## [0.2.0] - 2026-01-10

### Added

- `download` command for attachments
- Blob download via JMAP

## [0.1.0] - 2026-01-10

### Added

- Initial release
- Authentication with API token
- List mailboxes and emails
- Get email details with body
- Search emails
- Send email with CC/BCC
- Move emails between mailboxes
- Mark as spam
- JSON output for all commands
- GitHub Actions CI/CD with automatic releases

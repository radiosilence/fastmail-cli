# Changelog

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

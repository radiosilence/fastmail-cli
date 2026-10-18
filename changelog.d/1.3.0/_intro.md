Released 2026-01-11.

### Added

- `thread` command to view all emails in a conversation
- Full JMAP filter support for search command
- Search flags: `--text`, `--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`
- Search flags: `--mailbox`, `--has-attachment`, `--min-size`, `--max-size`
- Search flags: `--before`, `--after`, `--unread`, `--flagged`

### Changed

- Search now uses explicit flags instead of query string parsing

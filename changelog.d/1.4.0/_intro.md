Released 2026-01-11.

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

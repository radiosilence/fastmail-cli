Released 2026-04-11.

### Added

- **HTML email body**: `--html-body` (inline) and `--html-file` (from file) flags on send, reply, and forward ([#20](https://github.com/radiosilence/fastmail-cli/pull/21)). JMAP assembles multipart/alternative automatically when both text and HTML are provided.
- **File attachments**: `--attachment` / `-a` flag (repeatable) on send, reply, and forward. Files are uploaded as blobs via JMAP's upload endpoint and attached with proper multipart/mixed MIME tree.
- **`upload_blob` JMAP method**: POST raw bytes to Fastmail's upload endpoint, returns a `blobId` for use in email composition.
- **GraphQL `html_body` parameter**: `sendEmail`, `replyToEmail`, and `forwardEmail` mutations accept optional HTML body content. Previews indicate when HTML is included.
- **Comprehensive test suite**: 55 tests covering body structure construction (all 4 JMAP modes), upload_blob with wiremock mocks, attachment loading, HTML resolution, and identity selection.

### Changed

- **Refactored body construction**: Extracted `apply_body_structure` pure function from `create_and_submit_email` — handles plain text, text+HTML (multipart/alternative), and attachments (multipart/mixed with nested alternative) in a single code path. Eliminated duplicated body/cc/bcc logic across compose methods.
- **Better upload error handling**: `upload_blob` now reports actual HTTP status and error body for 4xx failures instead of a confusing "missing blobId" message.
- `bodyValues` keys changed from `"body"` to `"textBody"` / `"htmlBody"` for clarity.
- Pinned GitHub Actions to commit SHAs.

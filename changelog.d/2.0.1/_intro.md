Released 2026-03-26.

### Fixed

- **Silent send failures**: EmailSubmission/set response is now checked — previously, email creation could succeed but submission could silently fail
- **Forward body extraction**: Fixed HashMap iteration ordering bug where forwarded email body could pick the wrong body part; now uses text_body parts correctly
- **Output::print panic**: Replaced `.unwrap()` with proper error handling when JSON serialization fails
- **Unsafe env var manipulation**: Removed `unsafe` blocks in config tests that used deprecated `std::env::set_var`/`remove_var`

### Changed

- **MCP confirmation tokens**: `sendEmail`, `replyToEmail`, `forwardEmail` mutations now return a `confirmationToken` from PREVIEW that must be passed to CONFIRM/DRAFT — prevents accidental sends without preview
- **Commit Cargo.lock**: Removed from `.gitignore` — binary crates should have reproducible builds
- **Mailbox caching**: `list_mailboxes` result cached after first fetch, avoiding redundant API calls during compose operations
- **Deduplicated send/reply/forward**: Extracted `create_and_submit_email` helper with `EmailDraft` struct (~80 lines removed)
- **XML parsing**: Replaced hand-rolled string-splitting XML parser with `roxmltree` for CardDAV responses
- **vCard parsing**: Added RFC 6350 line unfolding and quoted-printable decoding for contact names/fields
- **account_id() helper**: Extracted repeated 3-line `session()?.primary_account_id().ok_or(...)` pattern into a single helper method
- **Renamed `md5_hash` → `hash_id`**: The function uses SipHash (DefaultHasher), not MD5 — name was misleading
- **Removed `#[allow(dead_code)]`** on `impl Email` — removed unused `sender_display` method
- **Use Display trait**: Forward email sender formatting now uses `EmailAddress::Display` instead of manual format logic

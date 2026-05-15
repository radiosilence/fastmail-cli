# Changelog

## [Unreleased]

### Changed

- Dependency updates via `cargo update` (Cargo.lock only).

## [2.2.2] - 2026-04-18

### Changed

- **NonceStore is now bounded** ([#27](https://github.com/radiosilence/fastmail-cli/pull/27)): 15-minute TTL per nonce and a hard cap of 256 outstanding nonces. Closes [#25](https://github.com/radiosilence/fastmail-cli/issues/25) — follow-up from the v2.2.1 security audit.
- **Dep bumps**: `kreuzberg` 4.4 → 4.8, `toml` 0.8 → 1.1, `rmcp` 0.12 → 1.5, `schemars` 0.8 → 1.2. The rmcp 1.0 `#[non_exhaustive]` model-struct fallout moved `get_info()` onto the builder API (`ServerInfo::new().with_*()` instead of struct literals) — cleaner read, same behaviour.

## [2.2.1] - 2026-04-18

### Fixed

- **Reply-all preview divergence (B1)** — The MCP `replyToEmail` mutation's PREVIEW path never consulted `reply_all` when building recipients, so calling it with `all: true` showed only the original sender in `To` and whatever the user explicitly passed as `cc`. Meanwhile the send path in `reply_email` expanded reply-all correctly. This had two knock-on effects: (1) the preview lied about who would actually receive the email, and (2) a user who "fixed" the under-reported preview by passing missing recipients as explicit `cc` could produce a duplicate-send, because those same addresses would also be expanded into `To` by the send path at CONFIRM time. Extracted `jmap::expand_reply_recipients` as a shared pure function used by both preview and send; the function now also deduplicates by lowercase email and strips from `Cc` anything already present in `To`, closing the duplicate-send window regardless of how the paths evolve. 9 unit tests cover reply-all expansion, me-filtering (case-insensitive), dedup, and the exact overlap scenario from the bug report.

### Security

- **Attachment path traversal (C1)** — `fastmail-cli download` wrote attachments to `Path::new(out_dir).join(attachment.name)`, where `attachment.name` is chosen by the email sender. A name of `../../etc/cron.d/pwn` escaped the output directory via relative traversal; an absolute name like `/etc/cron.d/pwn` replaced the base path outright because `Path::join` discards the base when the joined segment is absolute. A malicious email could write arbitrary files on any recipient who ran the `download` subcommand. Filenames are now run through `util::sanitize_filename`, which strips path separators, NUL/control bytes, and Windows-reserved stems (CON/PRN/NUL/COM1-9/LPT1-9). Writes use `OpenOptions::create_new(true)`, so silent overwrites and symlink-pre-placement attacks at the target path are also refused.
- **CardDAV URL injection (C2)** — `list_addressbooks()` interpolated the raw username into `/dav/addressbooks/user/{}/` without percent-encoding. Misconfigured usernames containing `/`, `?`, `#`, or `%` produced malformed URLs that could target a different CardDAV endpoint. Now percent-encoded with an explicit path-segment set.
- **Token file TOCTOU (H1)** — `Config::save()` ran `fs::write(path, token)` followed by `fs::set_permissions(0o600)`, leaving a window where the token file was readable under the default umask. The write is now atomic: the token is written to a sibling `.tmp` file opened with `OpenOptions::mode(0o600).create_new(true)`, then `rename()`d over the target. The parent directory is created with `DirBuilder::mode(0o700)`.
- **Symlinked config path (H2)** — `fs::write` followed symlinks at the config file path. A hostile program with write access to `~/.config/fastmail-cli/` could pre-place a symlink redirecting the token write. `save()` now checks `symlink_metadata()` and refuses to write if the target is a symlink.
- **Token in argv (H3)** — `fastmail-cli auth YOUR_TOKEN` exposed the token to `ps`, shell history, and the process environment. The token argument is now optional; when omitted it is read from stdin (with a TTY prompt). The positional form is retained for backward compatibility.
- **URL template substitution bleed (M1)** — `download_blob` and `upload_blob` built URLs by chaining `str::replace`, which would recursively substitute a template-like value into a later placeholder. Replaced with a single-pass `apply_url_template` helper. Defense-in-depth — no live bug, all current inputs are trusted — but it future-proofs the code against trust-boundary changes.
- **Stateless compose confirmation (M3)** — The MCP `sendEmail` / `replyToEmail` / `forwardEmail` PREVIEW→CONFIRM flow used a `DefaultHasher` of the params as the confirmation token, which was a signature rather than a nonce — any caller who knew the params could produce a valid token without ever calling PREVIEW. Replaced with a random UUIDv4 nonce issued on PREVIEW, stored server-side, and consumed one-shot on CONFIRM/DRAFT with a params-fingerprint check so tampering between PREVIEW and CONFIRM is detected.
- **`InvalidToken` variant footgun (M4)** — The variant held `String`, inviting future contributors to embed the actual token in the error payload for "better debug output". Narrowed to `&'static str` so only compile-time literals can be passed.
- **`rustls-webpki` name-constraint bypass** — Transitive upgrade to 0.103.12 via dependency bumps, fixing [RUSTSEC-2026-0098](https://rustsec.org/advisories/RUSTSEC-2026-0098) and [RUSTSEC-2026-0099](https://rustsec.org/advisories/RUSTSEC-2026-0099).

### Fixed

- **Reply-all preview divergence (B1)** — The MCP `replyToEmail` PREVIEW path computed recipients differently from the actual send path, under-reporting who would be emailed. The natural workaround of sending twice could deliver duplicate emails. Extracted `expand_reply_recipients` as a shared pure function with dedupe-by-lowercase-email; preview and send now share one recipient list.

### Changed

- `auth` CLI arg is now `Option<String>` (backward compatible — the positional form still works).

Security audit contributed by [@dylanbyars](https://github.com/dylanbyars) ([#23](https://github.com/radiosilence/fastmail-cli/pull/23)).

## [2.2.0] - 2026-04-11

### Added

- **Contact CRUD** ([#17](https://github.com/radiosilence/fastmail-cli/issues/17)): `contacts create`, `contacts update`, `contacts delete` CLI commands for managing contacts via CardDAV.
- **GraphQL contact mutations**: `createContact`, `updateContact`, `deleteContact` mutations in the MCP server, so AI assistants can manage contacts too.
- **`ContactFields` struct**: Replaces positional args for contact write operations, keeping clippy happy and the API clean.
- **vCard builder**: `build_vcard()` generates vCard 3.0 strings with proper `N`/`FN`/`EMAIL`/`TEL`/`ORG`/`TITLE`/`NOTE` properties.
- **4 new tests**: vCard building, roundtrip parsing, UID generation.

### Changed

- `contacts` CLI subcommand now has `create`, `update`, `delete` subcommands alongside existing `list` and `search`.
- Update merges fields: only provided fields are overwritten, existing fields are preserved.
- Delete requires `-y` confirmation flag (consistent with `masked delete` and `spam`).

## [2.1.0] - 2026-04-11

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

## [2.0.1] - 2026-03-26

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

## [2.0.0] - 2026-03-22

### Breaking

- **MCP interface replaced**: 18 individual tools collapsed into 2 GraphQL tools (`schema_sdl` + `graphql`). LLM clients must update to use GraphQL queries/mutations instead of calling tools by name.
- Removed `format.rs` — GraphQL returns structured JSON; formatting is now the LLM's responsibility.
- All MCP request structs (`ListEmailsRequest`, `SearchEmailsRequest`, `SendEmailRequest`, etc.) removed.

### Added

- `async-graphql` schema covering all previous operations: queries (mailboxes, emails, search, threads, identities, attachments, masked emails, contacts) and mutations (send, reply, forward, move, mark read, mark spam, masked email CRUD).
- `schema_sdl` tool — returns full GraphQL SDL for LLM introspection.
- `graphql` tool — executes arbitrary queries/mutations with optional JSON variables.
- **Nested attachment resolution** — `Email.attachments` returns `Attachment` objects with a lazy `content` field. Query `{ email(id: "x") { subject attachments { name content { textContent base64Content } } } }` to fetch email + attachment data in a single round trip.
- **Full thread content** — `thread` query returns complete `Email` objects (not summaries), so the LLM gets full body + attachments for entire conversations.
- `Identity` type now exposes `textSignature`, `htmlSignature`, `replyTo`, and `bcc`.
- `Email` type exposes `keywords` field for raw flag access.
- `Thread` type for thread queries (returns sorted emails + count).
- Structured `ComposeResult` and `Status` types replace text-formatted responses.
- `SendAction` and `SpamAction` enums exposed as GraphQL input enums.

### Changed

- MCP server instructions updated with GraphQL query examples.
- README MCP section rewritten for the two-tool pattern.
- Token-efficient: LLM fetches schema once, then composes exactly the queries it needs.

### Fixed

- Pin kreuzberg to ~4.4 — 4.5.3 has compile errors with `pdf` feature (filed upstream: kreuzberg-dev/kreuzberg#550).

## [1.8.1] - 2026-03-20

### Fixed

- Reply-all no longer silently drops all recipients when sender email is empty string
- Drafts now always attempt identity resolution via `--from` and skip gracefully on failure
- Drafts now receive both `$draft` and `$seen` keywords (previously only `$draft`)

### Changed

- `SendAction` is now a proper enum (`preview`/`confirm`/`draft`) instead of a bare string — improves MCP type safety
- `ComposeParams` struct eliminates `clippy::too_many_arguments` across send/reply/forward; removed all `#[allow]` attributes
- Shared `ComposeContext` helper deduplicates ~50 lines of branching in send/reply/forward
- CLI JSON output now includes `"status": "draft"` or `"status": "sent"` to differentiate results
- MCP preview text for send/reply/forward now mentions `action='draft'` option

Thanks to [@thrawny](https://github.com/thrawny) (Jonas Lergell) for the original PR (#9).

## [1.8.0] - 2026-02-27

### Added

- `--from` flag on send, reply, and forward to choose which identity/alias to send from
- `list identities` command to view available sender identities
- `list_identities` MCP tool
- Identity selection tests (`pick_identity`)

### Changed

- Identity resolution extracted into testable pure function

Thanks to [@bgilly](https://github.com/bgilly) for the original PR (#6).

## [1.7.2] - 2026-02-27

### Fixed

- Read-only API tokens no longer crash with "error decoding response body" — capabilities are filtered against the session
- Send/reply/forward fail fast with actionable error when token lacks submission capability
- Masked email operations fail fast when token lacks maskedemail capability
- Non-JSON API error responses (e.g. 400 from disallowed capabilities) are now surfaced instead of generic parse failures

### Changed

- Capabilities are computed once at authentication, not on every request
- `require_capability` is now generic — used for both submission and masked email checks

Thanks to [@kylehowells](https://github.com/kylehowells) for the original PR (#4).

## [1.7.0] - 2026-01-11

### Changed

- Text extraction now uses [kreuzberg](https://github.com/kreuzberg-dev/kreuzberg) - supports 56 formats
- No longer requires system tools (textutil/antiword) for DOC files
- Added language detection for extracted text

### Supported Formats

Documents (PDF, DOC, DOCX, ODT, RTF), Spreadsheets (XLS, XLSX, ODS, CSV), Presentations (PPT, PPTX), eBooks (EPUB, FB2), Markup (HTML, XML, Markdown, RST, Org), Data (JSON, YAML, TOML), Email (EML, MSG), Archives (ZIP, TAR, GZ, 7z), Academic (BibTeX, LaTeX, Typst, Jupyter notebooks)

## [1.6.0] - 2026-01-11

### Changed

- **Breaking:** Config file moved from `~/.fastmail-cli/config.json` to `~/.config/fastmail-cli/config.toml`
- Config now uses TOML format with `[core]` and `[contacts]` sections

### Migration

Old config:

```json
{ "api_token": "...", "username": "...", "app_password": "..." }
```

New config (`~/.config/fastmail-cli/config.toml`):

```toml
[core]
api_token = "..."

[contacts]
username = "..."
app_password = "..."
```

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
- Search flags: `--before`, `--after`, `--unread`, `--flagged`

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

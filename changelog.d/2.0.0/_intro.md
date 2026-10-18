Released 2026-03-22.

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

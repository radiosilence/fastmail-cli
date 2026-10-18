Released 2026-07-25.

### Added

- **`--graphql` and `--graphiql` on `mcp --http`.** The HTTP server can now
  mount a plain GraphQL-over-HTTP endpoint at `/graphql` and the GraphiQL IDE at
  `/`, alongside MCP at `/mcp` on the same port, so the schema can be explored in
  a browser rather than only through an LLM. `/mcp` speaks JSON-RPC, which a
  browser doesn't, hence the separate endpoint. `--browser` opens the IDE once
  the listener is bound. GraphiQL is loaded from a CDN with pinned versions and
  SRI hashes; the page is an Askama template.
- **`--http` now takes an optional address**, defaulting to `127.0.0.1:8080`.
- **Every collection is a connection.** `mailboxes`, `identities`,
  `maskedEmails`, `contacts`, `attachments`, `Mailbox.children` and
  `Thread.emails` now take `first`/`last`/`after`/`before` and expose
  `totalCount`, `pageInfo`, `edges` and `nodes`, with IDs for cursors. Only
  `emails` pages server-side; the rest arrive whole from one call, so their
  `totalCount` is free and a stale cursor errors rather than silently shifting
  the page. `Thread.emails` returns the same `EmailConnection` as the query
  does — `queryState` is null there, since a conversation is not a query.
  Value lists (`from`, `to`, `cc`, `keywords`, `headers`, …) stay plain arrays:
  they belong to the parent and paging them would be ceremony.
- **`cid`, `charset` and `partId` on `Attachment`.** All three already arrived
  with every full email fetch and were discarded. `cid` is the useful one:
  inline images are referenced from the HTML body as `<img src="cid:...">`, so
  without it there is no way to tell which attachment appears where.
- **`Email.sender`** — RFC 5322 Sender, distinct from `From`, set when a message
  was sent on the author's behalf. In the summary property set, so lists carry
  it with no extra call.
- **`Email.headers`** — every raw header in order, for the ones the typed fields
  don't cover (`List-Unsubscribe`, `Received`, `Authentication-Results`). On the
  full property set, so it resolves lazily and batched like the bodies.
- **`Mailbox.myRights` and `Mailbox.isSubscribed`** — what the account may
  actually do in a folder, so a move or flag change can be checked in advance
  rather than failing at the write.
- **Introspection over `/graphql` needs no Fastmail token.** A query whose
  top-level selections are all introspection fields is answered from the schema
  without authenticating or touching the network, so GraphiQL's docs,
  autocomplete and explorer work before credentials do — otherwise a bad token
  leaves you with an IDE that cannot describe the API you are trying to explore.
  Anything selecting a real field, mixing introspection with real fields, or
  hiding behind a fragment takes the normal authenticated path.

### Changed

- **`mcp --http` falls back to local credentials when a request sends no
  `X-Fastmail-Token`.** Previously the HTTP transport had no fallback at all and
  every request needed the header, which made running it locally awkward. The
  header still wins where present, and the fallback is best-effort — a hosted
  deployment ships no config or `FASTMAIL_API_TOKEN`, so it stays absent there
  and per-request auth is unchanged. Note the consequence: binding a non-loopback
  address on a machine that _does_ have credentials now serves them to anything
  that can reach the port.

- **Full nesting across the GraphQL graph.** Every logical edge is now
  traversable, so a client can fetch emails, bodies and attachment content in a
  single query instead of looping:
  - `Query.mailbox(name:)` — look up one folder by name or role.
  - `Mailbox.emails(limit:)`, `Mailbox.parent`, `Mailbox.children` — navigate
    from a folder into its contents and around the folder tree.
  - `Email.thread` — the whole conversation from any email.
  - `Email.mailboxes` — the folders an email lives in, resolved from `mailboxIds`.
  - `Thread.id`.
- **DataLoader batching for every lazily-resolved field.** Bodies, attachment
  metadata, threads, mailboxes and blob downloads are fetched through
  request-scoped DataLoaders, so resolvers that run concurrently collapse into
  one batched API call. Selecting `textBody` on a page of 25 emails costs one
  extra `Email/get` rather than 25 — and repeating an ID within a query is free.
  Attachment blobs, which have no batch endpoint, are downloaded concurrently
  instead of one at a time.
- **Query depth and complexity limits** (depth 15; nested lists priced by page
  size). The graph now contains cycles by design, so unbounded queries are
  rejected during validation, before any API call is made.

- **Composable `EmailFilter`.** Filters are now a tree mirroring JMAP's
  `FilterOperator` (RFC 8620 §5.5) instead of a flat argument list: scalar
  fields on one filter are AND-ed, and `and` / `or` / `not` nest arbitrarily, so
  a single query can express "unread, from either address, but not in Archive".
  Mailbox names and roles are resolved to IDs at every depth of the tree in one
  `Mailbox/get`. The same input type is accepted everywhere emails appear —
  on `Mailbox.emails` it is AND-ed with the mailbox.
- **Sorting** via `sort: [EmailSort!]`, over `receivedAt`, `sentAt`, `size`,
  `subject`, `from` and `to`, with tie-breakers. Previously hardcoded to
  newest-first.
- **Relay connections with anchor-based pagination.** Email lists return
  `EmailConnection` with `edges`/`nodes`, `pageInfo`, `totalCount`, `position`
  and `queryState`. **Cursors are email IDs**, mapped onto JMAP's `anchor` /
  `anchorOffset`: a positional cursor shifts every time mail arrives, so page 2
  would re-show or skip messages, whereas an anchor names a specific message and
  stays correct. They also stay legible for a model composing the follow-up
  query — the cursor is an ID it has already seen. `last` without `before`
  becomes a negative `position`, which JMAP counts from the end, so "the last N"
  is one call and needs no total. A cursor whose email has been deleted or no
  longer matches yields a clear "restart pagination" error rather than silently
  wrong results. Page size is capped at 100; conflicting `first`+`last` or
  `after`+`before` are rejected.
- **The full JMAP filter and sort surface.** `EmailFilter` now mirrors
  `FilterCondition` (RFC 8621 §4.4.1) field for field, adding
  `inMailboxOtherThan`, `allInThreadHaveKeyword`, `someInThreadHaveKeyword`,
  `noneInThreadHaveKeyword` and raw `header` matching. `EmailSort` adds the
  keyword comparators (`hasKeyword`, `allInThreadHaveKeyword`,
  `someInThreadHaveKeyword`) plus `collation`; comparators that require a
  `keyword` — and those that reject one — are validated before any API call.
- **Counts without fetching.** `totalCount` maps to JMAP's `calculateTotal`,
  which costs the server real work, so it is requested only when the field is
  selected. A query selecting _only_ `totalCount` issues one `Email/query` and
  fetches no emails at all — `{ emails(filter: {unread: true}) { totalCount } }`
  answers "how many?" without transferring a single message.
- **`collapseThreads`** on every email list, for a conversation view.

### Changed

- **`emails` and `searchEmails` return `Email` instead of `EmailSummary`.**
  `EmailSummary` was a dead end — reaching a body or an attachment meant a
  separate `email(id:)` round trip per result. `Email` is a superset of its
  fields under the same names, so existing queries are unaffected; the extra
  fields simply resolve lazily now. The `EmailSummary` type is gone from the
  schema.
- **`emails(mailbox:, limit:)` is now `emails(filter:, sort:, first:, after:, …)`**
  returning a connection. `searchEmails` is deprecated — it remains as a shim
  that maps each of its flat arguments onto one leaf of an `EmailFilter`, and
  also returns a connection.
- List and search results now also carry `blobId`, `sentAt`, `bcc` and `replyTo`
  without a second fetch.
- `thread(emailId:)` returns emails with attachment metadata, which it
  previously omitted.

### Fixed

- **List results now preserve the requested sort order.** `Email/get` makes no
  ordering guarantee, but the results were returned in whatever order the server
  sent them; they are now re-ordered to match the `Email/query` result. Affects
  the `list` and `search` CLI commands as well as GraphQL.

### Docs

- README: comparison table against Fastmail's official MCP server — positions
  `fastmail-cli` as the self-hosted CLI+MCP alternative (masked email,
  attachment text extraction, spam training, self-custody) vs the hosted
  official server (zero-setup, OAuth, and a wider suite: calendar, notes, org
  directory).

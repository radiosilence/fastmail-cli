# fastmail-cli

CLI for Fastmail's JMAP API. Read, search, send, and manage emails from your terminal or AI assistant.

## Features

| Feature               | Description                                                            |
| --------------------- | ---------------------------------------------------------------------- |
| **Email**             | List, search, read, send, reply, forward, threads, identity selection, HTML bodies, file attachments |
| **Mailboxes**         | List folders, move emails, mark spam/read                              |
| **Contacts**          | Search, create, update, delete contacts via CardDAV                    |
| **Attachments**       | Download files, extract text, resize images                            |
| **Text Extraction**   | 56 formats via [kreuzberg](https://github.com/kreuzberg-dev/kreuzberg) |
| **Image Resizing**    | `--max-size` to resize images on download                              |
| **Masked Email**      | Create, list, enable/disable aliases                                   |
| **MCP Server**        | Claude integration via Model Context Protocol                          |
| **Shell Completions** | Bash, Zsh, Fish, PowerShell                                            |
| **JSON Output**       | All commands output JSON for scripting                                 |

## Compared to Fastmail's official MCP

Fastmail [ships an official MCP server](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/)
(hosted at `api.fastmail.com/mcp`, OAuth with read/write/send scopes). It's
zero-setup and covers more of Fastmail's suite — calendar, notes, and org
directory that `fastmail-cli` doesn't touch. `fastmail-cli` is the self-hosted
alternative: a CLI *and* an MCP server you run yourself, open source, with
masked email, attachment text extraction, and spam-filter training the official
server doesn't offer — plus full custody of the data path.

|                                                            | `fastmail-cli`                     | Fastmail official MCP            |
| ---------------------------------------------------------- | ---------------------------------- | -------------------------------- |
| Interface                                                  | CLI **and** MCP (stdio / HTTP)     | MCP only                         |
| Setup                                                      | install + run the binary           | add URL + OAuth, nothing to run  |
| Auth                                                       | API token (+ app password for CardDAV) | OAuth: `read` / `write` / `send` |
| Hosting / data path                                        | your machine or your own server    | Fastmail-hosted                  |
| Source                                                     | open source (MIT), extensible      | proprietary                      |
| Email: read / search / threads                             | ✅ (rich search filters)           | ✅                               |
| Send / reply / forward (preview → confirm/draft)           | ✅                                 | ✅                               |
| Move / archive / mark read                                 | ✅                                 | ✅                               |
| Mark as spam (+ trains the filter)                         | ✅                                 | —                                |
| Masked Email (create / enable / disable / delete)          | ✅                                 | —                                |
| Attachments: text extraction (56 formats) + image resize   | ✅                                 | —                                |
| Contacts (create / update / delete / search)               | ✅ (CardDAV)                       | ✅                               |
| Org directory search                                       | —                                  | ✅                               |
| Calendar                                                   | —                                  | ✅                               |
| Notes                                                      | —                                  | ✅                               |
| Identities / signatures                                    | ✅                                 | ✅                               |

Use Fastmail's for zero maintenance and the wider suite (calendar, notes);
use `fastmail-cli` for a scriptable CLI, the masked-email / attachment-extraction
/ spam-training tooling, or to self-host and keep the data path yours.
(`fastmail-cli`'s column is verified against its GraphQL schema; Fastmail's is
its current hosted tool set and may grow.)

## Quick Start

### Installation

#### From GitHub Releases (recommended for mise)

```bash
# Add to mise config
mise use -g "github:radiosilence/fastmail-cli"
```

#### From Source

```bash
cargo install --git https://github.com/radiosilence/fastmail-cli
```

### Authentication

1. Generate an API token at [Fastmail Settings > Privacy & Security > Integrations > API tokens](https://app.fastmail.com/settings/security/tokens)
2. Auth with the CLI — the token is read from stdin so it stays out of shell history, `ps`, and the process environment:

```bash
# interactive — paste the token at the prompt
fastmail auth

# non-interactive — pipe from a password manager, file, or env var
echo "$FASTMAIL_TOKEN" | fastmail auth
```

The positional form `fastmail auth YOUR_TOKEN` still works for backward compatibility, but the stdin form is preferred.

Token is stored in `~/.config/fastmail-cli/config.toml` with `0600` permissions (directory `0700`). The file is written atomically via rename, and the path is refused if it's a symlink.

### Configuration

Credentials can be set via environment variables or config file. Env vars take precedence.

**Environment variables:**

```bash
export FASTMAIL_API_TOKEN="fmu1-..."      # Required for JMAP (email)
export FASTMAIL_USERNAME="you@fastmail.com"  # Required for CardDAV (contacts)
export FASTMAIL_APP_PASSWORD="xxxx..."    # Required for CardDAV (contacts)
```

**Config file** (`~/.config/fastmail-cli/config.toml`):

```toml
[core]
api_token = "fmu1-..."

[contacts]
username = "you@fastmail.com"
app_password = "xxxx..."
```

The `auth` command only sets `[core].api_token`. For contacts, add `[contacts]` section manually or use env vars.

## Usage

All output is JSON for easy scripting with `jq`.

### List Mailboxes

```bash
fastmail list mailboxes
```

### List Emails

```bash
# Default: INBOX, 50 emails
fastmail list emails

# Specific mailbox and limit
fastmail list emails --mailbox Sent --limit 10
```

### Get Email Details

```bash
fastmail get EMAIL_ID
```

### Search

Search uses JMAP filter flags (all filters are ANDed together):

```bash
# Full-text search
fastmail search --text "meeting notes"

# Filter by header fields
fastmail search --from "alice@example.com"
fastmail search --to "bob" --subject "project"

# Filter by mailbox
fastmail search --mailbox Sent --limit 10

# Attachments and size
fastmail search --has-attachment
fastmail search --min-size 1000000  # > 1MB

# Date range (ISO 8601)
fastmail search --after 2024-01-01 --before 2024-12-31

# Status filters
fastmail search --unread
fastmail search --flagged

# Combine filters
fastmail search --from "boss" --has-attachment --after 2024-06-01 --limit 20
```

Available flags: `--text`, `--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`, `--mailbox`, `--has-attachment`, `--min-size`, `--max-size`, `--before`, `--after`, `--unread`, `--flagged`

### List Identities

View available sender identities (useful for `--from`):

```bash
fastmail list identities
```

### Send Email

```bash
fastmail send \
  --to "alice@example.com, bob@example.com" \
  --subject "Hello" \
  --body "Message body here"

# With CC/BCC
fastmail send \
  --to "alice@example.com" \
  --cc "bob@example.com" \
  --bcc "secret@example.com" \
  --subject "Hello" \
  --body "Message"

# Send from a specific identity/alias
fastmail send \
  --to "alice@example.com" \
  --from "alias@yourdomain.com" \
  --subject "Hello" \
  --body "Message"

# HTML email body (inline or from file)
fastmail send \
  --to "alice@example.com" \
  --subject "Newsletter" \
  --body "Plain text fallback" \
  --html-body "<h1>Hello</h1><p>Rich content here</p>"

fastmail send \
  --to "alice@example.com" \
  --subject "Report" \
  --body "See attached" \
  --html-file ./email.html

# File attachments (repeatable)
fastmail send \
  --to "alice@example.com" \
  --subject "Documents" \
  --body "Please review" \
  -a report.pdf -a data.xlsx
```

### Move Email

```bash
fastmail move EMAIL_ID --to Archive
fastmail move EMAIL_ID --to Trash
```

### Mark as Spam

```bash
# Requires confirmation
fastmail spam EMAIL_ID

# Skip confirmation
fastmail spam EMAIL_ID -y
```

### Mark as Read/Unread

```bash
# Mark as read
fastmail mark-read EMAIL_ID

# Mark as unread
fastmail mark-read EMAIL_ID --unread
```

### Download Attachments

```bash
# Download to current directory
fastmail download EMAIL_ID

# Download to specific directory
fastmail download EMAIL_ID --output ~/Downloads

# Extract text content as JSON (PDF, DOCX, DOC, TXT)
fastmail download EMAIL_ID --format json

# Resize images to max 500KB
fastmail download EMAIL_ID --max-size 500K
```

Text extraction uses [kreuzberg](https://github.com/kreuzberg-dev/kreuzberg) and supports 56 formats:

- **Documents**: PDF, DOC, DOCX, ODT, RTF
- **Spreadsheets**: XLS, XLSX, ODS, CSV, TSV
- **Presentations**: PPT, PPTX
- **eBooks**: EPUB, FB2
- **Markup**: HTML, XML, Markdown, RST, Org
- **Data**: JSON, YAML, TOML
- **Email**: EML, MSG
- **Archives**: ZIP, TAR, GZ, 7z
- **Academic**: BibTeX, LaTeX, Typst, Jupyter notebooks

### Reply to Email

```bash
# Reply to sender only
fastmail reply EMAIL_ID --body "Thanks for your message"

# Reply all
fastmail reply EMAIL_ID --body "Thanks everyone" --all

# Reply with additional CC/BCC
fastmail reply EMAIL_ID --body "Response" --cc "boss@example.com"

# Reply from a specific identity
fastmail reply EMAIL_ID --body "Thanks" --from "alias@yourdomain.com"
```

### Forward Email

```bash
fastmail forward EMAIL_ID \
  --to "colleague@example.com" \
  --body "FYI - see below"

# Forward from a specific identity
fastmail forward EMAIL_ID \
  --to "colleague@example.com" \
  --from "alias@yourdomain.com" \
  --body "FYI"
```

### Shell Completions

```bash
# Bash
fastmail completions bash >> ~/.bashrc

# Zsh
fastmail completions zsh >> ~/.zshrc

# Fish
fastmail completions fish > ~/.config/fish/completions/fastmail.fish
```

### Contacts

CRUD operations for Fastmail contacts via CardDAV. Requires an app password (API tokens don't work for CardDAV).

```bash
# Set credentials
export FASTMAIL_USERNAME="you@fastmail.com"
export FASTMAIL_APP_PASSWORD="your-app-password"

# List all contacts
fastmail contacts list

# Search by name, email, or organization
fastmail contacts search "alice"

# Create a new contact
fastmail contacts create --name "Jane Doe" --email "jane@example.com" --organization "Acme Corp"

# Update an existing contact (only provided fields are changed)
fastmail contacts update CONTACT_ID --organization "New Corp" --title "CEO"

# Delete a contact (requires -y confirmation)
fastmail contacts delete CONTACT_ID -y
```

Generate an app password at [Fastmail Settings > Privacy & Security > Integrations > App passwords](https://app.fastmail.com/settings/security/devicekeys).

### Masked Email

Create disposable email addresses for signups. Requires Fastmail's masked email feature.

```bash
# List all masked emails
fastmail masked list

# Create a new masked email
fastmail masked create --domain "https://example.com" --description "Example Site"

# Create with custom prefix
fastmail masked create --prefix "shopping" --description "Shopping sites"

# Enable/disable a masked email
fastmail masked enable MASKED_EMAIL_ID
fastmail masked disable MASKED_EMAIL_ID

# Delete (requires confirmation)
fastmail masked delete MASKED_EMAIL_ID -y
```

## Output Format

All commands output JSON with this structure:

```json
{
  "success": true,
  "data": { ... },
  "message": "optional status message",
  "error": "error message if success=false"
}
```

### Parsing with jq

```bash
# Get unread count for INBOX
fastmail list mailboxes | jq '.data[] | select(.role == "inbox") | .unreadEmails'

# List email subjects
fastmail list emails | jq '.data.emails[].subject'

# Get email body
fastmail get EMAIL_ID | jq -r '.data.bodyValues | to_entries[0].value.value'
```

## Claude Code Skills

If you're using [Claude Code](https://claude.ai/claude-code), this repo ships skills that teach agents how to use the CLI — no need to explain flags or workflows manually.

Copy the skills into your project's `.claude/skills/` directory (or anywhere Claude Code loads skills from), then invoke them:

```
/fastmail              # full command reference + common patterns
/fastmail/search       # search filters, date ranges, workflows
/fastmail/compose      # send, reply, forward, drafts, identities
/fastmail/conversations # list, get, thread, mark-read, triage
/fastmail/attachments  # download, raw vs json, text extraction
/fastmail/masked       # masked email CRUD
/fastmail/contacts     # CardDAV setup, list/search
```

Skills are in `.claude/skills/` in this repo. Each one includes concrete examples and agent-oriented workflow patterns.

## MCP Server (Claude Integration)

Run as an MCP server for use with Claude Desktop or other MCP clients:

```bash
fastmail mcp
```

Configure in Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "fastmail": {
      "command": "mise",
      "args": ["x", "--", "fastmail", "mcp"],
      "env": {
        "FASTMAIL_API_TOKEN": "your-token-here",
        "FASTMAIL_USERNAME": "you@fastmail.com",
        "FASTMAIL_APP_PASSWORD": "your-app-password"
      }
    }
  }
}
```

Username and app password are optional - only needed for contact search (CardDAV requires app password, API tokens don't work).

### HTTP transport

Three independent surfaces, each opt-in, sharing one port (default
`127.0.0.1:8080`, or pass an address to `--http`):

| Flag         | Serves                                                      |
| ------------ | ----------------------------------------------------------- |
| `--http`     | MCP streamable-HTTP at `/mcp`                               |
| `--graphql`  | plain GraphQL-over-HTTP at `/graphql`                       |
| `--graphiql` | the GraphiQL IDE at `/`, and its `/graphql`                 |
| `--browser`  | opens the IDE once the port is bound, implying `--graphiql` |

```bash
fastmail mcp                                   # stdio MCP, no listener
fastmail mcp --browser                         # just the IDE, opened for you
fastmail mcp --http                            # just /mcp
fastmail mcp --http 0.0.0.0:8080 --graphql     # both, explicit address
```

Asking for any surface binds the listener; there is nowhere to mount an HTTP
route over stdio. Only `--http` puts MCP on it — the transport a model connects
through and a browsable endpoint for you are separate things. `--browser`
implies `--graphiql`, since the IDE is what it opens.

`/graphql` is plain GraphQL-over-HTTP, which is what a browser speaks; `/mcp` is
MCP JSON-RPC, which it doesn't. That is why GraphiQL needs its own route rather
than pointing at the MCP one. Both share the schema, the client cache and the
credential resolution below, so the IDE sees exactly what a model sees.

**Token resolution is the same everywhere:** the request's `X-Fastmail-Token`
header wins, otherwise the configured token (or `FASTMAIL_API_TOKEN`) is used.
Running it yourself, that means your own credentials with no ceremony. In a
hosted deployment there is no local token, so the fallback is absent and every
request must carry the header, injected by a trusted upstream after it has
authenticated the caller. Authenticated JMAP clients are cached per token, so
the JMAP session handshake runs once per distinct token rather than per call.

Do **not** expose this to the internet without such an auth layer in front —
the header is trusted unconditionally. Equally, do not run it with local
credentials present on a non-loopback address: anything that can reach the port
gets your mailbox without needing a token at all.

The token is resolved on first query rather than at startup, so an expired one
shows up as an error in the response pane rather than a server that won't boot —
run `fastmail auth` to refresh it, or ask `session` first. **Introspection needs
no token**: it is answered from the schema without touching Fastmail, so
GraphiQL's docs, autocomplete and explorer work before you have working
credentials. Queries that select any real field still authenticate as normal.

#### Checking a connection

`session` answers whether a token still works, and needs no mail to exist to do
it:

```graphql
{ session { status username primaryAccountId capabilities detail } }
```

One `GET /jmap/session` — the handshake Fastmail only completes for a request it
has authenticated. `status` is `CONNECTED`, `INVALID_CREDENTIALS`, or
`UNREACHABLE`, and a bad connection is reported there rather than raised as a
GraphQL error: it is the answer to this question, not a failure to answer it.
The split is the one a UI acts on — tell the user to re-authenticate, or tell
them to wait. A 401 is the only thing that reads as a credential verdict;
everything else (5xx, rate limiting, timeouts, DNS) is `UNREACHABLE`, because
telling someone their token is dead over an outage would be a lie. `detail`
carries the reason in prose for a tooltip; branch on `status`, which won't be
reworded.

It re-runs the handshake rather than reading the cached client — clients are
cached per token for the life of the process, so a cached answer would keep
reporting success long after a revocation, which is the case this exists to
catch. When connected it also reports the accounts the token reaches and the
capability URNs it was granted, which is what says whether masked email and
sending are available at all.

The MCP server exposes **2 tools** via a GraphQL interface:

- **`schema_sdl`** — returns the full GraphQL schema (SDL) so the LLM can discover all available operations
- **`graphql`** — executes any GraphQL query or mutation against the Fastmail API

This replaces the previous 18 individual tools with a composable interface. The LLM fetches the schema once, then constructs exactly the queries it needs — fetching multiple resources in a single round-trip, requesting only the fields it wants, and using typed arguments for filtering and pagination.

### Nested resolution

The object graph is fully navigable, so the LLM can get everything it needs in one hit:

```
Mailbox ──emails──▶ Email ──attachments──▶ Attachment ──base64/image/text──▶
   ▲ │                │ │
   │ └─parent/children┘ ├──thread──▶ Thread ──emails──▶ Email …
   └────mailboxes───────┘
```

Every collection is a Relay connection — `mailboxes`, `identities`,
`maskedEmails`, `contacts`, `attachments`, `Mailbox.children`, `Thread.emails` —
so each takes `first` / `last` / `after` / `before` and exposes `totalCount`,
`pageInfo`, `edges` and `nodes`. Cursors are IDs. Use `nodes` for the items;
`edges` only when you want per-item cursors. Default page 25, max 100 — check
`pageInfo.hasNextPage` rather than assuming you got the lot.

Only `emails` pages server-side, via JMAP's anchors. The rest arrive whole from
one call, so their paging is slicing: `totalCount` is free (a length, not
`calculateTotal`) and a cursor holds only as long as its item is still in the
list — if it goes, you get a "restart pagination" error rather than a quietly
different page. `Thread.emails` returns the same `EmailConnection` as the query
does, with `queryState` null because a conversation is not a query.

Value lists stay plain arrays: `from`, `to`, `cc`, `bcc`, `replyTo`, `sender`,
`keywords`, `mailboxIds`, `headers`, `Contact.emails`. They belong to the parent,
arrive with it, and paging them would be ceremony.

Attachment payloads are three separate fields, each doing the least work that
answers it. Metadata (`name`, `size`, `contentType`, `cid`, …) arrives with the
email and downloads nothing at all:

- `base64` — the raw bytes, base64-encoded. Download only, any type.
- `image(maxBytes:)` — resized then encoded, so a model isn't handed a 10MB
  photo. Null for non-images.
- `text` — extracted document text. **The expensive one**: it parses the whole
  file, is priced far above the other two, and nothing else on `Attachment`
  triggers it. Prefer a small page when selecting it.

```graphql
# Bodies and attachment text for a whole folder — one query, 3 API calls.
# Note the smaller page: `text` parses every document, so a big page means a
# lot of work. Nothing stops you asking for more; the cost is just yours.
{
  emails(filter: { inMailbox: "INBOX" }, first: 10) {
    nodes {
      subject
      from { name email }
      textBody
      attachments {
        nodes { name contentType size cid text }
      }
    }
  }
}

# Walk the graph: folder tree → emails → conversation → the folders they live in
{
  mailbox(name: "INBOX") {
    name
    children { nodes { name unreadEmails } }
    emails(first: 5) {
      nodes {
        subject
        thread { total emails { nodes { subject textBody } } }
        mailboxes { name role }
      }
    }
  }
}
```

### Composable filters

`EmailFilter` mirrors JMAP's filter tree (RFC 8620 §5.5) rather than flattening
it into scalar arguments. Fields on one filter object are AND-ed; `and` / `or` /
`not` nest arbitrarily. The same input type is accepted everywhere emails
appear, including `Mailbox.emails`, where it's AND-ed with the mailbox.

```graphql
# Unread, from either sender, not in Archive, biggest first
{
  emails(
    filter: {
      unread: true
      or:  [{ from: "alice@example.com" }, { from: "bob@example.com" }]
      not: [{ inMailbox: "Archive" }]
    }
    sort: [{ property: SIZE, ascending: false }]
    first: 20
  ) {
    totalCount
    nodes { subject size }
  }
}
```

Mailbox names and roles are resolved to IDs at every depth of the tree in a
single `Mailbox/get`. `collapseThreads: true` returns one email per
conversation, for a threaded view.

The filter mirrors JMAP's `FilterCondition` (RFC 8621 §4.4.1) field for field:
alongside the usual participant/date/size conditions there are
`inMailboxOtherThan`, `hasKeyword` / `notKeyword`, the thread-wide
`allInThreadHaveKeyword` / `someInThreadHaveKeyword` / `noneInThreadHaveKeyword`,
and raw `header` matching (`["List-Id"]` for presence, `["List-Id", "rust-lang"]`
for a value).

Sorting likewise covers the full comparator set — `receivedAt`, `sentAt`, `size`,
`subject`, `from`, `to`, plus the keyword-based `hasKeyword`,
`allInThreadHaveKeyword` and `someInThreadHaveKeyword`, with `collation`. The
keyword comparators require a `keyword` argument and the others reject one;
both are caught before any API call.

### Pagination

Email lists are Relay connections with `edges`/`nodes`, `pageInfo`, `totalCount`,
`position`, and `queryState`.

**Cursors are email IDs**, mapped onto JMAP's `anchor` / `anchorOffset`. This
falls out of what the API already offers, and it's what makes pagination stable:
a positional cursor silently shifts every time mail arrives, so page 2 would
re-show or skip messages. An anchor names a specific message, so the page after
it is the same page whatever else changed. It stays legible for a model
composing the follow-up query too — the cursor is just an ID it has already seen.

```graphql
{
  emails(first: 25, after: "Mabc123") {
    totalCount
    pageInfo { hasNextPage endCursor }
    edges { cursor node { subject } }
  }
}
```

`last` without `before` becomes a negative `position`, which JMAP counts from
the end — "the last N" is one call and never needs a total. `last` with `before`
anchors backwards from the cursor.

The trade for stability is that a cursor can go stale: if its message is deleted
or stops matching the filter, JMAP returns `anchorNotFound`. That surfaces as an
error saying exactly that, and that the fix is to restart pagination. Compare
`queryState` between pages to detect that the result set moved underneath you.

`totalCount` maps to JMAP's `calculateTotal`, which costs the server real work,
so it's only requested when the field is actually selected. The same look-ahead
means a query selecting **only** `totalCount` performs one `Email/query` and
fetches no emails at all:

```graphql
# "How many unread from this sender?" — one call, zero emails transferred
{ emails(filter: { unread: true, from: "alerts@example.com" }) { totalCount } }
```

### Lazy fields and batching

Nothing below a list is fetched eagerly, and every lazy field goes through a
[DataLoader](https://github.com/graphql/dataloader): the resolvers for a list's
elements run concurrently, and their fetches collapse into **one batched API
call** rather than one per element.

| Selection on `emails(first: 25) { nodes { … } }` | JMAP calls |
| ----------------------------------------------- | ---------- |
| `{ totalCount }` alone | 1 (`Email/query`, no emails fetched) |
| `{ subject from { email } }` | 2 (`Email/query` + `Email/get`) |
| `{ subject textBody }` | 3 (+1 batched `Email/get` for all 25 bodies) |
| `{ subject textBody attachments { nodes { name } } }` | 3 (same batch covers both) |
| `{ … attachments { nodes { name cid size } } }` | 3 (metadata downloads nothing) |
| `{ … attachments { nodes { base64 } } }` | 3 + the blob downloads, issued concurrently |
| `{ … mailboxes { name } }` + `mailbox(…)` + a name filter | +1 `Mailbox/get` total, however many ask |

The naive shape — one detail call per email — would be 26. Loader caches are
per request, so referencing the same email or mailbox twice in one query costs
one fetch; nothing is retained between requests where it could go stale.

Because the graph contains cycles (`Email.thread.emails`, `Email.mailboxes.emails`),
nesting is capped at depth 15 — nothing else bounds a cycle. Breadth is **not**
capped. Resolvers declare a cost (a document parse prices far above a download,
nested lists scale with page size) but that cost is guidance for choosing a page
size, surfaced in the field descriptions; it never refuses a query. Being told
"too complex" without being told the threshold just makes a caller guess.

All operations are available as GraphQL queries and mutations: mailboxes, emails, search, threads, identities (with signatures), attachments (with text extraction and image resizing), contacts, masked email management, and send/reply/forward with the preview/confirm safety pattern.

Token can be set via `FASTMAIL_API_TOKEN` env var or config file.

## Debug Logging

Enable debug output with `RUST_LOG`:

```bash
RUST_LOG=debug fastmail list mailboxes
```

## JMAP API

This CLI talks directly to Fastmail's JMAP server — the protocol layer is hand-rolled, there's no JMAP client library in the dependency tree. Capabilities are filtered dynamically based on your API token's permissions — read-only tokens work fine for listing/reading, while send and masked email operations require appropriate capabilities.

For more on JMAP: [jmap.io](https://jmap.io/)

## Development

The pipeline is a [Dagger](https://dagger.io) module in `.dagger`, so CI runs the
same way on a laptop as it does on a runner — GitHub Actions only decides when to
invoke it and hands it credentials.

```bash
dagger check                                   # fmt, clippy, tests
dagger check -l                                # list them
dagger check fastmail-cli:clippy               # just one
dagger call binary --platform=linux/arm64 export --path=./fastmail
dagger call image --platform=linux/arm64 as-tarball export --path=./image.tar
```

There is no Dockerfile. The release binary is the unit of caching: each platform
is compiled once and that file is reused by both the release tarball and the
container image, so a `docker build` rebuilding from source would be a second,
slower source of truth. Dagger pushes the multi-platform manifest itself, so
there are no per-architecture staging tags to assemble afterwards either.

CI runs the engine on Dagger Cloud (`--cloud`). Hosted runners are ephemeral, so
an engine started on one begins every job with an empty layer cache and empty
cache volumes, which defeats the point of caching the binary at all; cloud
volumes are restored at the start of a run and pushed back at the end.

The only module dependency is `dagger/dagger/toolchains/release/gh`, which is
maintained in the Dagger repo and pinned to the same version as the engine. The
Rust modules on Daggerverse are all community-maintained and none handle
cross-compilation, so the cargo invocations are spelled out here instead.

macOS binaries are the exception, and it is a dependency problem rather than a
Dagger one. Cross-compiling to darwin from a Linux container is possible in
principle (`cargo-zigbuild` against a macOS SDK), but `aws-lc-sys` — pulled in by
rustls, via reqwest — does not survive it: it needs macOS-specific compiler flags
and breaks in a different way on each zig and SDK combination. CI builds the mac
tarballs natively on a macOS runner and passes them into `deliver` as an
argument. Moving rustls onto the `ring` backend would likely make an all-Dagger
build viable, at the cost of changing the crypto backend.

## License

MIT

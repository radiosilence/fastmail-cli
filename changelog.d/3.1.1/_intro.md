Released 2026-07-25.

### Fixed

- **The MCP server no longer caps every client to the 2024-11-05 protocol.** It
  declared that version explicitly, and since rmcp negotiates down to the lower
  of the two, the declared version acts as a ceiling — a client supporting
  2025-11-25 was still held at the oldest revision, and anything added since was
  ignored. No version is declared now, so rmcp offers the newest it implements
  and negotiation picks the right one per client.

### Added

- **MCP instructions trimmed by 80%** — ~1,220 tokens to ~240. They had grown
  into a tutorial with worked GraphQL examples, all of which duplicated what
  `schema_sdl` already documents per field. Instructions ride in context for
  every conversation the server is connected to, whether or not it gets used, so
  what stays is only what the schema cannot say for itself: fetch the schema
  first, ask in one nested query rather than looping, collections are
  connections, and always preview before sending.

- **Display titles on the MCP tools.** `graphql` and `schema_sdl` are what a
  client shows in its UI, which reads as "Graphql" and "Schema sdl". They now
  carry `title` — "Fastmail" and "Fastmail schema". Wire names are unchanged, so
  existing prompts and configs keep working. (`title` is a 2025-06-18 field,
  which the protocol cap above was suppressing.)

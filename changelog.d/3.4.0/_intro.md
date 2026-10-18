Released 2026-08-17.

### Added

- **`Session.carddavConfigured`.** Contacts go over CardDAV, which authenticates
  with a username and an app password and rejects the API token everything else
  uses — so `contacts` can fail on a connection where mail works perfectly.
  `capabilities` could never report this: it lists what the JMAP server
  advertises, and CardDAV is a separate protocol the handshake cannot see. The
  only way to discover the gap was to run the query and fail, halfway through a
  plan that assumed contacts were reachable. It reports whether both credentials
  are present, not whether they work, and answers independently of `status`,
  since they are resolved per request and stay knowable when the token is dead.
  Both it and `contacts` read the same resolved value rather than loading the
  config separately, so the flag cannot disagree with the operation it
  describes.

- **`X-Fastmail-Username` and `X-Fastmail-App-Password` headers.** CardDAV needs
  its own credentials, so a hosted deployment could never offer contacts at all
  — one bearer token cannot cover two protocols. They resolve exactly like
  `X-Fastmail-Token`: the request's header first, local config after, each half
  independently, so a header-supplied username is never silently completed with
  the host's own password.

### Changed

- **The `graphql` tool describes everyday mail itself, so most sessions never
  fetch the schema.** The SDL is ~27KB, most of it the doc comments that make it
  worth reading, and it used to be the only way to learn anything — so reading
  mail cost a 27KB fetch first, and cost it again whenever the connection
  dropped. The tool description now carries a slimmed schema: the queries, the
  `EmailFilter` tree, the common `Email` fields, the connection shape and the
  PREVIEW→CONFIRM flow, plus worked examples. It names what it does *not* cover,
  so the remaining cases know to ask rather than guess.

- **`schema_sdl` takes a `types` list**, for those remaining cases:
  `["MutationRoot"]` or `["Attachment", "MaskedEmail"]` is a few hundred bytes
  rather than the lot. Definitions come back whole and documented; the types
  they reference do not, so name those too. An unrecognised name is reported in
  a trailing SDL comment with the names that do exist, rather than returning a
  schema with a hole in it. Omitting `types` still returns everything, and a
  client that sends no arguments at all is unaffected.

  Both halves of that are held down by tests: the worked examples are scraped
  out of the published tool descriptions and executed against the real schema,
  and every field name in the inlined sketch must exist in it. A cheat sheet
  that outlives a rename is worse than no cheat sheet — the first draft of this
  one had already invented a `nonce` field and list-valued recipients.

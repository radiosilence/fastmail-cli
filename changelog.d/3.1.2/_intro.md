Released 2026-07-25.

### Fixed

- **Stale query snippets in the schema's own documentation.** Making every
  collection a connection invalidated the examples embedded in resolver doc
  comments — `attachments { text }` had quietly become
  `attachments { nodes { text } }` — and nothing compiles a doc comment, so they
  went unnoticed. The snippets are gone rather than corrected: they restated
  structure the SDL already describes, which is precisely why they could rot
  without breaking anything. The prose that the SDL _cannot_ express — that
  scalar filter fields AND together, that attachment payloads resolve lazily,
  what each field costs — is what stays. A test now fails if the schema's prose
  names a construct the schema no longer has.

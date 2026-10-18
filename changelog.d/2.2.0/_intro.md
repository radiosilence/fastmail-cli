Released 2026-04-11.

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

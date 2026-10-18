Released 2026-02-27.

### Added

- `--from` flag on send, reply, and forward to choose which identity/alias to send from
- `list identities` command to view available sender identities
- `list_identities` MCP tool
- Identity selection tests (`pick_identity`)

### Changed

- Identity resolution extracted into testable pure function

Thanks to [@bgilly](https://github.com/bgilly) for the original PR (#6).

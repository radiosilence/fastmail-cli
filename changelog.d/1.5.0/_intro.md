Released 2026-01-11.

### Added

- Contacts support via CardDAV (`contacts list`, `contacts search`)
- `search_contacts` MCP tool for Claude to look up email addresses by name
- `FASTMAIL_USERNAME` and `FASTMAIL_APP_PASSWORD` env vars for CardDAV auth

### Notes

- CardDAV requires an app password - Fastmail's API tokens only work for JMAP
- Generate app password at Fastmail Settings > Privacy & Security > Integrations > App passwords

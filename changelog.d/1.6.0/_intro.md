Released 2026-01-11.

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

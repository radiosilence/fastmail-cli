Released 2026-03-20.

### Fixed

- Reply-all no longer silently drops all recipients when sender email is empty string
- Drafts now always attempt identity resolution via `--from` and skip gracefully on failure
- Drafts now receive both `$draft` and `$seen` keywords (previously only `$draft`)

### Changed

- `SendAction` is now a proper enum (`preview`/`confirm`/`draft`) instead of a bare string — improves MCP type safety
- `ComposeParams` struct eliminates `clippy::too_many_arguments` across send/reply/forward; removed all `#[allow]` attributes
- Shared `ComposeContext` helper deduplicates ~50 lines of branching in send/reply/forward
- CLI JSON output now includes `"status": "draft"` or `"status": "sent"` to differentiate results
- MCP preview text for send/reply/forward now mentions `action='draft'` option

Thanks to [@thrawny](https://github.com/thrawny) (Jonas Lergell) for the original PR (#9).

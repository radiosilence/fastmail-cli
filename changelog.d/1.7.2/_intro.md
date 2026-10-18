Released 2026-02-27.

### Fixed

- Read-only API tokens no longer crash with "error decoding response body" — capabilities are filtered against the session
- Send/reply/forward fail fast with actionable error when token lacks submission capability
- Masked email operations fail fast when token lacks maskedemail capability
- Non-JSON API error responses (e.g. 400 from disallowed capabilities) are now surfaced instead of generic parse failures

### Changed

- Capabilities are computed once at authentication, not on every request
- `require_capability` is now generic — used for both submission and masked email checks

Thanks to [@kylehowells](https://github.com/kylehowells) for the original PR (#4).

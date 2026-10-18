Released 2026-04-18.

### Changed

- **NonceStore is now bounded** ([#27](https://github.com/radiosilence/fastmail-cli/pull/27)): 15-minute TTL per nonce and a hard cap of 256 outstanding nonces. Closes [#25](https://github.com/radiosilence/fastmail-cli/issues/25) — follow-up from the v2.2.1 security audit.
- **Dep bumps**: `kreuzberg` 4.4 → 4.8, `toml` 0.8 → 1.1, `rmcp` 0.12 → 1.5, `schemars` 0.8 → 1.2. The rmcp 1.0 `#[non_exhaustive]` model-struct fallout moved `get_info()` onto the builder API (`ServerInfo::new().with_*()` instead of struct literals) — cleaner read, same behaviour.

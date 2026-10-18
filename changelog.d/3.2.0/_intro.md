Released 2026-07-25.

### Changed

- **The binary is now `fastmail`, not `fastmail-cli`.** The `-cli` suffix only
  ever disambiguated the crate from the service; on the command line it was
  noise you typed dozens of times a day. The package, repository and release
  asset names are unchanged, so `cargo install` and the crates.io identity stay
  put — only the installed executable is renamed. Two consequences: shell
  completions must be regenerated, and `mise use -g github:radiosilence/fastmail-cli`
  needs `[exe=fastmail]` appended, since ubi otherwise looks for a binary named
  after the repository. The config directory stays at `~/.config/fastmail-cli/`
  — nobody has to re-authenticate.

# fastmail-cli — CLI and MCP server for Fastmail's JMAP API

# What CI checks: format, lint with warnings as errors, tests, workflow lint.
check:
    actionlint
    cargo fmt -- --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --locked

# Format
fmt:
    cargo fmt

# Install the git hooks
hooks:
    lefthook install

# Write CHANGELOG.md from the fragments in changelog.d/. `--stdout` prints it instead.
changelog *args:
    python3 scripts/changelog.py {{args}}

# Open the release PR: `just release patch|minor|major`, or an exact version.
# Merging it publishes the image and the GitHub release; tags are never pushed by hand.
release version *args:
    scripts/release.sh {{version}} {{args}}

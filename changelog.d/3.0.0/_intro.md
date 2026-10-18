Released 2026-07-24.

Hosted MCP: `fastmail-cli` can now run as a remote MCP backend behind an OAuth
gateway ([`jaritanet-mcp-gateway`](https://github.com/radiosilence/jaritanet-mcp-gateway)),
in addition to the unchanged local stdio mode.

### Added

- **Streamable HTTP transport for the MCP server** (`fastmail-cli mcp --http <addr>`): serves MCP at `/mcp` over HTTP instead of stdio, for remote/multi-tenant hosting. The Fastmail token is resolved **per request** from the `X-Fastmail-Token` header (set by a trusted upstream) rather than baked in at startup; stdio mode is unchanged and still uses the config/env token. Authenticated JMAP clients are cached per token. The raw HTTP transport trusts the header unconditionally and must sit behind an auth layer.
- **Library crate**: the crate now exposes a `lib.rs` surface (JMAP client, GraphQL layer, MCP server) so a hosted service can link the machinery directly instead of shelling out to the binary. The CLI binary is unchanged.
- **Container image** (`ghcr.io/radiosilence/fastmail-cli`): default command runs `mcp --http`, for use as a gateway backend. Multi-arch (amd64/arm64).

### Fixed

- **DNS-rebinding Host allowlist disabled in `--http` mode**: rmcp's streamable-HTTP server rejects a proxied `Host` (e.g. an internal service name) with `403 Forbidden: Host header is not allowed`. Rebinding protection guards browsers hitting a localhost MCP directly — irrelevant for a proxied, non-browser-facing backend where the proxy is the security boundary — so it's disabled for this transport.

### Changed

- Dependency updates via `cargo update` (Cargo.lock only).

### Note

Bumped to a major version because the MCP server gained a new transport and a
public library surface; the stdio CLI behaviour is backward-compatible.

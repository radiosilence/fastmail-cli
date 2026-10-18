Released 2026-07-26.

### Changed

- **Only `--http` mounts `/mcp` now.** The GraphQL surfaces bind the listener
  because there is nowhere to mount an HTTP route over stdio, but they no longer
  drag MCP onto it: the transport a model connects through and a browsable
  endpoint for a human are separate things, and `fastmail mcp --browser` meaning
  "open the IDE" should not also publish a JSON-RPC endpoint nobody asked for.
  `--http` remains what a hosted deployment passes, so the Docker image is
  unaffected. Anyone relying on `--graphql` alone to serve `/mcp` now needs both
  flags. This matches `caldav-cli`, `tfl-mcp` and `mainlynorfolk-mcp`, where the
  surfaces were already independent.

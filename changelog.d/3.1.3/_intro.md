Released 2026-07-25.

### Changed

- **`--graphql`, `--graphiql` and `--browser` no longer require `--http`.** They
  cascade instead: `--browser` implies `--graphiql` implies `--graphql`, and any
  of them starts the HTTP server on `127.0.0.1:8080`. The old `requires` chain
  made you name a transport in order to ask for a surface that only exists on
  that transport, when the address already had a default. `--http` now earns its
  place only when you want a different address.

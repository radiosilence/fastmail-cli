Released 2026-10-13.

### Changed

- **Inline attachments are fingerprinted with SHA-256**
  ([#78](https://github.com/radiosilence/fastmail-cli/pull/78)). The confirmation token
  keyed inline attachment bytes by the standard library's 64-bit hash, whose
  fixed keys put a deliberate collision within reach of a client that chooses
  the bytes. A CONFIRM could then have carried different content under the
  same token.

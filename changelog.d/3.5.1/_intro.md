Released 2026-09-29.

### Changed

- **macOS release binaries are signed with a Developer ID and notarised.** Downloaded through a browser, the ad-hoc signed binaries were quarantined and Gatekeeper refused to run them until the quarantine attribute was cleared.

### Fixed

- **`reply` now prefers `Reply-To` over `From`**
  ([#67](https://github.com/radiosilence/fastmail-cli/issues/67), reported by
  Kendell). Transactional and support senders routinely put a branded address in
  `From` and the inbox that actually receives mail in `Reply-To`; replying to
  `From` then bounces off a domain with no MX record, which is how this was
  found. `Reply-To` replaces `From` rather than joining it — on a reply-all the
  branded address is no more deliverable than it was on a plain reply. An empty
  `Reply-To` list is not a `Reply-To`, and still falls back.

  The CLI and GraphQL's `replyToEmail` share the helper, so both were wrong
  together and are fixed together — including the send preview, which had been
  reporting the address it was about to fail to deliver to.

Released 2026-09-29.

### Added

- **Draft editing over MCP: `updateDraft`, `sendDraft`, `deleteDraft`**
  ([#73](https://github.com/radiosilence/fastmail-cli/pull/73)). A
  draft saved with `action: DRAFT` could not be changed, sent or discarded
  afterwards. JMAP emails are immutable apart from keywords and mailboxes, so
  `updateDraft` saves an edited copy and then destroys the original, in two
  requests so that a failed create never loses the draft; the draft's ID
  changes and the new one is returned. Fields not passed are carried over,
  including threading headers and attachments (by blob, without re-uploading).
  `sendDraft` follows the same PREVIEW → CONFIRM flow as `sendEmail` and files
  the message in Sent on success. `deleteDraft` moves the draft to Trash rather
  than destroying it.

### Fixed

- **CONFIRM tokens now cover every compose parameter**
  ([#73](https://github.com/radiosilence/fastmail-cli/pull/73)). The token issued by
  PREVIEW fingerprinted only `to`/`subject`/`body` (for replies, only the
  email ID and body), so a CONFIRM could add `cc`/`bcc`, change `from` or the
  HTML body, or turn a reply into reply-all without the user having seen it.
  Any difference from the preview is now rejected.

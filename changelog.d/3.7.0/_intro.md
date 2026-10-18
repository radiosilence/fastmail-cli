Released 2026-10-13.

### Added

- **Attachments over MCP**
  ([#76](https://github.com/radiosilence/fastmail-cli/pull/76)). `sendEmail`, `replyToEmail`, `forwardEmail` and
  `updateDraft` take `attachments: [AttachmentInput!]`, each one either inline
  `base64` or the `blobId` of a blob already on the server. The server usually
  runs remotely, so a client cannot name a local file the way `fastmail send -a`
  does. `uploadAttachment` stores a file and returns its `blobId`, so the bytes
  need not be sent with both PREVIEW and CONFIRM. The preview lists each
  attachment's name, type and size, and the confirmation token covers them, so a
  CONFIRM with different attachments is rejected. Attachments are capped at
  25 MB per message.

### Fixed

- **Image attachments are sent with their image type.** The extension-based
  MIME inference behind `fastmail send -a` knew documents only, so a PNG or JPEG
  went out as `application/octet-stream` and mail clients showed it as a file
  rather than an image.

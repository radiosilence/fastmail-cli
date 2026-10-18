Released 2026-04-18.

### Fixed

- **Reply-all preview divergence (B1)** — The MCP `replyToEmail` mutation's PREVIEW path never consulted `reply_all` when building recipients, so calling it with `all: true` showed only the original sender in `To` and whatever the user explicitly passed as `cc`. Meanwhile the send path in `reply_email` expanded reply-all correctly. This had two knock-on effects: (1) the preview lied about who would actually receive the email, and (2) a user who "fixed" the under-reported preview by passing missing recipients as explicit `cc` could produce a duplicate-send, because those same addresses would also be expanded into `To` by the send path at CONFIRM time. Extracted `jmap::expand_reply_recipients` as a shared pure function used by both preview and send; the function now also deduplicates by lowercase email and strips from `Cc` anything already present in `To`, closing the duplicate-send window regardless of how the paths evolve. 9 unit tests cover reply-all expansion, me-filtering (case-insensitive), dedup, and the exact overlap scenario from the bug report.

### Security

- **Attachment path traversal (C1)** — `fastmail-cli download` wrote attachments to `Path::new(out_dir).join(attachment.name)`, where `attachment.name` is chosen by the email sender. A name of `../../etc/cron.d/pwn` escaped the output directory via relative traversal; an absolute name like `/etc/cron.d/pwn` replaced the base path outright because `Path::join` discards the base when the joined segment is absolute. A malicious email could write arbitrary files on any recipient who ran the `download` subcommand. Filenames are now run through `util::sanitize_filename`, which strips path separators, NUL/control bytes, and Windows-reserved stems (CON/PRN/NUL/COM1-9/LPT1-9). Writes use `OpenOptions::create_new(true)`, so silent overwrites and symlink-pre-placement attacks at the target path are also refused.
- **CardDAV URL injection (C2)** — `list_addressbooks()` interpolated the raw username into `/dav/addressbooks/user/{}/` without percent-encoding. Misconfigured usernames containing `/`, `?`, `#`, or `%` produced malformed URLs that could target a different CardDAV endpoint. Now percent-encoded with an explicit path-segment set.
- **Token file TOCTOU (H1)** — `Config::save()` ran `fs::write(path, token)` followed by `fs::set_permissions(0o600)`, leaving a window where the token file was readable under the default umask. The write is now atomic: the token is written to a sibling `.tmp` file opened with `OpenOptions::mode(0o600).create_new(true)`, then `rename()`d over the target. The parent directory is created with `DirBuilder::mode(0o700)`.
- **Symlinked config path (H2)** — `fs::write` followed symlinks at the config file path. A hostile program with write access to `~/.config/fastmail-cli/` could pre-place a symlink redirecting the token write. `save()` now checks `symlink_metadata()` and refuses to write if the target is a symlink.
- **Token in argv (H3)** — `fastmail-cli auth YOUR_TOKEN` exposed the token to `ps`, shell history, and the process environment. The token argument is now optional; when omitted it is read from stdin (with a TTY prompt). The positional form is retained for backward compatibility.
- **URL template substitution bleed (M1)** — `download_blob` and `upload_blob` built URLs by chaining `str::replace`, which would recursively substitute a template-like value into a later placeholder. Replaced with a single-pass `apply_url_template` helper. Defense-in-depth — no live bug, all current inputs are trusted — but it future-proofs the code against trust-boundary changes.
- **Stateless compose confirmation (M3)** — The MCP `sendEmail` / `replyToEmail` / `forwardEmail` PREVIEW→CONFIRM flow used a `DefaultHasher` of the params as the confirmation token, which was a signature rather than a nonce — any caller who knew the params could produce a valid token without ever calling PREVIEW. Replaced with a random UUIDv4 nonce issued on PREVIEW, stored server-side, and consumed one-shot on CONFIRM/DRAFT with a params-fingerprint check so tampering between PREVIEW and CONFIRM is detected.
- **`InvalidToken` variant footgun (M4)** — The variant held `String`, inviting future contributors to embed the actual token in the error payload for "better debug output". Narrowed to `&'static str` so only compile-time literals can be passed.
- **`rustls-webpki` name-constraint bypass** — Transitive upgrade to 0.103.12 via dependency bumps, fixing [RUSTSEC-2026-0098](https://rustsec.org/advisories/RUSTSEC-2026-0098) and [RUSTSEC-2026-0099](https://rustsec.org/advisories/RUSTSEC-2026-0099).

### Fixed

- **Reply-all preview divergence (B1)** — The MCP `replyToEmail` PREVIEW path computed recipients differently from the actual send path, under-reporting who would be emailed. The natural workaround of sending twice could deliver duplicate emails. Extracted `expand_reply_recipients` as a shared pure function with dedupe-by-lowercase-email; preview and send now share one recipient list.

### Changed

- `auth` CLI arg is now `Option<String>` (backward compatible — the positional form still works).

Security audit contributed by [@dylanbyars](https://github.com/dylanbyars) ([#23](https://github.com/radiosilence/fastmail-cli/pull/23)).

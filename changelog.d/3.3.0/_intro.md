Released 2026-07-26.

### Added

- **`session` — is this token actually good?** Nothing on the graph answered
  that. Every field needs auth, so a dead token surfaced as whatever error the
  selected field happened to produce, and an empty result read the same as a
  revoked credential. `session` re-runs the JMAP handshake rather than reporting
  the cached one — clients are cached per token for the life of the process, so
  a cached answer would keep claiming success long after a revocation — and
  reports `CONNECTED` / `INVALID_CREDENTIALS` / `UNREACHABLE` as data rather
  than raising an error: not being connected is the answer to this question, not
  a failure to answer it. Only a 401 counts as a credential verdict; 5xx, rate
  limiting and timeouts are `UNREACHABLE`, since a caller that can't tell an
  outage from a revocation ends up telling users to re-authenticate through
  someone else's downtime. `detail` carries the reason in prose for a tooltip;
  the enum is what to branch on. When connected it also returns the username,
  the accounts the token reaches and the capability URNs it was granted — the
  only way to know whether masked email or submission are usable before trying
  them. One API call, no mail touched. Matches `caldav-cli`'s `viewer`, minus
  the rename: the field keeps its own name because that is what the gateway
  already queries.

### Fixed

- **A stale `Cargo.lock` no longer reaches the image build.** `check` builds and
  tests with `--locked`, so a lockfile that has drifted from `Cargo.toml` fails
  in the pull request rather than in the Docker build, which was the only step
  using `--locked` and so the only one that noticed.
- **A version tag is never cut without an image behind it.** `publish` now waits
  for the image builds as well as the tarballs. Previously they ran in parallel,
  so a failed image build still produced a GitHub release with nothing to pull.

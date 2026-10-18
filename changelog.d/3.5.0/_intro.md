Released 2026-08-28.

### Added

- **`fastmail watch`** ([#65](https://github.com/radiosilence/fastmail-cli/pull/65),
  requested by [@davidcelis](https://github.com/davidcelis) in
  [#64](https://github.com/radiosilence/fastmail-cli/issues/64)). Blocks and
  emits one JSON object per line as mail arrives, so incoming email can drive a
  shell loop instead of a cron job that re-lists the inbox and diffs it by hand. `--mailbox` narrows to one folder,
  `--full` includes bodies, and `--poll <seconds>` swaps the push connection for
  periodic checks where a long-lived one will not survive the network.

  JMAP has offered the transport all along: the session advertises an
  `eventSourceUrl` (RFC 8620 §7.3) that we parsed and threw away. The design
  decision worth knowing is that push is only ever a wake-up. The `Email` state
  cursor lives in the CLI, and every notification — or poll tick, or reconnect —
  runs `Email/changes` against it, so all three paths converge on the same
  answer and a lost notification costs latency rather than mail. When the server
  has discarded history back past the cursor, the watcher resyncs and says so on
  stderr rather than replaying the mailbox as new; stdout stays pure NDJSON
  either way.

  Only creations are reported. Reporting updates too would replay every flag
  change and folder move as an arrival, which is not what a mail loop means by
  "new".

- **The `emails` GraphQL subscription**
  ([#65](https://github.com/radiosilence/fastmail-cli/pull/65)), at
  `/graphql/stream` over Server-Sent Events when the HTTP surface is up. Same
  watcher as `fastmail watch`, so the cursor semantics are identical rather than
  merely similar — the CLI and the subscription are two front ends on one
  implementation, which is the only way they stay that way.

  SSE rather than WebSockets: the subscription is a server-to-client firehose,
  nothing is ever sent back up the socket, and SSE reconnects on its own.

  MCP deliberately has none of this. Tools are request/response and a
  subscription never returns, so the `graphql` tool's description says so and
  points at the other two ways to get it.

# Provider disconnect evidence

A lost connection does not establish whether the provider completed a request.
Keep the existing unknown-outcome fence and do not automatically resend a
possibly executed submission.

## Ownership

- `provider::acp` owns ACP framing, pending RPC correlation, and the first
  terminal transport error. Grok and OpenCode use this same transport. EOF,
  I/O error kind, protocol violations, response timeouts, and local abandonment
  remain distinguishable. Later cleanup cannot replace the first error.
- `provider::codex` owns its WebSocket failure. Pending RPCs and notification
  consumers consult the same failure owner. The reader lifetime guard is
  constructed before spawning and owns both pending RPC retirement and the
  notification sender. Normal return, panic, and cancellation before the first
  poll all preserve the cause before waking either group of consumers.
- The execution owner supplies process state and exit status. Engine stdio logs
  terminal output observations by execution ID before provider cleanup, including
  output continuity failures. Pipe EOF alone is never interpreted as process exit.
- Server session code correlates the transport error with provider, worker,
  turn, and execution identity before cleanup. It does not parse provider logs or
  infer execution policy from provider names.

## Completion and notification ordering

ACP setup, prompts, and compaction drain notifications concurrently with the
pending RPC. When the notification stream closes, disable that receive branch
and resolve the RPC owner. A closed notification channel must not hide an
already queued result or replace a specific protocol error with a generic close.
Codex notification consumers drain queued notifications before returning the
retained transport failure.

## Incident inspection

Start with the worker and turn IDs from the ACP failure log. Use its execution
ID to find the output observation from before cleanup; distinguish that evidence
from later process settlement caused by cleanup. For shared Codex connections,
the forwarder records the backend ID and retained connection failure. Historical
unknown receipts remain unknown; the added evidence is in service logs, not a
new database field.

New diagnostics contain lifecycle metadata and error categories, without raw
protocol frames, stdout, stderr, peer close text, or credentials. A missing exit
status remains unknown. This change cannot reconstruct details already discarded
by an older build.

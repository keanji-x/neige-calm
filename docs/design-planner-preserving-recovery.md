# Resume a failed conversation without resetting it

## Outcome

An explicit human message resumes a conversation stranded by a provider
`systemError`, including a persisted pre-upgrade `failed / wedged` session.
The card, worker-session ID, Codex thread ID, transcript, pending-entry IDs,
attachments, and replay watermark are preserved. No `thread/start`, transcript
clear, or implicit replay of a completed failed turn is allowed.

## Boundaries

- Only human `POST /planner/input` may recover a failed session. Ordinary
  boot recovery and machine-authored input do not retry exhausted quota.
- The exception is narrowly `failed`, snapshot `wedged / system_error`, current
  card/session ownership, no completion or queue-harvest marker, a valid snapshot
  and existing thread. Superseded, exited, corrupt, interrupt-timeout, deleted,
  terminal-track, and unavailable-workspace sessions remain refused.
- Serialize with the existing per-card recovery/reset lock and track deletion
  fence. Quiesce through the existing run-loop command channel, so ordinary
  deliveries, queue mutations, notifications and snapshotting cannot interleave.
  Stop an old failed harness before installing its replacement; recheck
  ownership in the restoring transaction. The general worker transition matrix
  remains unchanged.
- Resume the exact provider thread before restoring its active status. Reject
  active or missing threads. On cold load, refresh the same card's credentials
  while its session is still failed. Creation and both recovery paths share one
  profile capability policy: plain chats remain without MCP credentials;
  planners and assistants retain their own roles. A lost RPC response remains
  retryable using that persisted token. Loaded threads retain their existing credentials.
- Provider recovery must never fall back to starting a new thread. A provider
  outage/refusal leaves the failed snapshot and all conversation data intact.
- Record a matching failed turn completion even when `systemError` arrived first.
  Keep the harness blocked until a human sends again; show the provider error in
  the existing transcript and make the recoverable state visible on planner/run.
  Live notifications and provider-history backfill use the same idempotent
  outcome store. The UI hint and restoring write share one eligibility predicate;
  schema constants belong to the shared persistence vocabulary.
- Existing snapshot schema and migrations are unchanged. Recovery edits only
  phase/reason and the active-state fields in the current row, under an exact
  snapshot comparison. A crash after that commit is covered by normal boot/lazy
  recovery.

## Acceptance

1. Reproduce `systemError` then matching quota-failed `turn/completed`; retain the
   error, reject stale completions, and issue no automatic retry.
2. Send against a persisted failed session with no registry entry: same session
   and thread, retained transcript/queue/attachments, exactly one accepted send.
3. Exercise the same operation while the failed harness is still registered,
   concurrent sends, provider failure, reset/supersession/deletion races, and
   non-human input. No data loss, duplicate harness, or authority resurrection.
4. Verify the actual provider `thread/read` and `thread/resume` wire calls for hot
   and cold recovery with fake RPC transport; no real Codex on the shared host.
5. Run focused regressions, mutation-check preservation/ownership assertions,
   quick Rust gates, and two independent reviews in isolated copies.

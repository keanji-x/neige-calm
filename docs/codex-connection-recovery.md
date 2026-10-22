# Shared Codex connection recovery (#3039)

Review tier: L2. Connection replacement crosses native-source and approval routing
boundaries; unknown prompt delivery must remain unknown across reconnection.

## Evidence and outcome

An isolated Codex 0.163.0 with a private CODEX_HOME and a copy of the backed-up
rollout reproduced the failure through the unmodified production Rust client.
No model turn, production socket, credentials, or production-home writes were used.
The client logged `Space limit exceeded: Message too long: 17349300 > 16777216`,
then returned `thread/resume: connection closed before response`; the next
`model/list` returned the misleading incomplete-write poison diagnostic.
An unrestricted measurement observed one complete text frame of 17,348,935 bytes
(the precise response size varies with the resume configuration). Resume and the
following model query succeeded. The trigger is the default 16 MiB frame bound,
not corrupt JSONL, process exit, or the 64 MiB complete-message bound.

## Ownership and invariants

- Provider sets the frame bound equal to its existing bounded 64 MiB message
  limit and retains the first transport failure before dropping pending requests.
- Shared daemon owner replaces only a failed connection, initializes it, reuses
  approval routes, publishes the actual new native source, and preserves the
  owned process, watcher and PID identity. No RPC is retried by the transport.
- Running owns one process-bound pending replay plan (ColdRespawn or HotTakeover)
  until completion. Connection replacement preserves its prepared configurations
  and failed-candidate exclusions; process replacement discards them. Completed
  replay releases cached credentials and cannot be cold-replayed a second time.
- The lock order is recovery coordinator, process transition, then per-thread
  replay. Restoration retains the process boundary, and each resume RPC budgets
  its complete send and response. Core is never held across I/O.
- Exact-thread operations obtain a ready connection before entering lifecycle
  locks, revalidate and pin it inside, and never reconnect within the callback.
  Mid-scope EOF fails on that original source; it does not change sources.
- Automatic replay revalidates attribution under the existing deletion fence.
  It keeps canonical recovery in its existing owner and uses the existing cold
  authorization. Prepared cold configs are retained before sending and revalidated
  against current card/thread/runtime authority on every attempt, including
  active-versus-failed state, rather than rotating tokens again after cancellation.
- A replay candidate that closes the connection is skipped for that recovery
  pass, with a warning and its mapping intact. Earlier healthy subscriptions
  are restored on the final connection; the number of candidate exclusions is
  finite. Explicit recovery can still address the failed thread.
- Remote execution is never declared stopped because a connection died. Existing
  native uncertainty and deletion safeguards remain authoritative.

## Acceptance and verification

First demonstrate red regressions for the observed oversized frame and for
connection recovery. Then verify normal and oversized-history responses, bounded
oversized rejection with the actual cause, one initialized replacement shared by
concurrent callers, no replay of unknown turn/start, and automatic replay of
healthy threads before and after a connection-closing candidate. Exercise new
worker-thread mint and deletion cleanup, plus existing native-source/control and
partial-write cancellation tests. Mutation-check unknown-input non-replay and
candidate exclusion assertions in the exclusive worktree. Run required text,
contract, and quick Rust gates; CI owns broad suites and real Codex E2E stays off.

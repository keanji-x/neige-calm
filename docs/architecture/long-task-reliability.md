# Long task execution reliability

Status: directory and report-admission fixes merged in
[PR #1490](https://github.com/keanji-x/neige-calm/pull/1490), 2026-09-05.
The delivery scope below separates shipped behavior from checkpoint/recovery
follow-ups. For UI instructions, see [Using Neige Calm](../using-neige-calm.md).

Outcome: a long task's evidence, verification directory, and terminal outcome
remain attributable to the execution that produced them, including failure.

Source-level findings before the fix (historical diagnosis):

- The deployment fallback for the per-Track task budget is one. This is a policy
  limit, not a failure to discover independent tasks. Settings → General already
  supports a live default override (#1470); Track-specific and server limits
  still apply. This repair did not change the concurrency default.
- Gate preparation ignores the worker's durable workspace lease and falls back
  to the declaration or Track workspace.
- Worker reports tolerate a losing terminal CAS and append contradictory events.
  The existing lifecycle contract requires conflicts to be rejected instead.
- Claude workers acquire plain empty directories, unlike Codex worktrees.
- The running deadline is a wall-clock limit, not an inactivity detector;
  expiration persists no structured recovery evidence before cleanup.
- Report projection concatenates byte-exact block text. Safe block insertion
  must preserve that round-trip contract rather than alter the projection.

Acceptance boundaries:

1. Gates use explicit gate cwd when provided, otherwise the bound execution's
   persisted cwd before declaration/Track defaults. Missing bound workspace is
   an infrastructure error, never permission to inspect a different checkout.
   Freeze the selected cwd and expose it with the gate evidence.
2. Worker ownership is checked for every task state. Opposite terminal reports
   fail atomically without an event or lifecycle change. Genuine repeats are
   idempotent. A scheduled worker cannot report under an unrelated/card key.
   Gate success and worker success retain their distinct authority.
3. Timeout cleanup must preserve structured recovery evidence and never claim
   that an output fragment is a completed result. Partial acceptance must retain
   gate requirements and distinguish the failed attempt from an accepted result.

Verification uses real MCP report entry points, the real gate adapter and shell,
and scheduler timeout entry points. No real Codex E2E runs on this host. Existing
migrations and user worktrees remain untouched. Review the complete diff through
two isolated independent channels after tests, and repeat after fixes.

## Delivery scope

The user selected directory and state fixes for this change. Checkpoint storage,
attempt-aware resume/partial acceptance, concurrency defaults, old-card folding,
and report insertion separators remain follow-up work. Existing failed attempts
are not rewritten; only new report admission changes. A normal gate completion
still follows a worker success in its own transaction because gate execution is
an external process, but each transition and its events remain atomic.

The UI separately displays worker cwd and the gate's frozen cwd (including
explicit overrides). Gate preparation records its display cache and CardUpdated
event in the same transaction; execution never reads that mutable display cache.
`gate_result.cwd` retains the independently frozen evidence directory. Both providers receive the exact completion task ID. Claude's new operations acquire
Git worktrees and publish readiness only after provisioning. Frozen pre-upgrade
Claude operations keep their original directory on recovery.

Checkpoint follow-up acceptance: persist task/attempt/session identity, exact
checkout and revision, completed work, remaining work, artifact references and
validation results before cleanup. Present a resumable attempt under the same
logical task and collapse superseded attempts without deleting their evidence.
Partial acceptance must explicitly state its scope and run applicable gates;
never reinterpret timeout as success or revive a terminal attempt in place.


## Ownership change request

Approved by the coordinating agent for this user-requested repair: register only
`fe/web/src/ui/path-label` in `fe/module-file-inventory.yaml`. The primitive owns
no domain data or lifecycle and introduces no global styles. Existing ownership
entries and frozen interfaces are unchanged. The complete diff is independently
reviewed. A future commit touching this frozen inventory must carry its exact
`OWNERSHIP-CHANGE` trailer under the repository's contribution rules.

<!-- archived review text, round 2, doc @ 56043881b -->

# Round 2 / channel B — verdict: REVISE
## Wrong facts or false §11 dispositions (file:line → correct)
Paths below: `D` = `docs/architecture/1722-track-activity-indicators.md`; `S` = `crates/calm-server/src`; `T` = `crates/calm-truth/src`.

- `D:412` → recomputing a maximum cannot recover overwritten evidence or tracks excluded from enumeration. B-M3 is not fully resolved.
- `D:410` → `current_tasks` excludes obsolete task failures, but §4.2 independently reintroduces their failed worker sessions.
- `D:305`, `D:366` → closing a preserved PTY does not necessarily invoke fix 2: its exit writer emits no session-status event, and the post-reboot FSM map is empty.
- `D:381` → disabling the feeder does not make existing rows gray; persisted `active`/waiting statuses survive.
- `D:49` → `harness/mod.rs:866,1065` are inside the test module beginning at `:677`, not production writers.
- `D:390` → this exact commit contains A-/B-labeled dispositions, not R1–R12; `D:415` references an undefined R4. I reviewed the actual dispositions without opening round-1 archives.

Cleared: SQL column names and `current_tasks` match migrations 0045/0053/0081/0097. Autocommit reads plus IMMEDIATE writes satisfy `deferred_write_tx_invariant.rs:20,76`; mixed snapshots remain the declared limitation.
Cleared: WEB 28→29/API 8→9 and unchanged SYNC 20 fit the version rules and preflight. 0110 is next, subject to numbering last; an old binary will refuse that migrated database (`T/db/sqlite/infra.rs:77`), so rollback requires database restoration.
Backend constructions: harness working/input, none found within the stated registry/state contract; terminal, none found under the intentional no-turn policy. Other rows fail below.
Completion checks: persisted E1 survives projector crash/lag; interrupted E1 is excluded, refused `turn/start` creates no outcome. E2 filtering and E4/E7 casing match code (`calm-types/src/ids.rs:32`, `event.rs:115`). Pruning preserves an already-folded high-water, not uncaptured evidence.
FSM fixes 1/3 clear the stated `Stop → idle_prompt → SubagentStop` construction after the debounce, assuming the proposed mappings; this was source reasoning, not an executed regression.
Verification: both read-only lockstep scripts passed on the baseline, WEB 28/SYNC 20. Worktree remained clean; no files written or application test suites run.

## MAJOR-1: Database identity is not constrained to one row
Where: `D:242`; §11 B-Q1 at `D:422`.
Construction: boot inserts UUID A; the next boot inserts UUID B. Both satisfy `id TEXT PRIMARY KEY`, so `INSERT OR IGNORE` ignores neither. Reading “the” identity is underspecified and can rotate receipt scope.
Required change: define a fixed singleton key with a separate immutable UUID, initialize/read it atomically, and test repeated and concurrent initialization.

## MAJOR-2: Shared-daemon hints cannot provide the promised reconciliation
Where: `D:149,166,213`; `S/liveness_feeder.rs:79,106,118`; `S/reaper/mod.rs:211`.
Construction: persist `active`, then lose the final idle notification through feeder lag or its ignored write error. Every activity tick rereads `active`, so working persists indefinitely. The reaper’s busy pre-gate also skips authoritative death checking. Conversely, losing a waiting notification hides attention.
Required change: specify authoritative reconciliation/freshness for shared-thread status, including feeder failure and reconnect. Reading the same stale column every 30 seconds is not recovery; correct G5/G9 and the §11 disposition.

## MAJOR-3: E6 is neither completion history nor an active-to-idle edge
Where: `D:183–189`; `T/db/sqlite/session_row.rs:402`; §11 B-M3 at `D:412`.
Construction: between projection scans, turn A completes and stamps idle at t1; turn B starts and overwrites both columns at t2. E6 loses A permanently, even after reboot. Conversely, initial/repeated idle or idle after interruption qualifies without a successful completion.
Required change: persist qualified completion evidence or a separate monotone completion timestamp at ingestion. Define interruption and duplicate-idle behavior; test idle→active between scans and crash before projection.

## MAJOR-4: Tick enumeration can permanently miss quiet completions
Where: `D:202–213`; §11 B-M9 at `D:418`.
Construction: a quiet track’s short execution finishes between ticks; its wakeups are lost. All sessions are now exited/superseded, tasks done, lifecycle done, and its old overlay remains quiet. Every tick predicate is false despite durable completion evidence. Startup scanning only repairs this after another restart.
Required change: enumerate unconsumed durable changes independently of current activity, or periodically scan all eligible tracks. Cover a complete start/finish interval lost before any working overlay was published.

## MAJOR-5: Historical session failures survive task recovery
Where: `D:148–151,159`; §11 B-M1 at `D:410`.
Construction: reaper marks attempt A’s session failed (`S/reaper/mod.rs:363`). Recovery allocates B and rebuilds task projections without clearing A’s session (`S/task_recovery.rs:215–251`). B succeeds, but §4.2 still finds A’s `state='failed'` and keeps the track red.
Required change: define current card/session and current task-attempt ownership for actionable session evidence. Test recovery with the real failed worker session retained, not only task rows. Apply active/current eligibility to shared-daemon waiting/systemError evidence too.

## MAJOR-6: Fix 2 misses actual PTY exits and strands attention
Where: `D:151,232,305`; `S/terminal_renderer/attach_reader.rs:133`; `S/terminal_sweeper.rs:92–119`.
Construction: a tracked Claude card reaches AwaitingInput, then exits without an accepted SessionEnd hook. The production exit path updates the session through `write_in_tx_typed` but emits no `WorkerSessionStatusChanged`. Fix 2 never runs; §4.2’s attention predicate lacks a live-session guard, so every tick preserves input attention.
Required change: reconcile terminal sessions into the FSM or eventize the actual exit boundary, and exclude terminal-session input evidence. Correct the runbook’s promised Done transition while preserving the settled no-backfill decision.

## MAJOR-7: Native session UUID reuse defeats fix 4
Where: `D:234`; §11 B-M5 at `D:414`.
Construction: Claude restart creates worker session s2 using s1’s native UUID (`S/operation/claude_restart_adapter.rs:171,223–234`). A delayed s1 hook carrying that UUID resolves to active s2 (`T/db/sqlite/session_projection.rs:124`). Its actor is therefore `AiClaudeSession(s2)` and passes the proposed fence: old Stop hides new work; old PostToolUse resurrects idle work.
Required change: bind hook provenance to the kernel execution generation, not merely the resumable native UUID. Also fence terminal-event handling against the current generation. This failure does not require the explicitly deferred timer fix.

## MAJOR-8: Isolated running means acknowledged execution, not current turn activity
Where: `D:150`; `S/isolated_codex/journal.rs:176,214`; `S/isolated_codex/observe.rs:85–114,335`.
Construction: an isolated worker reports success and its turn completes, but boundary cleanup cannot prove quiescence. `stop()` returns before changing the session to exited; it remains running. The shared reaper deliberately skips isolated cards (`S/reaper/mod.rs:120`). The proposed projector continues working although the turn ended.
Required change: distinguish durable turn completion from execution cleanup ownership. Do not falsely terminalize the session to clear the indicator; add completion/cleanup-pending coverage.

## MINOR-1: Recovery guarantees and deferred-timer consequences are overstated
Where: `D:166,234,382`.
Construction: E1 insertion failure is deliberately swallowed before phase persistence (`S/harness/run_loop.rs:4986`); E3 can disappear from `current_tasks` before being folded when recovery allocates a successor. Neither is recovered by a later maximum. Separately, an unfenced pending downgrade can cross a session replacement; G10 describes only same-session late hooks.
Required change: qualify recovery guarantees, document these residual losses and cross-generation timer effects, and correct the provenance/ruling references above. The settled timer deferral need not be reopened.

## Open-question answers (§8, one line each)
Q1: Keep one server-clock baseline per device/database identity; MAJOR-1 must establish a genuine stable singleton.
Q2: Keep option (c), no backfill; correct the exit/runbook claims identified in MAJOR-6.
Q3: Keep the notification whitelist; excluded notifications do not themselves clear an already stranded state.
Q4: Keep starting excluded; no additional working/input counterexample found for the harness predicate.
Q5: Keep the shared indicator primitive and device-size signoff; no kernel objection.
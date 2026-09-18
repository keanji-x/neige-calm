<!-- archived review text, round 5, doc @ b2bebf116 (verbatim; no #1316 ratchet substitutions needed) -->

# Round 5 / channel B — verdict: REVISE

## Wrong facts or false §11 dispositions (file:line → correct)

`D` = [docs/architecture/1722-track-activity-indicators.md](/mnt/data2/kenji/neige-calm/.claude/worktrees/1722-activity-design/docs/architecture/1722-track-activity-indicators.md), reviewed at `b2bebf116`.

- `D:189,480,585` → A done task does **not** prove the current session’s failure belongs to that completed execution. A replacement session can fail independently. G19’s closure misses this construction.
- `D:165,170,481,584` → A child-track parent row is not always merely orchestration: after child completion it can become `verifying` and execute its **own** gate. The blanket exclusion hides that work.
- `D:189,585` → `claude_adapter/mod.rs:304-309` constructs the interactive-create command, not the worker command. The worker claim is true, but its evidence is `:362-382`, called at `:798-804`.

Read-only checks: applied existing migrations plus proposed DDL in memory. E1/E2 use the proposed composite index; S0’s isolated lookup uses `operations_target_idx`. Singleton rejection and monotone success-only timestamps behaved as specified. Existing WEB 28 and SYNC 20 lockstep gates passed; proposed WEB 29/API 9 and rollback behavior match the cited code. Autocommit reads satisfy the deferred-transaction invariant. No files changed; Rust tests were inspected, not executed.

## MAJOR-1: G19 hides failure of a replacement session; where; construction; required change

**Where:** `D:189,204,396,480,585`.

**Construction:** Task T finishes on worker card C. Its original session exits. The user restarts C:
- `claude_restart_adapter.rs:223-241` mints S2 without changing T; `session_mirror.rs:282-289` makes S2 current.
- Replacement spawn fails; compensation writes S2 `failed` (`claude_restart_adapter.rs:493-498`).
- This failure path already has coverage in `tests/claude_card_endpoint.rs:886-931`.
- S0 admits S2 through C’s current done task. G19 suppresses its only failure evidence; the live-session gate excludes FSM evidence. After reading the earlier result, the track/card remain quiet despite the failed restart.

The in-memory construction returned `eligible_current_worker=1`, `all_current_tasks_done=1`, `v5_failed=0`. This is a new execution failure, not cleanup of completed work.

**Required change:** Restrict suppression to the session proven to belong to the completed execution, not every replacement session on its card. Add a task-bound restart-failure regression while retaining the original post-completion cleanup and live-prompt tests.

## MAJOR-2: Child-track exclusion hides the parent’s verification gate; where; construction; required change

**Where:** `D:164-170,390,481,584`.

**Construction:** Parent P has a child-track task with `gate_json`. Child finishes quiescently while P’s planner is idle:
- `scheduler/mod.rs:910-922` moves the parent task to `verifying`.
- `guarded_child_success_flip_tx` retains `child_track_id` and leaves `worker_card_id=NULL` (`:423-430`).
- P’s scheduler runs that gate (`:1052-1059`); `tests/scheduler.rs:8051-8077` explicitly verifies this production path.
- With a long-running gate, v5 W remains false because `child_track_id` is non-null. Neither P nor its completed child supplies a working indicator.

The in-memory construction returned `status=verifying`, `v5_working=0`. G20’s deferred child-activity propagation does not cover work executing on P itself.

**Required change:** Preserve `verifying → working` regardless of `child_track_id`; exclude child orchestration only from the applicable dispatched/running cases. Add a parent-gate regression alongside the existing idle-child and failed-child cases.

## MINOR-1: Worker-command citation targets the wrong producer; where; construction; required change

**Where:** `D:189,585`.

**Construction:** Following the cited `claude_adapter/mod.rs:304-309` reaches interactive card creation; worker preparation calls a separate builder.

**Required change:** Cite `claude_adapter/mod.rs:362-382,798-804`. The no-`-p` conclusion remains valid.

## Open-question answers (§8, one line each)

Q1: Closed; stable database identity and one baseline per device/database are coherent.
Q2: Closed; retain projection-only cleanup and the documented exit-versus-signal distinction.
Q3: Closed; retain the whitelist and no-op background hooks; no additional kernel finding.
Q4: Closed; harness `starting` remains non-working, with registry membership required for `turn_pending`.
Q5: Closed; reuse the primitive, leaving dimensions to visual signoff.
G19: Not fully closed; preserve the narrow intent, but address replacement-session failures in MAJOR-1.
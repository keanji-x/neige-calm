<!-- archived review text, round 1, doc @ e634a0f8 -->

# Round 1 / channel B — verdict: REVISE
## Wrong facts (file:line → correct)
Reviewed `e634a0f8`; worktree unchanged. Below, `D` means `docs/architecture/1722-track-activity-indicators.md`.
Checked F2.1–F2.24. The worker-session/overlay SQL column names are valid; the principal SQL defect is historical-versus-current task semantics.
- F2.4, `crates/calm-server/src/operation/terminal_adapter.rs:550` → Claude writes Running at `crates/calm-server/src/operation/claude_adapter/mod.rs:1291`; Codex also writes it at `crates/calm-server/src/operation/codex_adapter/mod.rs:1801` and `crates/calm-server/src/pending_codex_threads.rs:420`.
- F2.4/F2.5, `crates/calm-server/src/worker_flow/mod.rs:585` / `crates/calm-truth/src/db/sqlite/session_mirror.rs:131` → persisted mint/re-arm liveness is **Unknown**, from `session_mirror.rs:75,157`. The status-derived value is a temporary capture object, consumed at `worker_flow/mod.rs:411,427`, not a DB mint.
- F2.7 ordering is correct: `run_loop.rs:2420 → 2425` and `2472 → 2477`. However, “durable completion evidence” needs qualification: `crates/calm-server/src/harness/run_loop.rs:4986` explicitly makes outcome persistence best-effort.
- F2.22’s `0041_tasks.sql` citation misses the subsequent attempt model: `crates/calm-truth/migrations/0097_task_attempt_allocations.sql:103` defines current allocations and `:107` defines `current_tasks`.
§4.4 satisfies the deferred-transaction guard (`crates/calm-server/tests/cases/deferred_write_tx_invariant.rs:20`). Autocommit reads plus an IMMEDIATE write are permitted; they do not provide a shared read snapshot.
Both lockstep scripts passed unchanged: WEB=28, SYNC=20. No runtime tests or mutations were run.

## MAJOR-1: Retried failures remain permanently actionable; D:158–160
Construction: attempt A fails; recovery allocates B, which succeeds. A remains failed in `tasks` (`crates/calm-truth/src/db/sqlite/task_attempt.rs:217`). A2 still selects A; its historical card’s failed session can independently keep the track red.
Required change: select current failed attempts through `current_tasks`; exclude superseded task-attempt cards/sessions from actionable failures. Pin §7 C3/C4 through the real recovery path.

## MAJOR-2: Working coverage omits an existing execution backend; D:142–174
Construction: an isolated Codex executor acknowledges a turn, writing `state=running` and `active_turn_id` (`crates/calm-server/src/isolated_codex/journal.rs:199,214`). Without a hook status overlay, W1 rejects Running and W2 has no row: active execution appears quiet.
Required change: distinguish harness, shared worker, isolated worker, and terminal execution using persisted backend identity; define each predicate explicitly. W1 currently ignores liveness, W2 ignores provider/contract, so the table’s terminal “never” and validator fallback are not encoded. Both queries correctly exclude terminal session states.

## MAJOR-3: Reconciliation cannot recover lost activity timestamps; D:180–193,337
Construction: activity watermark=100; completion at 200 commits, but the subscriber lags or the process crashes before projection. Existing overlays are never reseeded, so every reconcile preserves 100. A missed FSM Stop similarly leaves Working on disk forever. Pruning later removes hook/phase evidence (`crates/calm-truth/src/events_prune.rs:112`).
Required change: specify durable catch-up/checkpointing and reconciliation from completion evidence, including recovery of missed FSM inputs. Preserve projected high-water marks across pruning. Also define timestamp acquisition: `BroadcastEnvelope` has **no `at` field** (`crates/calm-truth/src/event_bus.rs:80`).

## MAJOR-4: TurnCompleted phase is not a completion receipt; D:181,209
Construction: refused `turn/start` restores `TurnCompleted`, even with `"unknown-turn"`, and persists a phase event (`crates/calm-server/src/harness/run_loop.rs:4481`). The proposed rule invents unread activity. Conversely, a system-error completion writes its outcome and emits `harness.item.added` without transitioning to TurnCompleted (`:2434`).
Required change: advance from a newly persisted completion identity/timestamp, not the phase tag. Include the failed-completion signal and conversation-list invalidation; currently item-added invalidates only transcript queries (`fe/core/events/invalidation-plan.ts:250`).

## MAJOR-5: FSM fixes 1–2 still lack session/turn fencing; D:203–204
Construction, fix 1: `UserPromptSubmit → Stop → 750 ms → late PostToolUse` resurrects Working indefinitely; removing four auxiliary mappings leaves tool hooks unconditional (`crates/calm-server/src/card_fsm.rs:241,329`).
Construction, fix 2: `new-session UserPromptSubmit → delayed old-session terminal event → 750 ms` overwrites the new session’s Working with Done. `Done → late PostToolUse` also resurrects a terminated card.
Required change: bind observations and pending timers to the current session/turn generation; reject stale events and prevent terminal-generation revival. Do not collapse terminal events to `card_id` alone.

## MAJOR-6: FSM fix 3 loses completions through debounce; D:205
Construction: `UserPromptSubmit → Stop → UserPromptSubmit` within 750 ms cancels pending Idle because Working equals the committed state (`crates/calm-server/src/card_fsm.rs:443`). No Idle overlay/event exists, so a real completion never advances activity. A first Stop after restart can instead emit Idle even when persisted status was already Idle.
Required change: persist/deduplicate completion occurrences independently of debounced visual-state transitions. Retain Stop→Idle semantics.

## MAJOR-7: Restart repair is neither scoped nor evidence-based; D:321,326
Construction: PTYs survive restart while the FSM map empties (`card_fsm.rs:403`; `crates/calm-server/src/main.rs:57`). Stale Working plus Running/Alive survives every W2 reconcile. Proposed Q2(b) also clears genuinely busy workers and any plugin’s matching status rows, without updating timestamps or emitting events.
Required change: prescribe a bounded repair of identified stale kernel/card rows, using current-session evidence and eventized writes. Hydrate/reconcile FSM state consistently; do not ship the blanket SQL as the deployment procedure.

## MAJOR-8: Legacy fallback defeats the new authoritative projection; D:198,217
Construction: persisted `any_card_needs_input=true`; after restart the session becomes terminal without an FSM event. New activity correctly computes none, but `needsUserAttention` still ORs the stale legacy flag. Projector reconciliation cannot clear a value owned by the unchanged FSM writer.
Required change: consult the legacy flag only when a valid new activity projection is absent, or make both projections share the same reconciled source.

## MAJOR-9: Bootstrap enumeration misses inactive failures; D:187–189
Construction: upgrade with a current failed task/session, only Done card overlays, no live sessions, and no activity overlay. It matches none of the tick candidates; absent a new event, its task/session failure is never projected.
Required change: bootstrap all relevant tracks and make recurring enumeration cover every attention/completion source, including tracks with only failed sessions or current failed tasks.

## MAJOR-10: Starting has no claimed wedge escape; D:169,328
Construction: a restored Starting session never obtains its thread. W1 remains true for every liveness value. The watchdog handles Resumed, interrupt deadlines, and running-turn duration, not PendingThreadStart (`crates/calm-server/src/harness/run_loop.rs:4705`). Reaper deliberately leaves Starting convergence to the spawn operation (`crates/calm-server/src/reaper/mod.rs:172`).
Required change: identify and test an actual bounded startup-failure owner, or explicitly change the starting indicator policy. “Wedged→failed” alone supplies no deadline.

## MAJOR-11: Compatibility analysis checks only one direction; D:86–87,199,209
Construction: new frontend against old server receives no `lastTurnCompletedAt` and no activity projection. A required nullable Zod field rejects the conversation response; tolerating absence instead silently suppresses activity. Existing bundled-client guards require a raised WEB version (`fe/web/src/app/providers/public.tsx:94`).
Required change: regenerate ts-rs/OpenAPI, update the SQL row/conversion, domain schema/model and fixtures; bump WEB 28→29 in both declarations. Follow the required-field REST precedent with revision 8→9 (`crates/calm-server/src/routes/version.rs:38`). SYNC remains 20 for this additive REST/opaque-overlay design; its gate does not require a bump.

## MINOR-1: Thirty seconds is an interval, not a convergence bound; D:136,337
Construction: a long reconciliation branch in the single projector task prevents receiving events or selecting another tick; the broadcast buffer is bounded (`crates/calm-truth/src/event_bus.rs:65`).
Required change: state the bound conditionally on sweep/DB latency, or specify bounded batches and scheduling. Serializing projector invocations alone cannot guarantee ≤30 seconds.

## Open-question answers (§8, one line each)
Q1: New-device baseline does not settle old-device migration; additionally, `dbInstanceId` rotates on every boot (`crates/calm-server/src/state.rs:1672`), so the proposed baseline repeats after restarts.
Q2: Reject the blanket update; use scoped, evidence-backed, eventized repair with a verified postcondition.
Q3: Filter to verified actionable notification subtypes; the proposed subtype vocabulary remains unverified, and generic Notification must not restore idle attention.
Q4: Keeping Starting requires a demonstrated bounded failure path; the cited watchdog does not provide one.
Q5: Reuse the primitive provisionally; final size/visibility remains a real-device visual acceptance decision.
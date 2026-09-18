<!-- archived review text, round 4, doc @ cf1a6b798 (four substitutions for the #1316 ratchet: the child-track route value quoted twice in MAJOR-1 reads `sub-<retired-word>` instead of the word itself; in the quoted gate-output block the two retiring vocabulary rows read `<retired-id-term>` / `<retired-item-term>` instead of the words themselves) -->

# Round 4 / channel A — verdict: REVISE

Reviewed `docs/architecture/1722-track-activity-indicators.md` @ cf1a6b798 against the worktree code (= b2341b871). Lens: product/FE semantics + tests, fresh eyes.

## Verified facts (34 confirmed / 1 incomplete)

Confirmed by reading the cited lines: F1.1 F1.2 F1.3 F1.5 F1.6 F1.7 F1.8 F1.9 F1.10 F1.11 F1.12 F1.13 F1.15 F1.17 F1.18 F1.21 F2.2 F2.3 F2.7 F2.10 F2.14 F2.18 F2.21 F2.22 F2.23 F2.25 F2.26 F2.28 F2.30 F2.31 F2.32 F2.35 F2.36 F2.37. Every §11 citation I re-read says what the row says (incl. `run_loop.rs:2456-2465,5201-5233`, `scheduler/mod.rs:1834-1843,1882-1900`, `task.rs:198-203,264-269`, `reaper/mod.rs:317-369,544-638`, `claude_restart_adapter.rs:171-175,223-237`, `preserving_recovery.rs:146`, `codex_appserver.rs:563-565,612-615`, `version.rs:157,234-249`, `terminal_hooks.rs:380` = test fixture, `calm-codex-bridge/src/main.rs:95-128`, `threads.rs:18`, `invalidation-plan.ts:250,262-268,323-331`, `session_projection_row.rs:12,28,62,100,122-129`, `model.rs:513-520`, `schemas.ts:251`, `event_serde_goldens.rs:321-332`, `runner.ts:504`). The `isRunning` consumer set (`track.ts:685`, badge `:23`, row `:95`, today `:230`; `activeTracksOn:768` does not call it) is exactly as §11 R3 rules. `EditAuthor` is `rename_all="lowercase"` (`event.rs:114`) so E7's `'user'` literal is right; `events.payload` is the `data` object (`events.rs:378-385`) so `$.author`/`$.kind`/`$.card_id` predicates are right; `turn/completed` params are the turn object minus items (`harness/turn_outcome.rs:15-18`) so E1's `$.status` is right.

- **F1.16 incomplete**: the `Working` live mark has **five** TSX sites, not four — `chat/thread/quiet-sync.tsx:100` (`QuietSyncFold`, its own `styles.live` from `quiet-sync.module.css:73-81`, wired from `ChatThread` at `thread/public.tsx:659` `live={live && holdsLast}`). §5.3 and §5.6 inherit the miss (see MINOR-3).

## Findings

### MAJOR-1 W clause makes every sub-track parent spin until the child's planner walks `reviewing → done` — the #1722 symptom, one level up
- Where: §4.2 W (`status ∈ {dispatched, running, verifying} → working`); §1/§9 G2; no mention of `spawn='sub-<retired-word>'` / `child_track_id` anywhere in the doc.
- Construction: parent track P dispatches a sub-track task (`TASK_CHILD_TRACK_ROUTE = "sub-<retired-word>"`, `calm-types/src/task_recovery.rs:15`). After the child-track spawn op the scheduler stamps it `running` with `worker_card_id = NULL` (`task_mark_sub_track_running_tx`, `task.rs:100-101`; caller `scheduler/mod.rs:1692,1705-1713`). That row leaves `running` only when `reconcile_child_track_task` sees `child.lifecycle = 'done'` with no in-flight/pending child tasks, or the child turns `failed`/`canceled`/is deleted (`scheduler/mod.rs:850-910,414-500`). `→ done` is a planner-only edge (`track_lifecycle.rs:41`), and §1/G2 are the doc's own evidence that planners leave tracks at `planning`. So under W, P is `working=true` for the whole life of the child — including while the child's planner is `idle`, or blocked on the user (child shows `attention`, parent shows a spinner). `current_tasks` has it (it is the only attempt), the mutation `dispatched_task_is_working_without_session_signal` cannot see it, and no tick corrects it. This is precisely "planner 干完活不推进阶段，track 就永远转", transplanted onto every parent.
- Required change: W must treat `child_track_id IS NOT NULL` rows separately. Minimal: exclude them from `working` (their "worker" is a track whose own overlay carries the truth; `failed`/E3 keep working as-is — child failed ⇒ parent task `failed` ⇒ red is correct, child done ⇒ E3 unread is correct). Better: parent `working` ∨= child's `activity.working`, with a wakeup rule (`overlay.set kind=activity` on the child ⇒ recompute `tasks.child_track_id` parents) and a depth bound — the doc must pick one and add a must-red test: parent with a sub-track task `running`, child planner `idle`, no child tasks in flight ⇒ `working=false`.

### MAJOR-2 (OPEN QUESTION G19) The proposed rule hides a real, present-tense attention request; adopt a narrower rule instead
- Where: §9 G19; §4.2 "活会话门" paragraph; §7 D-signal.
- Construction against the proposed rule ("for a task-bound worker card, session-sourced `failed`/`input` counts only while its current task is not terminal"): claude workers are spawned as an interactive TUI, no `-p` (`operation/claude_adapter/mod.rs:304-309`), and the worker card *is* a terminal card the user can type into (`systems/cards/builtins/claude.ts:3-6,17`). Worker reports `calm.task.complete` inside its turn (F2.36) ⇒ task `done`; the CLI stays at its prompt (that is why §1's PTYs are alive for days). The user opens the card, types a follow-up ("also fix the test"), switches to another track. The agent hits `PermissionRequest` ⇒ FSM `AwaitingInput`, session `running`, card is the current attempt's worker (S0 eligible) — a real "nothing moves until you act" — and the proposed rule suppresses it because the task is terminal. Same for `StopFailure → Errored` on that follow-up. Rail, sidebar strip and Notifications sidebar all stay quiet.
- What G19 actually is: only the **exit verdict** `ws.state='failed'` (signal-kill via `attach_reader.rs:127-131`, or reaper `session_commit_exit(Failed)` after the race-lost CAS `reaper/mod.rs:632-635,361-369`). Hook/FSM and `last_thread_status` evidence are already bounded by the active-session gate — a dead session cannot produce them — so they never create G19.
- Required change (ruling): for a task-bound worker card whose current task is terminal (`done`; `canceled` is vacuous — `task_cancel_tx` is `pending → canceled` only, `task.rs:135-146`, so such a card has no `worker_card_id`), drop **only** `ws.state='failed'` as `failed` evidence; keep FSM `AwaitingInput/Errored` and `last_thread_status ∈ {waitingOn*, systemError}` under the existing active-session gate. Update §4.2 (the "不套活会话门" sentence), §7 D-signal (task-bound worker of a `done` task ⇒ quiet, not red), G19 (closed), and add a must-red pair: `done_task_worker_signal_killed_is_quiet` (task `done`, session `failed` ⇒ `attention='none'`, `cards` has no entry) / `done_task_worker_permission_prompt_is_input` (task `done`, session `running`, overlay `AwaitingInput` ⇒ `input`). Residual to register: a live (ii) worker thread reporting `systemError` after `done` still shows red (accept; it is a live thread saying it is broken).

### MINOR-1 Per-card fold precedence is not written, so §7 B1/C1 CARDS cells are not derivable
- Where: §4.1 `cards[]` ("每张有结论的卡一行"), §7 B1 (`input`), B3 (`working`), C1 (`failed`).
- Evidence: at B1 the same worker card has W ⇒ `working` (task `running`) and FSM ⇒ `input`; at C1 a reaper-failed worker has W ⇒ `failed` plus session ⇒ `failed`. §3's `activityStateOf` is track-level; nothing says how one card's several conclusions fold. A reader can produce `working` at B1.
- Required change: one sentence in §4.1: per card, `failed > input > working` (same order as §3 minus unread); note C1's "1 项 source:task" holds for a worker-reported failure only — a reaper-failed worker also yields a `source:session` item (S0 eligible, `state='failed'`).

### MINOR-2 C3/C3′ timing is underivable: the `running` stamp emits no bus event and `task.dispatched` is not a wakeup
- Where: §4.3 wakeup table (lists `task.completed/failed/execution_settled` only); §7 C3/C3′.
- Evidence: `mark_running` is "a plain guarded UPDATE, no event rides along" (`scheduler/mod.rs:1878-1881`); the `worker_session.started` wakeup fires inside the spawn op, i.e. before the stamp. So C3's rail `working` appears at the spawn's `worker_session.started`, not at `dispatched`, and C3′'s `cards[worker]='working'` appears only at the next tick (≤30 s). B′ says this for (ii); C3/C3′ do not.
- Required change: add `task.dispatched` to the wakeup table (envelope scope) and annotate C3′ "下一次 tick" like B′; or state that the stamp has no wakeup as a G5 sub-bullet.

### MINOR-3 `QuietSyncFold` live mark missing from §5.3/§5.6; the primitive swap drops seven `aria-label="Working"` assertions
- Where: F1.16, §5.3 "对话流 Working 点" row, §5.6.
- Evidence: §5.6 deletes `.live` from `quiet-sync.module.css:73-93` but `quiet-sync.tsx:100` keeps rendering `styles.live` (undefined class ⇒ mark silently disappears; `quiet-sync.test.tsx:211` queries `summary [aria-label="Working"]`). `ActivityIndicator` is `aria-hidden="true"` with no name (`ui/activity-indicator/public.tsx:8`), while `chat/thread/public.test.tsx:95,184,216,423,426,633` and `quiet-sync.test.tsx:211` pin the accessible name.
- Required change: list `quiet-sync.tsx:100` as the fifth site; decide the a11y contract (wrap the primitive with a named span, or amend the seven assertions) and say which in §6 S2.

### MINOR-4 Pending-window all-blue flash: scope `null` should read as "not unread", not "everything unread"
- Where: §5.2 last bullet ("不做加固，登记为已知行为").
- Evidence: `UiPreferencesProvider`'s layout effect calls `setReadScope(null)` while `verdict==='pending'` (`ui-preferences.tsx:105-107`, `providers/public.tsx:89`), which clobbers the construction-seeded id; `receipt()` then reads an empty memory map (`:51`) ⇒ every track with `activityAt > 0` shows `unread` for one `/api/version` round trip on **every** web page load. Under this design almost every track has `activityAt`, so the flash is the issue's own screenshot ("17 条 track 全是蓝点"). The doc already treats `null` timestamps as "永不 unread"; the same convention for a null scope is one line in `isUnread` (`database === null ⇒ false`) and is in the safe direction of decision (c).
- Required change: adopt it (and pin with a test: `setReadScope(null)` ⇒ `isUnread(...)` false), or state the round-trip bound and why the flash is acceptable.

### MINOR-5 (ii)/(iii) discriminator re-derives what `is_isolated_card_tx` owns
- Where: §4.2 S0 `op.kind AS op_kind` via `ws.spawn_op_id`; (iii) `op_kind='codex-isolated-worker'`.
- Evidence: the original is by card, not by session: `EXISTS(SELECT 1 FROM operations WHERE kind='codex-isolated-worker' AND target_type='card' AND target_id=?)` (`isolated_codex/lookup.rs:9-12`; the reaper uses it, `reaper/mod.rs:120-127`). A re-minted isolated session with `spawn_op_id NULL` falls into (ii). Harmless today (W owns `working`, `failed` is the same rule, isolated has no feeder so `last_thread_status` is NULL) but it is the "mirror must call the original" shape the doc itself invokes in §4.1.
- Required change: use the card-keyed predicate in S0.

### MINOR-6 Must-red `user_lifecycle_edge_does_not_advance_activity` names an edge the kernel actor cannot take through the production writer
- Where: §6 S1 row "E4 去掉 actor 过滤" (positive twin "同边 by KernelDispatcher").
- Evidence: `draft → planning` is User/PlannerAgent only (`track_lifecycle.rs:32`); the kernel's production lifecycle writer is `auto_transition_if_current_in_tx` `Working → Reviewing` (`reaper/mod.rs:607-615`).
- Required change: make the positive twin `working → reviewing` by `KernelDispatcher` through that writer, or say the fixture inserts the `events` row directly.

## Mechanism attacks (no construction found)
- `working`: superseded sessions excluded by `c.session_id = ws.id` (F2.6); terminal cards never; harness `running` never written (`run_status_for` has no `Running`); preserving upgrade = G15 as stated; stale interactive-claude `Working` = G10 as stated. Tried: restart with stale `AwaitingInput` row ⇒ ≤750 ms `input` flash until `SessionStart` (severity 2<5 downgrade) — cosmetic, no change needed.
- `activity_at_ms`: user cannot light own unread via E1–E7 (E4/E7 actor/author filters correct on the persisted shapes; E3 excludes cancel; user typing into an interactive card ⇒ `stop` hook ⇒ unread is an agent result). Agent result not lit: only planner-terminal Claude sessions (F2.13 path, no `events` row) — covered by the planner's own E1.
- Receipts: two tabs write one baseline (key-missing check); fresh device = all read; db reset ⇒ new `databaseId` ⇒ new baseline; layout-before-passive order holds (React runs all layout effects, children first, before any passive effect); `refetchOnWindowFocus` refetch of `/api/version` is a no-op (`database === id` early return). No construction.
- deferred-tx: autocommit SELECTs + `write_with_event_typed` satisfy the head comment (`deferred_write_tx_invariant.rs:27-35`); `harness_turn_outcome_put` is `begin_immediate_tx` (`out_of_domain.rs:464`).
- Precedence/surfaces: after §5.1/§5.3 no surface derives an *indicator* from lifecycle/session/token; remaining lifecycle readers are ordering (`lifecycleRank` rank 1) and the badge tone, both declared. Exception is MINOR-3's fifth site.

## §6 gates and mutations
- S1 command set matches CI: `scripts/run-rust-nextest.sh:42-44` = `env -u NEIGE_CODEX_BIN cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci` (+`--test-threads 8` self-hosted, `run-ci-rust-nextest.sh:21`); clippy `ci.yml:636`; ratchets `:611-632`; openapi-drift `:1386-1432`. FE: `lint/build/test/test:browser` `:922-968`, `e2e` `:1285`, mutation `:1039,1151` (`lint:css` is inside `npm run lint`, `fe/package.json:20`). Manifest validator additionally requires a Vitest witness path per entry (`runner.ts:557-561`) — satisfied by the listed `selection_paths`.
- Every must-red row is a real kill given the named test (checked each stub against its assertion). Two targets ⇒ two entries is correct (`runner.ts:504`). Only MINOR-6 needs a fixture fix.

## §7 derivability
A0–A3, B1–B4, B′, C1–C4, D-exit/D-signal′ derive from §4/§5 given MINOR-1's fold sentence and MINOR-2's timing note. D-signal for a `done` task's worker derives to red today (G19) and must flip to quiet under MAJOR-2.

## Open-question answers (§8)
- Q1 baseline per `(device, databaseId)`: agree, closed. Q2 = (c) via the active-session gate: agree, closed. Q3 `Notification` whitelist from the official list: agree (doc-only value domain is stated). Q4 `starting` not working: agree. Q5 primitive first, size on device: agree.
- G19: reject the proposed wide rule (construction in MAJOR-2); adopt the narrow rule — suppress only `ws.state='failed'` for terminal-task worker cards.

## Gate outputs (ratchets, from the worktree root)
`scripts/gate-1316-terminology-ratchet.sh` (exit 0):
```
    <retired-id-term>     web    0
    <retired-item-term>   web    0
OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope.
```
`scripts/gate-prose-ratchet.sh` (exit 0):
```
OK: agent-facing prose in *.rs under crates/ is at the #1635 baseline for every term.
```

Note (not a finding against cf1a6b798): `origin/main` is now 3dbb96c84; since b2341b871, `fe/web/src/features/chat/thread/public.tsx` (+233), `crates/calm-truth/src/validation.rs` (+30) and `docs/oracle/capabilities-e2e.yaml` changed — §5.3's `thread/public.tsx:592,604,669,979` and §4.5's registry anchors must be re-read on the S1/S2 base.

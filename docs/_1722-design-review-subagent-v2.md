<!-- archived review text, round 2, doc @ 56043881b (two words substituted for the #1316 ratchet: in the quoted gate-output block the two retiring vocabulary rows read `<retired-id-term>` / `<retired-item-term>` instead of the words themselves) -->

# Round 2 / channel A — verdict: REVISE

Doc `docs/architecture/1722-track-activity-indicators.md` @ 56043881b; code @ b2341b871 (worktree `1722-activity-design`). Lens: product/FE semantics + tests, fresh eyes.

## Verified facts (31 confirmed / 1 wrong, 3 imprecise)

Confirmed by reading the cited lines: F1.1, F1.5, F1.6, F1.9, F1.10, F1.12, F1.14, F1.17, F1.18, F1.19, F2.2 (identity columns + writers), F2.3, F2.4 (`session_commit_exit` emits nothing: `session_repo_impl.rs:124-149`), F2.7, F2.9, F2.10, F2.11, F2.14, F2.15 (bridge fallback `calm-codex-bridge/src/main.rs:95-128`), F2.16, F2.18 (`ActorId` serde `{"kind":"User"}`, `ids.rs:32`), F2.20, F2.21, F2.22, F2.23 (`preflight.rs:287-296`, `version.rs:157`), F2.25, F2.26, F2.27, F2.28, §1 `db_instance_id` mint (`state.rs:1672`), §2.4 oracle anchors, deferred-tx head comment, §6 gate commands (= `.github/workflows/ci.yml:151,636` and `scripts/ci/local-rust-gates-selftest.sh:48-49`: `nextest run --workspace --locked --features calm-server/codex-e2e --profile ci --test-threads 8`; fe `lint`/`build`/`test`/`test:browser` at `ci.yml:929-968`).

- **F1.21 wrong (incomplete)**: a manifest entry is NOT `{mutation_id, defends, target, patch}`. `fe/tools/mutation/runner.ts:505-526` requires `expected_red` (exact vitest titles), `selection_paths` (tracked test files), `why_more_than_one`, and `defends` must name an INV that exists in `docs/oracle/*.yaml` (`run.mjs:75`). §6's S2 rows ("登记到 manifest.json") are under-specified: no oracle INV id / yaml named, no `expected_red` titles.
- Imprecise: F2.2 paths are `crates/calm-server/src/operation/{codex_adapter/mod.rs,planner_harness_start_adapter.rs}` (lines correct); F2.3 the writer is `session_set_harness_observation_runtime_tx` (`run_loop.rs:5145`); F2.26's first citation is `crates/calm-truth/src/db/sqlite/session_projection.rs:104-142`.

## Findings

### BLOCKER-1 §4.6 fix 2 has no emitter for the case it exists for; an `AwaitingInput`/`Errored` card whose session exits is stranded amber/red forever
- Where: §4.6 修 2, §4.2 row (iv) `attention`/`failed`, §7 D, §6 runbook, §4.6 修 4 blast radius ("直到…会话退出（修 2）").
- Evidence: production emitters of `Event::WorkerSessionStatusChanged` are 6× `Running` + 1× `TurnPending` (`grep "new_status: WorkerSessionState::"`) plus exactly one `Exited` — the isolated executor (`isolated_codex/observe.rs:113-135`). `Event::WorkerSessionSuperseded` has **zero** production emitters (only `worker_flow/mod.rs:302` consumer + tests). Claude PTY exit goes `terminal_renderer/attach_reader.rs:130` → `terminal_sweeper::complete_ephemeral_session_from_terminal_exit` (`terminal_sweeper.rs:92-118`, `write_in_tx_typed`, no event); boot reconcile `lib.rs:105` same; reaper `session_commit_exit` no event (F2.4); supersede `session_mirror.rs:341-364,580-596` no event. So fix 2 never fires for backend (iv) — the backend whose stale overlays motivated the design (§1: 5 `Working` + 1 `AwaitingInput` on 4140).
- Construction: claude worker C, session S `running`. `PermissionRequest` → overlay `AwaitingInput`. User closes the terminal / PTY dies / reaper. S → `exited` silently. FSM map keeps `AwaitingInput` (or is empty after restart). Projector (iv) `attention = status ∈ {AwaitingInput}` has **no session-state condition** (unlike `working`) → `attention='input'`, `items=[{source:card}]`, `cards=[{C,input}]` at every tick, forever. Same for `Errored`. §7 D "直到终端被关掉（修 2 → Done）" and the runbook line are false; the 4140 `AwaitingInput` card stays amber after ops closes it. A non-preserving (API 9 = Breaking) upgrade kills all PTYs → `lib.rs:90-120` completes sessions silently → same stranding on day one.
- Required change: (a) gate (iv)/(ii) `attention` and `failed` on `cards.session_id` being in an active state exactly like `working` (a status row with no active session = no conclusion); (b) drive fix 2 from a signal that exists — either the projector's tick observing `cards.session_id` ∈ {exited,failed,superseded} and calling `observe(card, Done)`, or add `WorkerSessionStatusChanged{Exited}` emission to `complete_ephemeral_session_from_terminal_exit`/reaper (caller sweep: `worker_flow/mod.rs:244-252` consumes it → cancel_card side effects must be reviewed); (c) rewrite §7 D, §6 runbook, 修 4 blast radius; must-red test: `AwaitingInput` overlay + `UPDATE worker_sessions SET state='exited'` (no event) → `reconcile()` → `attention='none'`.

### BLOCKER-2 backend (ii) `working` rests on `last_thread_status='active'`, which is the resting value after every turn: codex sends `thread/status/changed{idle}` BEFORE `turn/completed`, and the feeder stamps `turn/completed` as `active`
- Where: §4.2 row (ii), §4.3 E6, G8.
- Evidence (observed, codex-rs source checkout `/home/jasper/codex/codex-rs` @ 35aaa5d9fc, 2026-05-01): `app-server/src/bespoke_event_handling.rs:184-201` — on `EventMsg::TurnComplete` it calls `thread_watch_manager.note_turn_completed` (→ `clear_active_state` → status `Idle` → `ThreadStatusChanged` notification, `thread_status.rs:155-157,183-190,357-372`) and only THEN `handle_turn_complete` → `emit_turn_completed_with_status` (`:1316-1343`). Wire order: `thread/status/changed{idle}` → `turn/completed`. Neige's own run loop already knows this ordering for the sibling case ("Codex sends systemError BEFORE the failed turn/completed", `run_loop.rs:2430-2433`). Feeder: `liveness_feeder.rs:80-85` stamps `TurnCompleted ⇒ "active"`; nothing else writes the column (reaper only reads it, `reaper/mod.rs:210-215`; `session_record_activity_by_thread` has one caller). Inferred: 0.153.4 (installed) keeps the order — must be measured.
- Consequence: after the first turn every `codex-create`/`codex-worker` card has `last_thread_status='active'` until the next `thread/status/changed` (which only comes on the next turn) → (ii) `working=true` for a card idle for days = the "永远转" class this design exists to remove; E6 never fires → interactive codex cards never produce `unread`. G8's stated blast radius ("只是不产生 unread") is wrong: it is stale-Working. (Side note: this same sticky `active` is why the reaper's busy pre-gate never reaps completed codex threads.)
- Required change: measure the sequence on 0.153.4 (the `codex_appserver_e2e` observer at `tests/cases/codex_appserver_e2e.rs:234-248` already prints it — run it detached per memory, or a 20-line spike); then either stamp `TurnCompleted ⇒ "idle"` (and `TurnStarted ⇒ "active"`) in `stamp_status_for` with a unit test pinning `[status idle, turn/completed] → idle` (reaper semantics review: a completed-turn thread becomes deadline-reapable, gated by `confirm_durable_death`), or add an explicit turn-in-flight column set on `turn/started` and cleared on `turn/completed`/`idle`/`systemError`/`notLoaded`. Do not ship (ii) on the unverified premise; §4.3 "idle 只来自其后的 thread/status/changed" is contradicted by source.

### MAJOR-1 §4.6 fix 4 keys the fence on a Claude session id that rotates; after rotation every hook is discarded and the card's FSM freezes (permission prompts invisible)
- Where: §4.6 修 4; F2.26.
- Evidence: the fence accepts only `AiClaudeSession(ws) ∧ ws == cards.session_id`; the actor is `AiClaudeSession` only when `resolve_session_for_thread` matches payload `session_id` to `worker_sessions.agent_session_id` (`routes/codex.rs:359-397`, `session_projection.rs:123-142`), which is pre-minted once via `--session-id` (`claude_adapter/mod.rs:296-308`) and reused by restart via `--resume` (`claude_restart_adapter.rs:171-175`). Nothing updates `agent_session_id` afterwards (grep: no writer besides mint). Claude Code's `/clear` ends the session (`SessionEnd.reason=clear`, cited by `card_fsm.rs:309-312`) and starts a new one (`SessionStart.source=clear`); if the new session carries a new `session_id` (docs-inferred, not measured), every later hook degrades to `AiClaude(card)` and fix 4 drops it: `PermissionRequest` → no `AwaitingInput`, `Stop` → no `Idle`, card stuck at its last state. Today the card-level fallback still projects; fix 4 removes that.
- Required change: verify with a spike whether `/clear`/`/resume` rotate `session_id` under `--session-id`; if yes, either rotate `agent_session_id` on `SessionStart{source ∈ clear,resume}` (writer + test) or fence on "no *other* active session for this card exists" instead of id equality; register the residual as a KNOWN GAP with a must-red test either way.

### MAJOR-2 Three surfaces still derive working/failed locally; §7 C3 is not derivable from §4.2
- Where: §5.3 rows CONVERSATIONS / TASKS / Track 页头; §7 C3; §3 "判定只在内核一处".
- (a) CONVERSATIONS row keeps `isLiveConversation(state)` on the **server** `turn_pending` (`chat/list/public.tsx:43`, `conversation.ts:155-157`; router feeds `turn_pending` for server kinds when locally in flight, `router/public.tsx:282-284`, not only `'running'` as M12 says). A `turn_pending` row with no live harness (A-M6, accepted for the rail) therefore still spins here forever. Fix: read `activity.cards.get(card.id)` (harness cards are in `cards[]` per §4.2 (i)) and keep only the sender's local in-flight.
- (b) TASKS row maps `dispatched/running/verifying → working` in TS. The kernel has **no** task-based `working` source (§4.2 only sessions), so at C3 `dispatched` the session is at most `starting`/NULL-status → rail quiet, TASKS row spinning; §7 C3 claims rail `working` — underivable. Fix: either add `tasks[]` (or a task clause) to the overlay and have both rows read it, or make TASKS token-only and rewrite C3.
- (c) `TrackLifecycleBadge` tone: `isRunning → 'running'` painted accent-tinted "reads as alive" (`lifecycle-badge/public.tsx:21-24`, `.module.css:20-24`) and `failed` lifecycle painted `--warn-text` (`:16-18`). After §5.4 the header shows an amber badge next to a red dot for the same `failed` track, and an "alive" tint for the §1 planning track next to a quiet indicator. Fix: neutralize the `running` tone and give `failed` lifecycle `--error-text`, or declare it.

### MINOR-1 §4.2 "恢复后…行也是 idle，两边一致" is false; two restart gaps to register
- `state_from_snapshot` (`run_loop.rs:5201-5233`) sets in-memory `Resumed` but nothing persists it until the next `persist_snapshot`; the row keeps its pre-restart `turn_pending`. After `boot_harnesses` the registry is Live → first tick: `working=true` (spurious after a crash, correct during a preserving upgrade) → first persist flips it to `idle` while the resumed turn may still run. Also a `TurnCompleted` arriving in `Resumed`/`Idle` is "ignoring stale" (`:2456-2466`) → no `turn/completed` row → E1 never lights that turn. Register both in §9 and fix the sentence.

### MINOR-2 harness identity: call the original discriminator
- §4.2 (i) uses `handle_state_json IS NOT NULL`; the codebase's authoritative predicate is `json_extract(handle_state_json,'$.mode')='harness'` (`read.rs:1050-1051`, `shared_codex_appserver.rs:3997`) / `is_harness_snapshot_value` (`snapshot.rs:674`). Use the same predicate (镜像代码必须调用原件).

### MINOR-3 receipts: constructor seed and the pending-window flash
- `createUiPreferences` seeds `database` from `DB_INSTANCE_ID_KEY` at construction (`ui-preferences.tsx:17`, built at module init `production-app.tsx:69`). Under §4.8 that is the wrong identity family; §5.2's "scope 未定时 `database === null`" holds only after the first layout effect. Say explicitly: seed from a persisted databaseId key (gate writes it like `:81`) or seed `null`. Also (status quo, not regression): while `/api/version` is pending, scope is `null` → no baseline → every track reads unread until the version lands.

### MINOR-4 `items[].at_ms` unspecified for `card`/`session` sources; sidebar orders by it (`router/public.tsx:2703`). Name the column (`overlays.updated_at`? `worker_sessions.updated_at_ms`?).

### MINOR-5 (ii) transitions are tick-bound: the feeder writes with no bus event, so shared-daemon `waitingOnApproval` (§7 B′) surfaces only at the next tick (≤30 s + scan). Declare in G5/B′ or add a wake.

### MINOR-6 `overlay.set` trigger reads envelope `scope.track`, but `card_fsm::commit` falls back to `EventScope::System` on track lookup failure (`card_fsm.rs:539-546`) → add a `card_get` fallback.

### MINOR-7 nits: §6 "5×5 枚举" is 2×3×2=12 input combos; fix 3 leaves `PermissionDenied → AwaitingInput` (say so); Q3 whitelist values are docs-cited only (no fixture).

## Open-question answers (§8)
- Q1: agree (per-device, per-databaseId baseline, server `nowMs`); add the constructor-seed line (MINOR-3).
- Q2 (c): the runbook mechanism is false — closing terminals never reaches fix 2 (BLOCKER-1); the fix belongs in the projector's session gate, not in ops.
- Q3: agree with the whitelist; state `PermissionDenied` explicitly.
- Q4: agree, `starting` is not working.
- Q5: agree.
- G8 (not in §8 but asked): codex source shows `idle` IS emitted after every completed turn — before `turn/completed` (BLOCKER-2). Close G8 with a measured sequence, not a reading.

## Acceptance oracle (§7) derivation check
A0–A3 derivable (A2 via `harness.phase.changed` → E1 → `overlay.set` → `['overlays','track']`; drawer/list via `harness.phase.changed` → `trackConversations`, `invalidation-plan.ts:262-268`). B1–B4 derivable (B4: `Stop→Idle` commit → `overlay.set` → E5; if already Idle, next tick). B′ derivable only after ≤30 s (MINOR-5) and only if BLOCKER-2 is fixed (`idle → unread` never fires today). C1/C2/C4 derivable; **C3 rail `working` not derivable** (MAJOR-2b). **D false** (BLOCKER-1).

## Gate outputs (ratchets, last 3 lines each)
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
(Only one line of output; exit 0.)

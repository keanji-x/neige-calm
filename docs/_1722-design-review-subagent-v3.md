<!-- archived review text, round 3, doc @ 14fbcc3fe (five substitutions for the #1316 ratchet: the route kind name quoted three times reads `shared-<retired-word>` instead of the word itself; in the quoted gate-output block the two retiring vocabulary rows read `<retired-id-term>` / `<retired-item-term>` instead of the words themselves) -->

# Round 3 / channel A — verdict: REVISE

Doc `docs/architecture/1722-track-activity-indicators.md` @ 14fbcc3fe, code @ b2341b871 (worktree `1722-activity-design`).
Lens: product/FE semantics + tests. Fewer than one BLOCKER; three MAJOR (each with a small, local fix). §11 rows of both
rounds re-verified: every claimed fix is present in the doc text and every citation I opened says what the row says.

## Verified facts (34 confirmed / 0 wrong, 3 incomplete)

Confirmed by reading: F1.1 F1.2 F1.3 F1.4 F1.5 F1.6 F1.7 F1.9 F1.10 F1.11 F1.12 F1.13 F1.15 F1.17 F1.19 F1.20 F1.21 ·
F2.2 F2.3 F2.4 F2.6 F2.7 F2.9 F2.10 F2.11 F2.14 F2.18 F2.20 F2.21 F2.22 F2.23 F2.25 F2.26 F2.30 F2.31 F2.32 F2.33 ·
§2.4 deferred-tx head comment, oracle anchors (`capabilities-e2e.yaml:116,394`, `a11y-contract.yaml:25,41`), `owner-aliases.yaml:29`
(`core/domain/overlay` canonical), §11 citations (`terminal_hooks.rs:380` is inside `mod tests` @235; `run_loop.rs:2430-2433,2456-2465,4705`;
`claude_restart_adapter.rs:171-175,223-232`; `reaper/mod.rs:361-369`; `task_recovery.rs:215`; `codex_appserver.rs:563-565,612-615`).
No `file:line` was wrong. Three rows are incomplete in a way a mechanism rests on:

- **F2.22** omits that `worker_card_id` is stamped only at `dispatched→running`: `task.rs:198-203` (claim SQL sets no card),
  `:263-269` (`running` sets `COALESCE(worker_card_id, ?1)`), caller `scheduler/mod.rs:1875-1900` after the spawn op completes.
  A `dispatched` row therefore has `worker_card_id IS NULL` on every production path → MAJOR-3.
- **F1.6/F1.11** treat `/conversations` rows as the only server rows. The planner conversation is a route-injected row
  (`router/public.tsx:2791-2798`: `kind:'shared-<retired-word>'`, `state: plannerCard.runtime.status`, `updatedAt: runtime.updated_at_ms`)
  prepended to `rows` (`:2844`) and therefore fed to both `:1335` and `:1825`; `'shared-<retired-word>'` is a **route** kind
  (`conversation.ts:108-114`); the server list excludes it (`track_conversations.rs:382,389` `role = Assistant`) → MAJOR-1.
- **F2.25/§4.2 (ii)** `waitingOnApproval` is unreachable: every shared-daemon thread is started with `approval_policy: "never"`
  (`operation/codex_adapter/mod.rs:459`, `shared_codex_appserver.rs:1326,1376`, harness drops `approval/*` at `run_loop.rs:2303-2311`).
  Also `status_str_from_value` maps any unparsable status shape to `"active"` (`liveness_feeder.rs:39-45`) → MINOR-3/MINOR-6.

## Findings

### MAJOR-1 The planner conversation row is not covered by §4.7/§5.3 — §7 A1/A2 "CONVERSATIONS 行" is underivable
- Where: §4.7, §5.3 CONVERSATIONS row, §7 A1/A2; `router/public.tsx:2791-2798,2844,1335,1825`; `schemas.ts:246-258`.
- Evidence: §4.7 adds `last_turn_completed_at` only to `TrackConversationSummary` (the `/conversations` list). The planner row
  (scenario A's row) is built from `plannerCard.runtime` (`CardRuntimeView`: `status/updated_at_ms/thread_status`, no completion
  time). §5.3 keys its rewrite on "服务端 kind 的行" and says "本地 kind 的行不变"; by F1.6's own table the planner row is a route
  kind, so read literally it keeps `isLiveConversation(runtime.status)` (the A-M6 `turn_pending`-without-live-harness spinner) and
  `updated_at_ms` for unread (moves on the user's own enqueue, F2.8 — the defect the issue lists). Yet §7 A1 says
  "`cards[planner]='working'` 接手" and A2 says the row goes `unread` via `lastTurnCompletedAt`, which has no wire source.
- Required change: (a) add `last_turn_completed_ms` (same subquery as §4.7) to the planner row's source — `CardRuntimeView`
  (`session_projection_lookup.rs`) or the track-detail card payload — and list it in §4.5's wire/OpenAPI bump; (b) in §5.3 name
  the injected planner row explicitly (kind `shared-<retired-word>`, `router/public.tsx:2791`) as reading `activity.cards.get(plannerCard.id)`
  and `lastTurnCompletedAt`, and state the rule by *row origin* (server list + injected planner) rather than by
  `CONVERSATION_STATE_SOURCE`; (c) F1.6/F1.11 get the injected row.

### MAJOR-2 E5/E6 re-light a task-bound worker's result that E3 already lit (double unread)
- Where: §4.3 E5, E6 (SQL has no "interactive card" predicate although the prose for E6 says 交互卡); §4.2 (ii) row "完成类证据: E6（交互卡）；E3（worker）"; §7 B4.
- Construction: worker reports via MCP `calm.task.complete` (`mcp_server/tools/emit.rs:57` → `decision_sink.rs:194`) **inside**
  its turn; the turn ends later — F2.29's own numbers: `task.completed` 16:47:49, last feeder stamp 16:48:41 (=`turn/completed`,
  52 s later). Sequence: `task.completed` → E3 → blue dot → user opens the track (receipt = E3 time) → `turn/completed{completed}` →
  feeder writes `last_turn_completed_ms` → E6 (`WHERE provider='codex' AND mode<>'harness'`, worker sessions included) →
  `activity_at_ms` > receipt → blue dot again. Same for claude PTY workers: report tool call, then `Stop` hook seconds later → E5.
  Isolated executors are unaffected (E3 only), so the three worker backends disagree on "one result = one unread".
- Required change: restrict E5 and E6 to cards that were never task-bound (`NOT EXISTS (SELECT 1 FROM tasks t WHERE
  t.worker_card_id = <card>)`, the same predicate S0 already uses), so task-bound work lights exactly once via E3; fix the E6 prose/SQL
  mismatch; add a must-red `worker_turn_end_after_task_done_does_not_relight` (task `done@t1`, `last_turn_completed_ms=t2>t1` on the
  worker session → `activity_at_ms == t1`).

### MAJOR-3 Three §7 cells and one must-red test are not derivable from §4/§5 as written
- Where: §7 A2 (rail), §7 C3 (TASKS 行), §7 B′; §6 S1 row `dispatched_task_is_working_without_session_signal`.
- Evidence:
  1. **A2 rail `unread`** while the same row says "页头: 无（页面可见 → 回执立即清）". The rail and the page share one receipt
     (`sidebar.tsx:143` and `router/public.tsx:2719` both compare `track.activityAt` against `read:${db}:track:${id}`); a visible
     track page marks the new `activityAt` read as soon as `['track', id]` refetches, so the rail shows at most a flash. The rail
     value is only `unread` if the user is on another track — which the header column then cannot describe.
  2. **C3 "TASKS 行 `working`（`cards[新 worker]='working'`）" at `dispatched`**: `worker_card_id` is NULL until `running`
     (F2.22 addendum above), so W yields no `cards[]` entry and the TASKS row has no indicator at that step (rail `working` is
     correct). The must-red test asserts `cards[worker]='working'` with `status='dispatched'`: its green requires a fixture row
     (`dispatched` + `worker_card_id`) the production path never writes. During that same window S0 classifies the just-spawned
     worker card as "从未绑任务" (interactive), contradicting "任务绑定的 worker 走 W" — harmless today (both say working) but undeclared.
  3. **B′ `waitingOnApproval`** cannot occur on this stack (`approval_policy: "never"` on every shared-daemon thread, cites above);
     the row is fine as fixture-only, but as a 真栈 oracle step it is unexecutable.
- Required change: A2 → split into "on the page: rail 无 (or one-flash)" and "elsewhere: rail unread"; C3 → assert `cards[worker]`
  at `running` and only track-level `working` at `dispatched` (both oracle and test), and state the dispatched window under (ii);
  B′ → either `waitingOnUserInput` with a cited producer, or mark "fixture-only, approval unreachable under approval_policy=never".

### MINOR-1 Two local derivations survive on the assistant/planner conversation surfaces
- `chat/thread/public.tsx:68` `live = pending || isLiveConversation(conversation.state)` drives the dots §5.3 swaps at `:592,604,669`;
  for the assistant thread `state` is the server `turn_pending`, i.e. (i) minus the registry condition → disagrees with the rail in
  exactly the A-M6 case. `router/public.tsx:282` `facts.stalled ? 'failed'` (planner-run `phase==='wedged'`, `:346`) stays a second
  `failed` source for the open row. Add both to §5.3 (thread: `pending || cards.get(card)==='working'`; stalled: drop or declare).

### MINOR-2 `NEUTRAL_ACTIVITY` as specified fails the `architecture/no-module-runtime-state` gate §2.4 claims to satisfy
- §5.1 `NEUTRAL_ACTIVITY: … / [] / 空 Map`. The checker rejects every `new` at module scope (`immutableConstructors` is empty,
  `no-module-runtime-state.mjs:31`) and a nested `[]` inside `Object.freeze({…})` unless it is itself frozen (`isStaticData`, `:99-100`,
  `allowContainer=false` for property values). Use `Object.freeze([])` and a frozen `Readonly<Record<string, CardActivity>>` (or a
  lookup helper) instead of `ReadonlyMap`.

### MINOR-3 Feeder's parse-failure default is fail-open for (ii) `working`
- `status_str_from_value` → `"active"` on any unknown status shape (`liveness_feeder.rs:39-45`, written for the reaper where
  `active` is the safe side). After §4.2.1, a codex release that adds an `activeFlags` value or status type makes an idle
  interactive card spin until its next turn. §4.2.1 should map unparsable shapes to a non-working value (e.g. `"unknown"`; the
  reaper's 900 s deadline pre-gate still protects it) and pin it with a unit test.

### MINOR-4 `DATABASE_ID_KEY` is written only when absent (§5.2 "像 `:81` 那样")
- After a DB reset (new `database_identity`), the stored key is stale forever; construction seeds the old id, the layout effect
  switches to the new one each load (one-frame flash of old receipts). Overwrite on change, as `providers/public.tsx:75-81` does
  for the instance id.

### MINOR-5 §5.2 pending-window sentence is too narrow
- With verdict pending the scope is `null` → receipts are the fresh in-memory map → **every** device on **every** web page load
  shows all tracks unread until `/api/version` lands (not only first-entry devices); bundled clients are shielded by `:91`.
  State it as such (existing behaviour, still not this design's to fix).

### MINOR-6 (ii) restart healing rests on an uncited observation
- Only the feeder writes `last_thread_status` (no other writer in `crates/*/src`); F2.29's "一次批量盖章（重启/恢复）" is inferred
  from timestamps. A crash mid-turn leaves an interactive `codex-create` row at `running`+`active`; whether the daemon emits
  `thread/status/changed` on resume decides if it heals or spins until the next user message. Cite the resume path or add to §9.

### MINOR-7 Smaller doc/test-shape items
- §5.3 Today: header "N working" (`isWorking`) will not equal the "In progress" section it sits above (`isRunning(lifecycle)`);
  say so or make the count the section's length.
- §4.2 (v) "终端 **永不** working" contradicts W: a terminal task's card gets `cards[worker_card_id]='working'` (the running
  stamp takes the spawn op's created card id for every worker kind, `scheduler/mod.rs:1836-1843`).
- §6 gate list omits `npm run e2e` and the mutation runner (`test:mutation:plan/run`, `ci.yml:1030-1151`) that validates the new
  manifest entries (`expected_red` titles must exist and go red).
- §6 S2 first row conflates two mutations with different `target`s (`core/domain/track.ts` vs `web/src/features/track/row/public.tsx`,
  `runner.ts:504` requires patch target == `target`) → two manifest entries. Rows "侧条仍读…" and "手机 painter" name files but no
  test title, so `expected_red` cannot be pre-derived from the doc.
- §4.1 `items[].source:'session'` for (i) Wedged: `id` = session id but the sidebar renders per card; say `card_id` is always set there.

## Mechanism attacks (constructions tried)
- `working`: stale-Working via (iv) survives only inside the 750 ms downgrade window across a restart (G10 covers); superseded
  sessions excluded by `cards.session_id` join (F2.6); terminal cards only via W (see MINOR-7); planner `turn_pending` vs
  `running`: harness never writes `running` (`state.rs:38-49`) — no construction beyond G15. Preserving upgrade: a real
  `TurnCompleted` after resume is dropped as stale (`run_loop.rs:2456-2465`) → G15 (no E1) — correctly registered.
- `activity_at_ms`: user's own action lighting unread — no construction found: REST report edit → `EditAuthor::User`
  (`track_report_origin.rs:444-446`), `auto_promote_draft` is `ActorId::Kernel` but only agent writes trigger it
  (`policy_for(RestUser).auto_promote_draft=false`, `:447`; `decision_sink.rs:341-345`, `plan.rs:666-670`), lifecycle PATCH is
  `User`, cancel excluded by E3. Agent result not lighting: E1 covers assistant cards (transcript rows carry `track_id`,
  `out_of_domain.rs:470-473`); remaining cases are G6/G15/G16. Double-lighting: MAJOR-2.
- Receipts: two tabs (both write the same baseline, last wins by ms — harmless); restart (`switched` path clears IDB/query cache
  only, `providers/public.tsx:75-80`; localStorage receipts survive under the stable `databaseId` key) ✔; fresh device ✔;
  layout-before-passive ordering ✔ (`ui-preferences.tsx:105-107` vs `:144-156`); DB reset → MINOR-4.
- `card_fsm` fixes: `exited` session + `AwaitingInput` row → hidden by the live gate at the next tick; a card restart resumes the
  same UUID and `SessionStart→Starting` downgrades the stale row within ~1 s (G14 shape) — no stranding found. `PermissionDenied`
  sticks at `AwaitingInput` until the next hook (claude continues → `PreToolUse`/`Stop`) — acceptable.
- Transaction shape: autocommit SELECTs + `write_with_event_typed` satisfy `deferred_write_tx_invariant.rs` (no `.begin(`/`begin_read_tx(`).
- Precedence/per-surface: `isRunning(lifecycle)` remains only in Today grouping, `lifecycleRank`, `activeTracksOn`, and the badge
  (tone → neutral) — out of every indicator path; survivors are MINOR-1.

## Open-question answers (§8)
- Q1: agree — one baseline per `(device, databaseId)`, written in `setReadScope(id, nowMs)`; old devices re-baseline once after the key-prefix change.
- Q2: agree with (c) — live-session gate + runbook; no FSM rewrite.
- Q3: agree — whitelist `{permission_prompt, elicitation_dialog, elicitation_url_dialog, agent_needs_input}`, fail-closed to `None`.
- Q4: agree — `starting` is not working (no bounded exit, B-M10).
- Q5: agree — primitive first, size signed off on device.

## Gate outputs (ratchets, last 3 lines each; run from the worktree root, both exit 0)
```
scripts/gate-1316-terminology-ratchet.sh
    <retired-id-term>     web    0
    <retired-item-term>   web    0
OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope.

scripts/gate-prose-ratchet.sh
OK: agent-facing prose in *.rs under crates/ is at the #1635 baseline for every term.
```
§6 S1 nextest/clippy lines match `scripts/run-rust-nextest.sh:39-41` and `ci.yml:636`; S2/S3 lines match `fe/package.json:11-22`
(`tools/oracle/oracle.test.ts` is inside `npm test` via the platform-independent project); see MINOR-7 for the two omissions.

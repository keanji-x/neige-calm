<!-- archived review text, round 1, doc @ e634a0f8 (two words substituted for the #1316 ratchet: in the quoted gate-output block the two retiring vocabulary rows read `<retired-id-term>` / `<retired-item-term>` instead of the words themselves) -->

# Round 1 / channel A — verdict: REVISE

Reviewed `docs/architecture/1722-track-activity-indicators.md` @ e634a0f8 against worktree `1722-activity-design` (= origin/main b2341b871). Lens: product/FE semantics + tests.

## Verified facts (44 confirmed / 0 wrong; 1 trivial citation slip)

Confirmed by reading the cited lines: F1.1–F1.21 (all), F2.1–F2.24 (all), §2.4 deferred-tx head comment (`deferred_write_tx_invariant.rs:1-56`), ownership rows (`module-file-inventory.yaml:41,42,43,124,130`), oracle anchors (`pages-shared.yaml:61`, `capabilities-e2e.yaml:332,354,394`, `a11y-contract.yaml:25,41`), CI gate commands (`ci.yml:149-151,611-636,922-935,967-968,1428-1432`; `scripts/run-rust-nextest.sh:42-44` = `env -u NEIGE_CODEX_BIN cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci`, self-hosted adds `--test-threads 8`). Both ratchets pass (below).

- Slip (not load-bearing): §2.4 says `capabilities-e2e.yaml:116,394` contain `2716-2729`; only `:394` does (`:116` is `548-566,708-715,773-777,946-959` — those also shift when line 1335 changes, so the anchor list is still right).
- Two facts the doc records but does not *evaluate*, and each hides a defect: F1.19 (token *values*, see MAJOR-1) and F2.15/M9 "app-server threads send no CLI hook" (the doc's own W2 depends on hooks for app-server codex workers, see BLOCKER-2).

## Findings

### BLOCKER-1 `Notification(idle_prompt)` re-creates `Stop → attention` 60 s after every Stop; the must-red test encodes the wrong end-state
- Where: §4.6 fix 3 + §8 Q3 + §6 row "S1 / SubagentStop" (`card_fsm::tests::stop_then_subagent_stop_stays_put` expects `AwaitingInput`); table row `card_fsm.rs:300-304` (`Notification → AwaitingInput`, unchanged by the design).
- Construction / evidence: the repo already measured the value the doc calls "未核实": `docs/architecture/1548-planner-terminal-wiring.md:683` — "Idle after Stop | `Notification` `notification_type: idle_prompt`, `message: Claude is waiting for your input` at +60 s | once per idle period". So for every Claude worker: `Stop → Idle` (fix 3) → +60 s `Notification(idle_prompt) → AwaitingInput` (upgrade 1→5, commits immediately) → `attention` amber on rail/CARDS/side-bar, "点开不清". Decision (a) is nullified by the side channel for the exact population it was made for. The production sequence `stop → notification → subagent_stop` (issue) is this shape; the doc's claimed result "停在 Notification 的结果上" (= AwaitingInput) is the bug, and the new test pins it.
- Required change: close Q3 inside the design, not as an open question: `Notification` projects to `AwaitingInput` only when `payload.notification_type ∈ {permission_prompt, elicitation_dialog}` (the first is measured in-repo, `terminal_hooks.rs:380`; verify the second against the installed Claude Code hooks reference — `PermissionRequest`/`Elicitation` hooks already carry the actionable signal per 1548:682, so absent/unknown subtype → `None`, fail-closed toward quiet). Rewrite the must-red test: sequence `user_prompt_submit → stop → notification{idle_prompt} → subagent_stop`, after >750 ms status is `Idle`, never `Working`, never `AwaitingInput`; add the positive twin `notification{permission_prompt} → AwaitingInput`. This needs `claude_kind_to_state(kind, payload)` to read the payload (it already receives it, `card_fsm.rs:329`).

### BLOCKER-2 `working` / `attention` / `unread` are unreachable for every shared-daemon codex card; the durable signal that would carry them (`last_thread_status`) is unused
- Where: §4.2 W1/W2 + table row 3 ("之后由 W2"); F2.15/M9; `codex_adapter/mod.rs:454-463` (`shared_codex_appserver.thread_start_mint_for_card`, worker path `spawn_codex_worker_via_shared_daemon`), `pending_codex_threads.rs:419` (`→ Running` at bind).
- Construction: dispatch a codex worker (shared daemon). `turn_pending` (W1) for the thread-registration window → `running` at bind → W1 stops matching; W2 needs a `kernel/card/status` row, which needs `hook.codex.*`, which for shared-daemon threads cannot arrive: `ingest_hook` requires a non-empty `card_id` query (`routes/codex.rs:150-160`, `EmptyAiCardId`), and the shared daemon's env has no per-card `NEIGE_CARD_ID` (`shared_codex_appserver.rs:59-75` passthrough list; per-card env exists only for dedicated/isolated codex, `dedicated_codex/layout.rs:69`). Result: the whole turn is `quiet`; a `waitingOnApproval` thread never lights `attention`; a thread that ends its turn without `task.completed` never lights `unread`. Same for interactive `codex-create` cards. The kernel already has the durable per-thread verdict: `worker_sessions.last_thread_status ∈ {active, waitingOnUserInput, waitingOnApproval, idle, systemError, notLoaded}` fed by `liveness_feeder.rs:85-115` on `thread/status/changed` + turn boundaries (0053), and it is already on the wire as `CardRuntimeView.thread_status` (`fe/core/api/schemas.ts:257`).
- Required change: add to §4.2 a W3/A4 pair keyed on `last_thread_status` for `provider='codex' ∧ state ∈ {running, turn_pending}`: `active → working`; `waitingOnApproval|waitingOnUserInput → input` (item `source:'session'`); `systemError → failed`; and treat the `active → idle` edge as completion-class for `activity_at_ms` (§4.3). Trigger: either subscribe the projector to the daemon notification stream the feeder already reads, or accept the 30 s tick and say so in G5. Add a must-red sqlite test (`worker_sessions.state='running', last_thread_status='active'`, no status overlay → `working=true`). If the implementer can show `hook.codex.*` rows with worker card ids in the production events table, downgrade this to MAJOR (the approval/idle gap remains).

### MAJOR-1 `--warn` and `--error` are the same colour: decision (b) is not implementable with the tokens as they are
- Where: §5.4, F1.19; `tokens.css:227` `--warn: oklch(49% 0.14 30)` vs `:398` `--error: oklch(51% 0.14 25)` (dark: `:524` hue 30 vs `:539` hue 25).
- Evidence: 5° of OKLCH hue at equal chroma, ΔL 0.02 — ΔE_ok ≈ 0.023; two 6 px dots (`--dot-sm`) cannot be told apart. `fe-design.md:78` ("Error 与 warning 必须分开，不能都退化成琥珀色") stays violated *visually*; the doc claims it becomes satisfied.
- Required change: state the token fix in the design: either re-hue `--warn`/`--warn-soft`/`--warn-border`/`--warn-text` (light+dark) to a real amber (≈ hue 70–85, then `tools/styles/check-contrast.mjs` re-run; `styles/` is readonly → OWNERSHIP-CHANGE trailer), or introduce an indicator-local token. Add a browser test that asserts the two computed `background-color`s differ by more than a threshold (or pin the hues), so the mutation "swap `.failed` back to `--warn`" goes red.

### MAJOR-2 the user's own lifecycle change advances `activity_at_ms` — contradicts "用户自己的 … 都不动它"
- Where: §4.3 row `track.lifecycle_changed` (unconditional 推进); `track_lifecycle.rs:32-47` user edges (`draft→planning` kickoff, `blocked/reviewing→working` resume, any→`canceled`, terminal→`planning`).
- Construction: tab A on Today, tab B on Today; user kicks off a Draft in tab B (`draft→planning`, actor `User`) → projector advances high-water → tab A shows a blue dot on a track with no agent result; same for cancel/resume. (Own tab is masked only when the track page is open, via `useReadReceipt`.)
- Required change: the projector reads `BroadcastEnvelope.actor` (`event_bus.rs:76-105`) and advances `activity_at_ms` on `track.lifecycle_changed` only when `actor ≠ ActorId::User` (planner/kernel edges: `→dispatching/working/blocked/reviewing/done/failed`); still recompute `attention` on every lifecycle event. Same actor filter documented for `task.*` (user-initiated cancel).

### MAJOR-3 attention/failed and per-card `working` are each derived twice; a fixture makes rail and Notifications side-bar disagree
- Where: §5.1 (`needsUserAttention = attention==='input' ∨ lifecycle∈{blocked,reviewing} ∨ anyCardNeedsInput`, `hasFailed = attention==='failed' ∨ lifecycle==='failed'`) vs §4.2 A3 (kernel folds the same lifecycle into `attention`/`items`); §5.3 CARDS row ("与 W2 同判据，FE 侧从 detail.overlays 读") vs `CardRuntimeView` (`fe/core/api/schemas.ts:246-258`) which carries no `liveness`, so the FE cannot reproduce W2 (`liveness ∈ {alive,unknown}`).
- Construction: fixture `lifecycle:'reviewing'`, overlay `attention:'none', items:[]` (kernel not yet recomputed, or old kernel) → rail `attention`, side-bar 0 items; §7 promises "同源、同一集合、同一消失规则". Fixture `runtime.status='running'` with `status=Working` overlay on a session the reaper marked `liveness='exited'` (`reaper/mod.rs:184-190,228`) → CARDS row `working`, rail quiet.
- Required change: pick one owner. Recommended: kernel owns it — FE `attention`/`failed` read the overlay only; the lifecycle OR stays only as the *named* old-kernel fallback (like `anyCardNeedsInput`) with its S4 removal listed in §5.6; put per-card verdicts into the overlay (`cards: [{card_id, state: working|input|failed}]`) so CARDS rows and the terminal head read the kernel's answer instead of re-implementing W2 in TS ("mirror code must call the original").

### MAJOR-4 the rail row's accessible name still says "running" from `isRunning(lifecycle)` — an indicator path the doc and its mutation miss
- Where: `row/public.tsx:95,104` (`running = isRunning(track.lifecycle)`; `bits = [attention ? 'waiting on you' : '', running ? 'running' : '']` → `aria-label`), not in §5.3; §6 mutation "把 `isWorking` 换回 `isRunning`" targets `trackActivityState` only.
- Evidence: after S2 the planning-track-with-idle-planner is visually quiet but announced "Track X, running, Planning" — the issue's lie survives for screen readers; `sidebar.test.tsx`/`row/public.test.tsx` query rows by that name.
- Required change: §5.3 rail row: `bits` from `trackActivityState` (`working` → "working", `attention` → "waiting on you", `failed` → "needs attention"); the must-red mutation must also flip the label's source; update `a11y-contract.yaml` if it pins the phrase.

### MAJOR-5 mobile track-page header has no indicator placement — an undeclared mobile/desktop difference
- Where: §5.3 "Track 页头 … `track/page/public.tsx:552,558`"; `public.tsx:211` `titleInHeader = compactViewport && mobileHeaderTitleHost !== null`; `:552` renders only `!titleInHeader`, `:558` only when `!(titleInHeader && !boardOpen)`.
- Evidence: on the unified mobile header (#1707) neither line renders, so the page-head indicator (and the lifecycle badge) is desktop-only; §7 A1/A2 "页头 working" cannot be observed on mobile. Owner rule: mobile may lack it, but the doc must say so.
- Required change: either place it in `MobileTitleReadView`/`MobileHeader meta` (and add the mobile case to the S3 same-`data-nc-activity` scan), or add a "手机页头无指示器" row to §5.3 and G4-style entry in §9.

### MAJOR-6 a persisted `starting`/`turn_pending` harness row with no live harness spins forever; the 30 s tick cannot converge it
- Where: §4.2 W1 (reads `worker_sessions.state` only); `harness/mod.rs:151-183,220-228` (`RecoveryOutcome::Skipped`: card/track gone, workspace not restored, area-chat, no `handle_state_json`, sealed thread; `DaemonIneligible`); reaper writes `liveness='exited'` as an observation on live resumable rows (`reaper/mod.rs:184,228,253,287`) which W1 ignores.
- Construction: kernel dies mid-turn (`run_status_for(TurnRunning) = TurnPending` persisted, `run_loop.rs:5128-5129`); on boot the track's managed workspace is not at its owned path → recovery refused → row stays `turn_pending` with no run loop → W1 true on every tick; no event will ever arrive. This is the issue's "persisted row lies" class, and G5's "≤30 s 收敛" does not cover it.
- Required change: gate W1 on the in-process authority — `HarnessRegistry::live_for_track` (`harness/registry.rs:221`) — or on `liveness ≠ 'exited'`; add the must-red test "row `turn_pending`, registry empty → `working=false`". This also answers Q4 (a daemon that never comes up is not "live").

### MINOR-1 fix 2 writes `status=Done` overlays for terminal and planner cards
`WorkerSessionStatusChanged{Exited|Superseded}` → `observe(card, Done)` is a first observation for cards with no FSM entry (`card_fsm.rs:432-437`) → a `kernel/card/status` row for every exiting terminal/planner/assistant card; #1620 says Planner-opened terminals never move the FSM (`routes/codex.rs:196-218`). Scope the two arms to cards that already have a `status` overlay (or `map` entry), which is all the retirement needs.

### MINOR-2 `calm.user.notify` silently dropped from v0's completion-class list
v0 lists it; §4.3 omits it with no M-n. It writes nothing itself (`mcp_server/tools/user_notify.rs:12-18`) but lands as `harness.item.added{method:'item/completed', item_type:'mcpToolCall'}` mid-turn; a long background turn keeps the reader unaware until `turn/completed`. Either add that trigger (filter on tool name in `params`) or register M13 with the rationale.

### MINOR-3 declare the remaining `failed` consumers
`sidebar.tsx:102` waiting bucket / collapsed count and `lifecycleRank` (`track.ts:684-687`) use `needsUserAttention` only — a `failed`-attention track sorts after working ones and is not counted "waiting on you". §5.3 covers Today's count but not these two.

### MINOR-4 baseline clock source
G3 is avoidable: the version response (or any server frame) can carry server `now_ms`; using it as the baseline removes the ±Δ window and the client-clock dependency for a one-line change. Also: `first_scope_entry_marks_everything_read` should also assert the baseline key is *not* written when `setReadScope(null)` runs first (fresh device passes through `null` before the verdict, `providers/public.tsx:89`).

### MINOR-5 reaper `session_commit_exit` emits no event, and the tick set can miss the track
`session_repo_impl.rs:124-149` (no bus event); `converge_dead_worker` fires `task.failed` *before* the row turns `failed`. A track with no `activity` overlay yet and no status overlay whose session fails this way is outside the §4.3 tick set → `failed` never appears. Include `∃ worker_session with state='failed' ∧ completed_at_ms > last tick` (or simply all tracks with any session row) in the tick set; the cost is one indexed SELECT.

### MINOR-6 small corrections
- §4.3 `overlay.set` row: the envelope scope already carries `track` (`EventScope::Card`, `card_fsm.rs:539-546`); no `card_get` needed.
- `turn/completed` is also written for interrupted turns (`run_loop.rs:2420,2438`) → user-initiated interrupt counts as a completion; say so or filter on `status`.
- `TrackConversationSummary.last_turn_completed_at` must be `.nullable().optional()` in the zod schema (old server omits the key), and §2.4's "no WEB_COMPAT bump" should note a new bundle on a v28 server shows no working/unread at all (the "server-update" curtain exists for exactly this; consider bumping).
- Tick set "all tracks with an `activity` overlay" grows monotonically; scope to `working ∨ attention≠none` to keep it O(active).
- §6 S2 mutations are prose; `fe/tools/mutation/manifest.json` (71 patch entries, `defends: oracle:*`) is where CI would run them — register the `isWorking→isRunning` and `activityStateOf` order mutations there with an oracle ID.
- After a restart the FSM's first observation re-commits an unchanged `Idle` (`card_fsm.rs:436`) → one spurious `overlay.set(Idle)` → one spurious unread per card; acceptable, but list it under G.

## Open-question answers (§8)
- Q1: keep "老设备也写一次" (it is the only reading of (c) that avoids the 换源 blue-dot burst); derive the value from server time (MINOR-4).
- Q2: (b), but through `write_with_event_typed` (eventized upsert, or a one-shot at S1 boot keyed by a `retention_meta` marker) — a raw `UPDATE overlays` emits no `overlay.set`, so every open bundle keeps spinning until a refetch.
- Q3: not open — BLOCKER-1; allowlist `permission_prompt` (+ verified elicitation subtype), everything else → `None`.
- Q4: keep `{starting, turn_pending}` and gate W1 on `HarnessRegistry` (MAJOR-6); a daemon that never starts has no live harness → quiet, and `Wedged → failed` still covers the harness's own verdict.
- Q5: replace with the primitive (one vocabulary is the point); keep the 6 px ring; preview sign-off decides size only, not whether.

## Gate outputs (ratchets, last 3 lines each)
`scripts/gate-1316-terminology-ratchet.sh` (from worktree root):
```
    <retired-id-term>     web    0
    <retired-item-term>   web    0
OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope.
```
`scripts/gate-prose-ratchet.sh`:
```
OK: agent-facing prose in *.rs under crates/ is at the #1635 baseline for every term.
```
(prose ratchet prints one line; the doc adds no `.rs` file at this commit.)

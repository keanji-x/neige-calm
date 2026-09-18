<!-- archived review text, round 6, doc @ a807c1b2f (two substitutions for the #1316 ratchet: in the quoted gate-output block the two retiring vocabulary rows read `<retired-id-term>` / `<retired-item-term>` instead of the words themselves) -->

# Round 6 / channel A — verdict: REVISE

Doc: `docs/architecture/1722-track-activity-indicators.md` @ a807c1b2f, code read at b2341b871 (worktree `1722-activity-design`; `git merge-base HEAD origin/main` = b2341b871, diff vs base is docs-only).
Lens: product/FE semantics + tests, fresh eyes. One MAJOR (a rendering site of `ChatList` that §5.3 does not map), five MINORs. No BLOCKER.

## Verified facts (47 confirmed / 0 wrong)

Confirmed against the code (every `file:line` says what the row says): F1.1–F1.14, F1.16–F1.19, F1.21; F2.1 (migrations 0032/0041/0045/0053/0054/0081/0094/0097, next number 0110 also on current origin/main), F2.2, F2.3, F2.7, F2.9–F2.12, F2.14, F2.16, F2.18, F2.20–F2.26, F2.28, F2.30–F2.33, F2.35–F2.40. Also §2.4 gate rows (deferred-tx head comment lines 1-56; ownership inventory `:41-43,124,130`; oracle anchors `pages-shared.yaml:61`, `capabilities-e2e.yaml:116,332,354,394`, `a11y-contract.yaml:25,41`), the 14 + 11 accessible-name assertion counts (re-ran both greps), the five `styles.live` sites, the four `@keyframes`, `manifest.json` = 71 entries and `runner.ts:500-527` / `run.mjs:70-76` validator shape, and the §11 v6 citations (`today/public.tsx:12-13,229-230,245,254,397-398`; `chat/list/public.tsx:46,67`; `page/public.tsx:71-77,817,830`; `router/public.tsx:2688-2692,3411-3419`; `providers/public.tsx:75,77-79,81`; `observe.rs:113-126`; `scheduler/mod.rs:414-440,910-922,1052-1059,1834-1843,1878-1900`; `session_mirror.rs:82,136-141,263-290`; `claude_restart_adapter.rs:157-168,223-241,493-498`; `tests/claude_card_endpoint.rs:886-931`; `tests/scheduler.rs:8051-8077`; `claude_adapter/mod.rs:362-382,798-804`).
Two cosmetic notes, not discrepancies: `snapshot.rs:674` `is_harness_snapshot_value` parses the whole snapshot (mode check is inside `parse_known`), so "同一判据" is loose but true; `INV-APP-117` is unused on origin/main (max is 116) — 118 is a skip, harmless.
CI commands: §6 S1 line matches `scripts/run-rust-nextest.sh:42-44` (`env -u NEIGE_CODEX_BIN cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci`, `--test-threads 8` from `run-ci-rust-nextest.sh:21`), `ci.yml:151,636,611-632`; S2/S3 lines match `ci.yml:929-935,968,1030,1039,1151,1285` and `fe/package.json:14-24`.

## Findings

### MAJOR-1 A third `ChatList`/`ChatThread` site — Today's launchpad Conversations module — is outside the §5.3 surface map, and its `activity` source is unstated
- Where: §5.3 "CONVERSATIONS 行" (names only the track route's two row sources), §6 S2 file list, §7 A (no Today conversation column); code `fe/web/src/app/router/public.tsx:2256-2307` (`launchpadConversationsQuery` → `launchpadRows` → the second `useConversationPanel` call, `showTrack:false`), `:1823-1827` (the one `<ChatList>`), `:2467` (`conversationList` into `TodayPage`), `today/public.tsx:246`.
- Construction: `POST /api/today/summary` creates the launchpad's `track-assistant` conversation, which is exactly what this module exists to show (`router/public.tsx:2243-2250`). Today its spinner comes from `isLiveConversation(row.state)` (`chat/list/public.tsx:43`, `thread/public.tsx:68`), so it turns while the summary is generated. S2 as written deletes `isLiveConversation` and makes every row read `activity.cards.get(row.id)` — but the launchpad track is not in `useWorkspace().tracks` (system area is filtered by `GET /api/areas`; pinned by `today-conversation.test.tsx:37-41`) and the Today route has no `detailActivity`. An implementer who threads `cards` only through the track route (the only site §5.3/§6 name) leaves this list with `cards = {}` → no working/failed mark while the summary runs, no `', working'` label, and the drawer `ChatThread live` for the summary conversation is also dead. The data does exist: `overlaysByKind('track')` (`queries.ts:537-542`) has no area filter (`read.rs:914-923`) and the tick set is "全部未归档 track" — so `trackActivityFrom(conversationTrackId, overlays)` is available on Today; it is just not written down. `INV-APP-118` ("只从 overlay 推导") would then be stated for a surface that has no overlay input.
- Required change: §5.3 CONVERSATIONS row — add the third site explicitly: "Today 的 launchpad 会话列表与抽屉（`router/public.tsx:2256-2307`，同一 `ChatList`/`ChatThread`）读 `trackActivityFrom(conversationTrackId, overlaysByKind('track'))` 的 `cards`（launchpad 不在 `workspace.tracks` 里，overlays 查询无 area 过滤）"; make the `cards` map an input of `useConversationPanel`'s `source` (both callers must supply it — no default). §6 S2: add `router/public.tsx` Today route to the file list and a must-red: `today-conversation.test.tsx` "the launchpad summary row reads the launchpad activity overlay" (`cards[SUMMARY_CONVERSATION]='working'` → `[data-nc-activity="working"]`; `state:'turn_pending'` with no entry → none). §7: one Today-conversation line (summary running → `working`; completed → `unread` via `last_turn_completed_at`).

### MINOR-1 §7 A2's Today `unread` is not derivable: Today's `renderTrackRow` passes no `unread` today
- Where: §5.1 ("Today 行 … 都调它"), §5.3 Today row, §7 A2 Today column; code `router/public.tsx:2453-2465` (`<TrackRow track variant hourLabel areaName onOpen onDelete>` — no `unread` prop; only the rail passes it, `sidebar.tsx:143`). F1.7 does not record this.
- Required change: F1.7 add "Today 的 `renderTrackRow` 不传 `unread`（`:2453-2465`）"; §5.3 Today row: "`unread = preferences.isUnread('track', id, track.activityAt)` 由 router 传入（同 rail）"; §6 Today must-red adds "`activityAt > receipt` 的行 `data-nc-activity=unread`". S3's `mobile-tracks.tsx` needs the same input (say so in the S3 row).

### MINOR-2 Wakeup table lacks `task.gate_result`; a parent gate finishing (`verifying → done/failed`) is tick-delayed and the §7 has no gate row
- Where: §4.3 唤醒表; code `operation/task_verify_adapter.rs:203-238`: `task_apply_gate_result_tx` then the event batch is `[TaskGateResult]` plus a `track.lifecycle_changed` only if the track is still `Working` (`auto_transition_if_current_in_tx(Working→Reviewing)`, `:231-238`). On a `planning` track (the §1 shape) nothing in the table fires → `working=false` + E3 unread wait for the ≤30 s tick. Not wrong (tick corrects), but the v6 `verifying`-is-working rule makes gate completion a visible edge with no wakeup.
- Required change: add `task.gate_result` (payload `task_id`; scope `EventScope::Track`) to the wakeup table; optionally a §7 line C5 "gate 通过 → `unread`（E3，唤醒 `task.gate_result`）".

### MINOR-3 † exception: make the empty-W-set semantics explicit
- Where: §4.2 活会话门段 ("W 结果集中 `worker_card_id = 该卡` 的行全部 `status='done'` 且 `ws.created_at_ms <= MAX(finished_at_ms)`"), §6 `superseded_failed_attempt_session_is_not_actionable`.
- Evidence: in SQL, `MAX()` over zero rows is NULL and `x <= NULL` is not true, so an unbound/superseded card is never suppressed; in Rust, `iter().all(done)` is vacuously true and `max()` is `None` — an implementer writing `max.map_or(true, …)` suppresses every `failed` session that has no current-attempt row, and the named must-red then stays green under the "S0 arms removed" mutation. Also the phrase "S0 第二臂" is undefined once the mutation removes the arms.
- Required change: one sentence: "至少一行；W 结果集中无该卡的行 ⇒ 不压（`MAX` 为 NULL）"; §6 row: name the empty-set case as the second assertion of `superseded_failed_attempt_session_is_not_actionable`.

### MINOR-4 A11y contract: the open drawer loses its only accessible "working" fact
- Where: §6 "S2 的 a11y 契约" ("在动的可访问事实放在行/summary 的可访问名里"); code `thread/public.tsx:592,604,669,979`, `quiet-sync.tsx:100` (today each is a `<span aria-label="Working">`, which `getByLabelText('Working')` finds); after the swap to the `aria-hidden` primitive (`ui/activity-indicator/public.tsx:8`) the thread region carries no text/label for it — the row label `', working'` lives in the list, which on mobile (`page/public.tsx:648` conversations panel) and in a focused drawer is not what the reader is on.
- Required change: name the carrier inside the thread (e.g. the pending-reply `<p className={styles.reply}>` at `:669` gets visually-hidden text "Working", or the composer footer status), or declare the loss in §9. Keep the 14 assertions' rewrite as stated.

### MINOR-5 Bookkeeping: §6 S1 gate list omits the B′ real-stack acceptance; drift registry is one commit stale
- §4.2.1 and §7 B′ make the interactive `codex-create` sequence "S1 必做", but the §6 S1 bullet (fmt/clippy/nextest/scripts/gen:api) does not list it, and the S1 slice row does not either — a brief built from §6 alone drops it. Add one line to the S1 gate bullet with the pass criterion (≥2 ticks at `idle`, then `unread`).
- §2 drift paragraph registers `3dbb96c84` (21 files); origin/main is now `f397061d2` (#1728, 35 files vs base). It moves `docs/architecture/1548-planner-terminal-wiring.md:683` (cited in §4.6 fix 2 for the +60 s `idle_prompt` measurement) to `:687`; `terminal_renderer/mod.rs:53-75` is unmoved (hunks at 22 and 125). Register the commit and the one anchor.

## Constructions tried (no defect found)
- `working`: harness `turn_pending`∧registry (superseded rows drop out via `cards.session_id`; `Resumed` decays to Idle after `resumed_reconcile_budget` 5 s → G15 window is short); (ii) codex-create stays `running` forever so `working` ≡ `last_thread_status='active'` after the feeder change; (iv) stale on-disk `Working` with a live PTY → declared (§6 runbook, §7 D); terminal cards never (v); planner never writes `running`. No new construction.
- `activity_at_ms`: user REST report edits are `EditAuthor::User` (`tracks.rs:4698-4700,4868`; `EditAuthor` serialises lowercase, so E7's `<> 'user'` is already the right literal); `auto_promote_draft_in_tx` (actor `Kernel`) is only reached with `auto_promote_draft=true`, which `report_op_attribution` grants to `CardRole::Planner` alone (`decision_sink.rs:606-616`; REST block ops pass `false`, `write.rs:352-358`) → a user's own edit cannot light E4/E7. `EVENTS_PRUNE_KINDS` and the `System`-scope fallback (`routes/codex.rs:266-276`) only lose evidence in the safe direction. Agent results: E1 (harness), E2, E3 (incl. gate), E5 (claude interactive), E6 (codex-create) each have a persistent carrier; gate completion is MINOR-2.
- Receipt baseline: two tabs (second sees the key, skips), fresh device, DB reset (`DATABASE_ID_KEY` overwritten on change; `dbInstanceId` still drives the clear/reload), layout-before-passive ordering, pending window (`isUnread → false`) — all hold.
- `card_fsm`: `Stop→Idle` + whitelist leaves no stranded card I could build; `exited` under `AwaitingInput` is cleared by the live gate at the next tick; `/clear` (new UUID) survives fix 3 via the card-level actor; `--resume` reuse is G14.
- Transaction shape: autocommit SELECTs + one `write_with_event_typed` matches the invariant's head comment (`deferred_write_tx_invariant.rs:26-34`: autocommit joins cannot be a cycle party).
- Precedence: `isRunning` consumers are exactly `track.ts:685`, `row/public.tsx:95`, `today/public.tsx:230`, `lifecycle-badge/public.tsx:23` (grep); `ActivityIndicator` mounts are `row/public.tsx:155,174` and `chat/list/public.tsx:96` only; every mapped surface reads `activityStateOf`/`cards`. The unmapped one is MAJOR-1.

## Open-question answers (the doc's §8, one line each)
- Q1 — agree closed: one baseline per `(device, databaseId)`, written in `setReadScope` from `/api/version` `nowMs`; old devices re-baseline once because the key prefix changes.
- Q2 — agree (c): live-session gate in the projector, FSM rows untouched; runbook's two exit paths (`exited` quiet / signalled `failed` red) are consistent with `attach_reader.rs:126-138`.
- Q3 — agree: `Notification` whitelist `{permission_prompt, elicitation_dialog, elicitation_url_dialog, agent_needs_input}`; `idle_prompt` at +60 s would otherwise undo decision (a).
- Q4 — agree: `starting` is not working (no bounded exit; `run_loop.rs:4705-4730` watchdog does not cover `PendingThreadStart`).
- Q5 — agree: primitive first, size at signoff; add the drawer's accessible carrier (MINOR-4).
- G19 — agree with the narrow rule + `created_at_ms <= MAX(finished_at_ms)`; add the empty-set clause (MINOR-3).

## Gate outputs (ratchets, last 3 lines each)
`scripts/gate-1316-terminology-ratchet.sh` (from worktree root):
```
    <retired-id-term>     web    0
    <retired-item-term>   web    0
OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope.
exit=0
```
`scripts/gate-prose-ratchet.sh`:
```
OK: agent-facing prose in *.rs under crates/ is at the #1635 baseline for every term.
exit=0
```

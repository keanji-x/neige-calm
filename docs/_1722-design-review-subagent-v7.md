<!-- archived review text, round 7, doc @ 880f67f74 (three substitutions for the #1316 ratchet: in the quoted gate-output block the two retiring vocabulary rows read `<retired-id-term>` / `<retired-item-term>` instead of the words themselves, and in the F2.35 EXPLAIN output the transcript table name reads `<转录表>` as the design doc itself writes it) -->

# Round 7 / channel A — verdict: APPROVE

Doc: `docs/architecture/1722-track-activity-indicators.md` @ `880f67f74` (worktree `1722-activity-design`); code = `b2341b871` (confirmed: `git diff --stat b2341b871 HEAD` touches only `docs/`). Lens: product/FE semantics + tests, fresh eyes. Zero BLOCKER, zero MAJOR; five MINORs below, none of which changes a mechanism.

## Verified facts (66 confirmed / 2 wrong)

Sampled with priority on rows the mechanisms rest on. Every `file:line` below was read on `b2341b871`.

**FE (19 rows, all confirmed):** F1.1 (`row/public.tsx:37,91,94-95,104-107,155,173-175`), F1.2, F1.3, F1.4 (`activity-indicator/public.tsx:3,7-8`), F1.5 (`chat/list/public.tsx:43-48,46,67`; `thread/public.tsx:68,659`), F1.6 (`router/public.tsx:282-284,346,2791-2798,2844`; `track_conversations.rs:366-389` — `role=Assistant`, LEFT JOIN four active states), F1.7 (`today/public.tsx:12-13,229-230,245,254,397-406`; `public.test.tsx:46-57`; `router/public.tsx:2453-2465` passes no `unread`), F1.8 (`sidebar.tsx:102,143,197-198`), F1.9 (`ui-preferences.tsx:12-17,45-57,59-71`; `production-app.tsx:69`), F1.10 (`providers/public.tsx:75,77-79,81,89,91,94-96`; `ui-preferences.tsx:105-107,144-156`), F1.11 (`:1335,1825,2719,2792`), F1.12 (`invalidation-plan.ts:250,262-268,264,323-331`; `queries.ts:537-542,993,998-1006`), F1.13 (`schemas.ts:246-258,251`; `model.rs:513,517-520`; `runtime.rs:44`; `session_projection_row.rs:12,28,62,100,122-129`; `event_serde_goldens.rs:321-332`), F1.14 (`router:2675-2703,2688-2692`; `page/public.tsx:71-77,817,830`), F1.15 (`terminal-card.tsx:46-47,62`), F1.16 (five `styles.live`: `thread/public.tsx:592,604,669,979` + `quiet-sync.tsx:100`), F1.17 (`mobile-tracks.tsx:7,73`), F1.18 (`page/public.tsx:211,552,558`), F1.19 (`tokens.css:227,228,398,405,462,524,525,539,542,544`; `check-contrast.mjs:175-180`; `fe-design.md:78`), F1.21 (`manifest.json` = 71 entries; `runner.ts:500-527`, `:504` patch-target==target; `run.mjs:70-75`; INV-APP-117/118 both unused; `owner-aliases.yaml:29` has `core/domain/overlay`).

**Kernel (27 rows, all confirmed):** F2.2 (`HARNESS_MODE` `harness.rs:13`; `read.rs:1051`; `shared_codex_appserver.rs:3996-3998`; `snapshot.rs:674`; `planner_harness_start_adapter.rs:1139,1648`; `harness/mod.rs:677` is `#[cfg(test)]`, `:866,1065` inside it; `claude_restart_adapter.rs:237` `spawn_op_id: None`), F2.3 (`state.rs:38-49`; `run_loop.rs:5145,2456-2465,2472,4481-4486`), F2.6/F2.40 (`session_mirror.rs:82,136-141,263-290`; `claude_restart_adapter.rs:157-168,223-241,238,493-498`; `session_projection.rs:198,675-684`; `claude_card_endpoint.rs:886-931` "exited / failed"; `model.rs:571-574`), F2.7 (`turn_outcome.rs:15-18` strips `items`/`itemsView` → `$.status` is top-level, E1 predicate shape correct), F2.9 (`card_fsm.rs:93,100-134,357,403,432-470`), F2.10/F2.11/F2.12 (`:149-175,230-334,329,705,817-847`), F2.14 (`:503-575`), F2.15 (`routes/codex.rs:150-160`; `calm-codex-bridge/src/main.rs:95-100`; `routes/threads.rs:18`), F2.18 (`ActorId` `#[serde(tag="kind", content="id")]` → `{"kind":"User"}`; `TrackReportEdited.author: EditAuthor` lowercase → `'user'`; hook events' `card_id`/`kind` at payload top level, goldens `claude_hook.min.json`), F2.20 (`events_prune.rs:78-79,112-123`: `track.*`/`task.*` not pruned), F2.21 (`state.rs:1401,1512,1660,1668-1672,1697`; `main.rs:50-51`), F2.22 (`task.rs:135-146,198-203,264-269`; `scheduler/mod.rs:1834-1843,1878-1900`; only writer of `'canceled'` is `task.rs:138`), F2.23/F2.24 (`version.rs:131,157`; `compatibility.rs:9`; `preflight.rs:290,295`; `ci.yml:1386-1432`), F2.25 (`liveness_feeder.rs:39-45,79-87,82,106-116,118-127`; `session_row.rs:396-423` no `updated_at_ms`), F2.26 (`session_projection.rs:124-142`), F2.28 (`user_notify.rs:12-18`; `conversation.ts:1258-1273` → `$.item.tool`, `$.item.error`, `$.item.status`), F2.30 (`reaper/mod.rs:28,120-127,172,205-238,265-298,361-369,607-615,632-635`), F2.31 (`attach_reader.rs:126-138`; `terminal_sweeper.rs:92-119`), F2.32 (seven `new_status: WorkerSessionState::` hits + `observe.rs:113-126`; `worker_flow/mod.rs:302` is a consumer; zero `WorkerSessionSuperseded` emitters), F2.35 (0032:29, 0094:34, 0081:49/75/78/99, 0097:84/107, 0001:52; **re-ran the E1 EXPLAIN on an in-memory sqlite with the §4.8 index → `SEARCH <转录表> USING INDEX idx_transcript_card_method_created_at (card_id=? AND method=?)`**), F2.36 (`emit.rs:57-58`; `decision_sink.rs:138,194`), F2.38 (`task.rs:94-108`; `scheduler/mod.rs:373-380,850,894-897,906-936,1692,1705-1713`), F2.39 (`scheduler/mod.rs:414-440,423-424,910-922,1052-1059`; `tests/scheduler.rs:8051-8077`).

**§2.4 / §4.3 / §5.3 / §6 citations (20, all confirmed):** `deferred_write_tx_invariant.rs:1-56` (autocommit SELECT + `begin_immediate_tx` satisfies it); `ci.yml:929-935,968,1030,1039,1151,1285`, and `scripts/run-rust-nextest.sh:42-44` is exactly the §6 S1 nextest line (`env -u NEIGE_CODEX_BIN … --features calm-server/codex-e2e --profile ci`, self-hosted adds `--test-threads 8`); `task_verify_adapter.rs:203-238` + `event.rs:1483` (`task.gate_result`, scope `Track`, lifecycle only when `Working`); `task.rs:468-478,500-513,551-580`; `registry.rs:221-232`; `today_summary.rs:639-666`; `today-conversation.test.tsx:37-41,51`; `today/README.md:201-202`; `read.rs:914-923` (no area filter); `router/public.tsx:1111-1122,1823,2150,2256-2307,2271,2310-2333,2467,2856`; `today/public.tsx:246`; `1548:683` (and `:687` on `f397061d2`); drift `origin/main = f397061d2`, 35 files; 14 `Working`-label assertions and 11 `'status Working'` assertions (counts reproduced); `is_isolated_card_tx` `lookup.rs:9-12`, `OPERATION_KIND` `mod.rs:25`; `dispatcher/mod.rs:988-999` kill-switch; `auto_promote_draft_in_tx` actor `Kernel` (`track_lifecycle.rs:27-39`) with every caller agent/kernel-driven (`decision_sink.rs:341`, `plan.rs:666`, `write.rs:894,958` — REST user entries pass `auto_promote_draft=false`, `write.rs:356-358`).

**Wrong (2, both citation-only, neither load-bearing):**
1. §7 A′: `useReadReceipt('conversation', …)` is at `router/public.tsx:1335-1336`, not `:1331-1332` (`:1331` is `useUiPreferences()`, `:1332` a comment). F1.11 has it right.
2. §4.2 live-session-gate paragraph and §11 R4 A-MAJOR-2: `input_authority.rs:31` lacks its directory — the file is `crates/calm-server/src/terminal_renderer/input_authority.rs` (`:31` does read `Self::InteractiveUser => true`).

## Findings

No BLOCKER. No MAJOR.

### MINOR-1 Terminal-card head has no named carrier for `activity.cards`
- Where: §5.3 "CARDS 行 / 终端卡头" row (`terminal-card.tsx:62` 换 `ActivityIndicator`).
- Evidence: `CardComponentProps` = `{card, host, onRemove}` (`systems/cards/registry.ts:37-49`); `KernelCardInput` = `{id, kind, payload, runtime}` (`:11-16`); `CardHostCapabilities` (`contracts.ts:51-65`) carries no track activity; `systems/**` sits below `app/**`. The only activity-shaped value a card can reach today is `card.runtime.status` (`terminal.ts:29` → `sessionState`) — exactly the second derivation INV-APP-118 forbids. The doc names the file but not how `cards.get(card.id)` arrives.
- Required change (one sentence in §5.3 + S2 file list): name the carrier — e.g. `KernelCardInput.activity?: CardActivity | null` filled by the board/composition layer from `detailActivity.cards`, or a `host.activity` port — and add the terminal-card mutation to §6 ("head derives from `card.runtime.status`" must red in `terminal-lifecycle.test.tsx`).

### MINOR-2 `items[].at_ms` for `source:'session'` is stale for feeder-derived conditions
- Where: §4.1 (`session → worker_sessions.updated_at_ms`).
- Evidence: the feeder's UPDATE writes `last_activity_ms,last_thread_status` and **not** `updated_at_ms` (`session_row.rs:403-408`; F2.25 says so). So a (ii) `systemError`/`waitingOn*` item's `at_ms` is the row's last non-feeder write, and the sidebar's `at_ms` sort puts a fresh `systemError` under older items. (`state='failed'` items are fine — `session_commit_exit_tx` sets `updated_at_ms`.)
- Required change: for items whose evidence is `last_thread_status`, use `COALESCE(ws.last_activity_ms, ws.updated_at_ms)` (same UPDATE writes both).

### MINOR-3 Archive/unarchive is neither a wakeup nor in the tick set
- Where: §4.3 wakeup table (`track.deleted` listed, `track.updated` not); tick set `archived_at IS NULL`.
- Construction: track archived while `working=true` → row leaves the tick set with that payload → unarchive → stale spinner until the next tick (≤30 s); while archived, the track page header (if opened by URL) shows the stale value indefinitely. Rail/Today are unaffected (`visibleTracks` filters archived, `track.ts:694-696`).
- Required change: add `track.updated` (archived_at change) as a wakeup, or state the ≤1-tick residue in G-list.

### MINOR-4 Two citation slips (listed under "Wrong" above)
- Required change: `:1331-1332` → `:1335-1336` in §7 A′; `input_authority.rs:31` → `terminal_renderer/input_authority.rs:31` in §4.2 and §11 R4.

### MINOR-5 §7 C3 skips the `pending` blink after recovery
- Where: §7 C3 ("重试 → `dispatched` → `working`").
- Evidence: the recovery tx allocates the attempt and rebuilds the projection in one tx (`task_recovery.rs:215-251`; `task_projection.rs:1769-1774` inserts the new attempt as `'pending'`); the claim to `dispatched` is a later scheduler tx. In between, W sees neither `failed` (old attempt left `current_tasks`) nor `working` (`pending` ∉ W) → rail is quiet/unread for one scheduler hop; the `+` lifecycle write in that tx is by the resuming actor (User) so E4 does not fire.
- Required change: one clause in C3 ("经 `pending` 一跳，W 两边都不给，随 claim 的 `task.dispatched` 唤醒到 `working`") or accept as a declared sub-second transient.

## Mechanism attacks (constructions tried)
- `working`: no construction found after trying stale `running`+`active` worker (W ignores sessions), superseded/old-attempt sessions (S0 requires `cards.session_id` and current attempt), `Resumed` after restart (G15 declared), terminal cards (never from session), planner `starting`/`idle`/`turn_pending` (registry gate), child-track `running` vs parent `verifying` (F2.38/F2.39 both read).
- `activity_at_ms`: no user-only trigger found — E4 excludes actor `User`; every `auto_promote_draft_in_tx`/`auto_transition_if_current_in_tx` caller is agent/kernel-driven; E7 reads `EditAuthor` (REST user paths are `User`); user cancel is excluded by `status IN (done,failed)`; interrupted turns excluded from E1; rejected `turn/start` writes no row. Every agent result has a lane (E1 harness, E5 claude interactive, E6 codex interactive, E3 all workers incl. terminal and child-track, E7 report, E2 notify).
- Receipt baseline: two tabs (second finds the key, no rewrite), fresh device (baseline=nowMs), db reset (`DATABASE_ID_KEY` overwritten on change → new key → one write), layout-before-passive ordering (`ui-preferences.tsx:105-107` vs `:147-156`) — no construction found. `receipt()` re-reads storage (`:53-55`) so cross-tab reads stay consistent.
- `card_fsm` fixes: with `Stop→Idle`, `PermissionDenied→AwaitingInput` lingers only until the next hook (`PreToolUse`/`Stop` downgrade); `exited` session ⇒ row ignored by the live gate, and a restarted card re-exposes the on-disk `AwaitingInput` only until `SessionStart` (+750 ms) — G10-class, declared. The SubagentStop mutation is real: same-state re-observe drops the pending downgrade (`card_fsm.rs:443-450`) so the named test goes red.
- Transaction shape: satisfies `deferred_write_tx_invariant.rs` (no `.begin(`/`begin_read_tx(`; one `begin_immediate_tx` write).
- Precedence/surfaces: `isRunning` consumers on the tree are exactly `track.ts:685`, `lifecycle-badge:23`, `row/public.tsx:95`, `today/public.tsx:230` — the doc's four; only the primitive writes `data-nc-activity`; `store.conversations` = server rows + injected planner row only (`router:696-706`), so §5.3's "two row sources" holds; the third site's `source.cards` is correctly required. Remaining second derivation: MINOR-1.

## Open-question answers (the doc's §8, one line each)
- Q1 (baseline per device): closed as written — per `(device, databaseId)`, once; older devices write once on upgrade day.
- Q2 (stale FSM rows): (c) — no rewrite; live-session gate + runbook two exits (`exited` → quiet; signalled → `failed` → red until restart/delete) is derivable from F2.31/F2.26.
- Q3 (Notification whitelist): closed — four types → `AwaitingInput`, others `None`; `idle_prompt` (+60 s) correctly excluded.
- Q4 (`starting` not working): closed — `PendingThreadStart` has no watchdog exit (B-M10), so counting it would strand.
- Q5 (primitive first, size on device): closed — plus the `:669` `VisuallyHidden` carrier; `ActivityIndicator` stays `aria-hidden`.
- G19: closed — narrow † exception with `created_at_ms <= MAX(finished_at_ms)` lower bound and explicit empty-set semantics; `superseded_failed_attempt_session_is_not_actionable` (2) pins the empty-set arm.

## Gate outputs (ratchets, last 3 lines each; run from the worktree root)
```
$ ./scripts/gate-1316-terminology-ratchet.sh   (exit 0)
    <retired-id-term>     web    0
    <retired-item-term>   web    0
OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope.

$ ./scripts/gate-prose-ratchet.sh   (exit 0)
OK: agent-facing prose in *.rs under crates/ is at the #1635 baseline for every term.
```
(Prose ratchet prints one line.) §6's S1 nextest line equals CI's `scripts/run-rust-nextest.sh:42-44` (+ `--test-threads 8` on self-hosted); S2/S3 lines match `ci.yml:929-935,968,1030,1039,1151,1285`.

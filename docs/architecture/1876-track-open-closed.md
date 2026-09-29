# #1876 — Track lifecycle becomes open / closed

**Owner rules.**
1. Simple first, pain points only. Use the fewest mechanisms. A hypothetical case is one line in
   KNOWN GAPS, not a mechanism. Prefer deleting a mechanism to guarding it.
2. Compatibility means only the 4140 database (`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`).
   There is no story for old clients, compat windows or migration matrices.
3. The agent-facing command surface is one consistent system. One option works on every object of
   the same kind, the CLI mirrors the MCP name through one shared renderer, and errors list the
   valid choices.

**Pinned by the owner.** A track is open or closed. One nullable `closed_at` (unix ms) replaces
`tracks.lifecycle`, `terminal_at` and `archived_at`, and is non-null exactly when the track is closed.
`TrackLifecycle`, its 9-state FSM and `archived_at` go away. The sidebar hides closed tracks by
default, except one that is unread or open in the view. The Area three-dot menu gets
Show closed / Hide closed. This work absorbs #1873 item 7: the ratify wait needs no lifecycle flips.

## 1. Facts

Verified at a0eb9cd6f by reading the code, or by the query shown against 4140 (2026-09-29).
`Q` stands for `sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`.

| # | Claim | Where / command |
|---|---|---|
| F1 | 28 tracks: done 22 (all 22 have `terminal_at`), draft 4 (one is the launchpad), planning 2 | `Q "select lifecycle,count(*),sum(terminal_at is not null) from tracks group by 1"` |
| F2 | `archived_at` is set on 0 rows. 0 tracks have a parent. 0 tasks have `child_track_id`. One `child-track` operation ever ran (succeeded, 2026-09-05); its tracks are gone | `Q "...where archived_at is not null"`, `"...parent_track_id is not null"`, `"select count(*) from tasks where child_track_id is not null"`, `"select phase,created_at_ms from operations where kind='child-track'"` |
| F3 | 406 `track.lifecycle_changed` rows. Each has a `track.updated` at `id+1` with the same `agent_message` | `Q "select count(*),sum(u.kind='track.updated'),sum(coalesce(json_extract(u.payload,'$.agent_message'),'')=coalesce(json_extract(l.payload,'$.agent_message'),'')) from events l left join events u on u.id=l.id+1 where l.kind='track.lifecycle_changed'"` → `406\|406\|406` |
| F4 | Edges by actor: 262 of 406 are kernel bookkeeping (draft→planning 44, dispatching→working 27, planning→dispatching 25, reviewing→working 45, working→reviewing 121). The Planner closed 41 times (→done 39, →failed 2). Only the user reopened (done→working 5, done→planning 1). Nobody ever canceled | `Q "select json_extract(payload,'$.from')\|\|'->'\|\|json_extract(payload,'$.to'),json_extract(actor,'$.kind'),count(*) from events where kind='track.lifecycle_changed' group by 1,2"` |
| F5 | 8 edges into `blocked`: 3 came with `ratify.requested`. The other 5 are Planner messages of the form "waiting for the user to …" | `Q` over `to='blocked'` joined to `ratify.requested` within 3 ids |
| F6 | The Draft hold never held a task. 14 tasks were created before their track left `draft`, and every one was created ≤20 ms before the edge, in the same write | `Q` comparing `tasks.created_at_ms` with the first `from='draft'` edge |
| F7 | The Blocked hold never held a task. Across 8 blocked windows, 0 tasks were created in a window and then waited. The one inside `affb2b97`'s window was declared by the same write that left `blocked` (1 ms apart, events 28537 and the task row) | `Q` over blocked windows × `tasks.created_at_ms` |
| F8 | 3 `ratify.requested` and 1 `ratify.resolved`. 2 requests are still pending, both on `done` tracks, and on both the user's latest Planner reply (U) is later than the request. `activity_dismissals` has 0 rows | `Q "select scope_track,kind from events where kind like 'ratify%'"`; U as in `track_activity/sql.rs:304-306`; `Q "select count(*) from activity_dismissals"` |
| F9 | Every write goes through `track_update_tx`. It refuses to reopen a child that a task references, stamps or clears `terminal_at`, and freezes the workspace once `lifecycle != Draft`. Leases and terminal rows freeze it themselves (`operation/workspace_lease/mod.rs:211`, `calm-truth/src/db/sqlite/card.rs:566`). A re-point also supersedes every live session and asks the disk whether the workspace is pristine (`routes/tracks.rs:2154-2190`). Task `cwd` is the Planner's declared value, not the workspace (`task_projection.rs:1673`). 14 of the 15 non-draft managed tracks were frozen only by the draft exit and never had a lease or a terminal | `calm-truth/src/db/sqlite/track.rs:223`, `:254-264`, `:266-273`, `:322-328`; `Q` joining `tracks.workspace_frozen_at` to the first lease, the first terminal and the first `from='draft'` edge |
| F10 | The needs-a-person signal reads only the newest edge into `blocked` (N1) and the newest edge out of it (N1b). `reviewing` is **not** an `Input` item. The attention fold reads only `items[]` | `track_activity/sql.rs:261-280`; `track_activity/notifications.rs:117-133`; `track_activity.rs:123-133` |
| F11 | `calm.ratify.request` requires `working` and no pending request, then moves the track working→blocked. A grant moves blocked→working; a deny changes nothing | `mcp_server/tools/review.rs:249-277`; `routes/cards.rs:1010-1040`; `ratify_state.rs:7-21` |
| F12 | The scheduling gate is `planning\|dispatching\|working\|reviewing`. Callers: `scheduler/mod.rs:954`, `:1110`; `task_recovery/admission.rs:74-94`, `:183`, `:620`; `task_recovery/view.rs:258`; `track_report/user_start.rs:22-28`; `track_report/dispatch.rs:422` | `scheduler/mod.rs:133-142` |
| F13 | Kernel auto-moves. Claim: planning→dispatching→working and reviewing→working (`scheduler/mod.rs:1230-1252`). Working→reviewing at 8 sites: `decision_sink.rs:218-231`, `scheduler/mod.rs:2487-2503`, `scheduler/running_worker.rs:402-408`, `scheduler/worker_failure.rs:43-56`, `operation/task_verify_adapter/mod.rs:187-193`, `git_candidate/action.rs:442-448`, `operation/workspace_lease/release.rs:303-309`, `reaper/mod.rs:544-557`. Auto-promote draft→planning (`track_lifecycle.rs:15-28`) is called at `track_report/write.rs:398`, `:462`, `decision_sink.rs:314`, `mcp_server/tools/plan.rs:560`; user start at `track_report/write.rs:518-531` | read |
| F14 | The dead-root reaper moves draft/planning → failed | `reaper/mod.rs:395-476`; `calm-truth/src/db/sqlite/session_repo_impl.rs:173-254` |
| F15 | Child outcome. The parent task succeeds (or goes to `verifying` with a gate) when the child is `done` with no pending or in-flight task. `done` with pending tasks fails it as `child-track-incomplete`. `failed`, `canceled` and deleted children fail it with distinct codes. The live trigger is `TrackLifecycleChanged`; the sweep runs every 300 s. 11 of the 22 `done` tracks have a `failed` current task, so a failed task does not mean a failed track | `scheduler/mod.rs:304-436`, `:746-892`; `dispatcher/mod.rs:84-90`, `:1035-1037`; `Q "select count(distinct t.id) from tracks t join current_tasks ct on ct.track_id=t.id where t.lifecycle='done' and ct.status='failed'"` → 11 |
| F16 | Child bootstrap failure forces the child to `failed` directly, without FSM validation | `scheduler/mod.rs:1533-1568` |
| F17 | Planner surface. `lifecycle` takes effect on `calm.task.verdict`, `calm.plan.cancel`, `calm.report.{write,edit,commit,blocks.upsert,write_markdown}`. It is ignored on the retired `calm.dispatch_request` and `calm.plan.upsert`. It is parsed by `lifecycle_args.rs:11-117`. `calm.track.state` is read-only. The CLI `state` renders one `lifecycle` line (`mcp_server/cli/render.rs:129`, `:174`), `log` renders the commit's `lifecycle` (`:419-426`), and `calm.area.outline` returns `lifecycle` (`report_links.rs:112`). The CLI table already has write verbs: `tag`, `task-completed`, `task-failed`, `track-gc` (`cli/commands.rs:168-229`). The dispatch and repair responses echo `lifecycle`, `archived_at` and `lifecycle_allows_scheduling` (`track_report/dispatch.rs:422`, pinned by `tests/cases/candidate_review_dispatch.rs:27`; `track_report/repair.rs:68`). `calm-exec` has `DecisionIntent::LifecycleTransition` (`reaction.rs:17`), used only by `calm-truth-test-harness/src/fakes.rs` (`set_lifecycle`, `:283-741`) | `tests/goldens/mcp_tool_registry.json:86,134,245,630,689,800,994,1048,1514` |
| F18 | The argument parsers ignore unknown keys: "unknown arguments are ignored, as in sibling tools" | `mcp_server/tools/preview.rs:3`; `lifecycle_args.rs:11-35` |
| F19 | REST `PATCH /api/tracks/{id}` takes `lifecycle` and `archived_at`, and refuses `lifecycle` on area-chat tracks. It is the only FE lifecycle write: "Resume work" sends `{lifecycle:'working'}`. `can_resume` = resumable ∧ ¬area-chat ∧ ¬(terminal ∧ referenced child) | `routes/tracks.rs:2513-2710`, `:2566`; `fe/web/src/app/router/public.tsx:2374`; `calm-truth/src/db/sqlite/read.rs:288-294` |
| F20 | Closed-track readers: boot harness recovery and planner takeover skip `done/canceled/failed` (`session_projection.rs:786`, `read.rs:832`, `session_system_error_recovery.rs:51`). The terminal sweeper ends workers on `done` or archived tracks (`terminal_sweeper.rs:73-74`). The run loop drops commit wakes on `done` (`harness/run_loop.rs:2743`). The activity card filter covers `done ∨ archived` (`track_activity.rs:261`), the tick covers unarchived tracks (`track_activity/sql.rs:110`), and the range query uses `terminal_at` (`read.rs:210`) | read |
| F21 | The activity unread evidence E4 is the newest non-user lifecycle edge (`track_activity/sql.rs:235-239`). The Today summary counts lifecycle changes (`activity_window.rs:13-113`) | read |
| F22 | `track_vcs_commits.lifecycle TEXT NOT NULL` and `event_id` are part of the commit hash. The hash is computed only when a commit is written and is never recomputed from stored rows (`commit_hash_for_tree` has 1 production caller). `backfill_existing_tracks` only covers tracks with no ref. Two commits cite a `track.lifecycle_changed` row (event ids 18165 and 18760). Each has its paired `track.updated` at `id+1` on the same track | `calm-truth/src/track_vcs/store.rs:61-118`, `snapshot.rs:38-48`; `Q "select c.event_id,(select kind from events u where u.id=e.id+1),(select u.scope_track=e.scope_track from events u where u.id=e.id+1) from track_vcs_commits c join events e on e.id=c.event_id where e.kind='track.lifecycle_changed'"` → `18165\|track.updated\|1`, `18760\|track.updated\|1` |
| F23 | No index, view, trigger, FK or CHECK names `lifecycle`, `terminal_at` or `archived_at` on `tracks` or `track_vcs_commits`. Bundled SQLite is 3.46.0 (libsqlite3-sys 0.30.1 via sqlx `sqlite`), which supports DROP COLUMN. Precedents: 0024, 0073, 0075 | `Q "select type,name from sqlite_master where type in ('view','trigger','index') and (sql like '%lifecycle%' or sql like '%terminal_at%' or sql like '%archived_at%')"` → empty; `grep SQLITE_VERSION ~/.cargo/registry/src/*/libsqlite3-sys-0.30.1/sqlite3/sqlite3.h` |
| F24 | Stored events that no longer match `Event` are skipped with an `error` log | `calm-truth/src/db/sqlite/events.rs:715-722` |
| F25 | FE. `sortByLifecycleRank` has no callers. `isWaitingForUser` (blocked ∨ reviewing ∨ failed) is used only by the badge tone. The rail filters `visibleTracks` (archived) before #1870's top-5 cut, pinned by `area-group.test.tsx:138-143`. `isUnread` is `ui-preferences.tsx:97-101`. Per-Area prefs use `area:${id}` (`:114-118`). #1870's Show more is component state (`area-group.tsx:52`). The Area menu has Edit / Delete only (`area-group.tsx:83-98`) | `fe/core/domain/track.ts:573-618`; `fe/web/src/app/shell/sidebar.tsx:81-85`, `:238` |

## 2. Decisions

### D1. Needs a person: derive it from what is actually waiting

Every current producer, and what replaces it:

| Producer | Today | After |
|---|---|---|
| `calm.ratify.request` (F11) | working→blocked, then `ask:lifecycle` item | No flip. Precondition: open ∧ no pending ratify. New item `ask:ratify:<events.id>` for the newest `ratify.requested`, text = `reason`, under the notify rule: open while its `at > U` and no later `ratify.*` exists |
| Planner `lifecycle:"blocked"` (F5, 5 uses) | ask item with `agent_message` | Deleted. The Planner asks with `calm.user.notify`, which is already an ask open until the user replies |
| Planner `lifecycle:"reviewing"` and 8 kernel working→reviewing sites (F13) | no kernel reader; FE badge tone only (F10, F25) | Deleted |
| User / Planner / grant leaving `blocked` (N1b) | closes the ask | Deleted, along with L. `answered` becomes U alone |

- `is_item_key` swaps its `ask:lifecycle:` prefix for `ask:ratify:`. There are 0 dismissal rows (F8).
- `track_for_event` (`track_activity.rs:545-576`) gains `RatifyRequested` and `RatifyResolved` arms,
  so the ask appears and clears without waiting for the 30 s tick.
- On 4140 the new source yields 0 items: on both pending requests U is later (F8).
- No stored flag, and no track-state term, which keeps D5's "items are not filtered" true.

### D2. Scheduling gate: open schedules, closed does not

`lifecycle_allows_scheduling` becomes `Track::is_open()` (`closed_at.is_none()`) at every caller in F12.

- The Draft and Blocked holds never held a task on 4140 (F6, F7), so no "paused while waiting for
  ratify" state is kept.
- `task_recovery::recovery_policy` loses `resume_blocked` and its Blocked branch. Recovery on a
  closed track refuses with "track is closed; reopen it first".
- `user_start.rs` refuses only a closed track.
- The `calm.task.dispatch` and repair responses (F17) echo only `"track": {"closed_at": …}`.
  `lifecycle_allows_scheduling` leaves the output.

### D3. Child outcome: "child closed" and quiescent; no stored outcome (orchestrator: decided)

The guards key on `child.closed_at IS NOT NULL` with the same quiescence subqueries:

- No pending and no in-flight task → success (`verifying` with a gate).
- Any pending task → `child-track-incomplete`, as today.
- Deleted → `child-track-deleted`.

`ChildTerminalOutcome` shrinks to `Deleted`.

- No "failed task" term: 11 of 22 done tracks carry a failed current task (F15).
- F16 becomes a kernel close of the child. The parent task is already failed in that transaction.
- The rule that a referenced child cannot be reopened stays (F9).

### D4. Who closes and reopens

- **User.** `TrackPatch` drops `lifecycle` and `archived_at` and gains `closed: bool`; the server
  stamps the time.
  - Area-chat tracks refuse `closed`, as `lifecycle` is refused today.
  - `can_resume` becomes `can_reopen` = closed ∧ ¬area-chat ∧ ¬referenced child.
  - "Resume work" becomes **Reopen**. **Close** is a new track action in PR-2.
- **Planner.** New tool `calm.track.close {message}` (Planner-only) emits `track.updated` with
  `agent_message`. Closing a closed track is a no-op that returns the current `closed_at`.
  - `lifecycle` leaves the schema of every tool in F17.
  - The shared parsers (`parse_write_args`, `parse_optional_write_args`) refuse a `lifecycle` key:
    ``"`lifecycle` is removed: close with calm.track.close; ask with calm.user.notify or calm.ratify.request"``.
    The parsers ignore unknown keys (F18), so without this a session started before the deploy
    would lose its close silently. It is one parser, so every write tool behaves the same.
  - The Planner cannot reopen. Only users ever did (F4).
- **CLI and agent reads.** Every agent-facing surface names the state `closed_at`:
  - `calm.track.state` (the Track row), `calm.area.outline`, and `neige state`.
  - `neige state` renders `closed_at  -` or `closed_at  <iso>` through `render::state` (one line,
    same grep contract as today's `lifecycle` line).
  - `neige log` and `calm.track.log` drop the per-commit `lifecycle` (F22).
  - The CLI gets `track-close --message <text>` in the same `COMMANDS` table as `task-completed`
    (`OptValue::Text`, required; `Render::Raw`), so `neige track-close` mirrors `calm.track.close`.
    Role refusal comes from the tool, as for every CLI verb.
- **Reaper.** `sweep_dead_roots`, `converge_dead_root`, `dead_root_candidates` and
  `DeadRootCandidate` are deleted (F14).
- **Auto-promote.** Deleted with the kernel auto-moves (F13), and so is the draft-exit freeze in
  `track_update_tx` (F9). It is redundant: the two durable workspace consumers, leases and terminal
  rows, freeze in their own transactions. A re-point already fences live sessions and checks the
  disk. Task `cwd` does not derive from the workspace. The 14 tracks it alone froze never had a
  lease or a terminal (F9).
- **`calm-exec`.** `DecisionIntent::LifecycleTransition` and the harness `set_lifecycle` fakes are
  deleted (F17).
- **Events.**
  - `TrackLifecycleChanged` is deleted. `track.updated` carries `closed_at` and `agent_message`.
  - The dispatcher adds `track.updated` to `SCHEDULER_TRIGGER_KINDS`. It pokes the scheduler and
    calls `reconcile_child_track` when `closed_at` is set (today `dispatcher/mod.rs:1035-1037`).
  - `track_vcs/delta.rs:352` keeps `TrackUpdated` only.
  - E4 is deleted. A Planner close happens inside a Planner turn, so E1 (turn completed) already
    covers it (F21). The Today summary drops its lifecycle-change line.
- **Versions.**
  - The migration deletes the 406 `track.lifecycle_changed` rows (F3, F24). Their `from`/`to` is
    also in the paired `track.updated`.
  - This is a `DELETE`, not an `UPDATE … kind`, so `gate-sync-event-version-lockstep.sh` R3 does
    not apply and `SYNC_EVENT_VERSION` stays 21. `track.updated` is already decoded leniently:
    `Track` is not `deny_unknown_fields`, and the FE invalidates and refetches on it.
  - `WEB_COMPAT_VERSION` goes 33→34 and `REST_API_VERSION` 14→15, because the REST shape changes.
- **Prompts.** The full sweep is `git grep -n -w -E 'lifecycle|lifecycles|blocked|reviewing|Reviewing|Lifecycle' -- crates/calm-server/prompts crates/calm-server/templates`.
  - `planner.md` lines 3, 5-20, 58, 67, 75, 81, 82, 109, 110, 122, 133, 137, 144 and 166.
    Lines 5-20 shrink to about 4: close with `calm.track.close`; ask with `calm.user.notify`, or
    with `calm.ratify.request` for a gated action; only the user reopens. The others lose their
    lifecycle clause: state reads (58, 144), `lifecycle` args (67, 75, 81, 109, 110, 166),
    "`done`/`failed`/`blocked`" (82, 133, 137) and the badge (122).
  - `prompts/tools/` (one line each):
    - Remove the `lifecycle` arg or transition from `calm.report.{commit,edit,write,write_markdown,blocks.upsert}`,
      `calm.plan.cancel`, `calm.task.{verdict,dispatch}` and `calm.ratify.request`.
    - Remove the lifecycle clause from `calm.task.{repair,delivery}`.
    - `calm.track.state` and `calm.area.outline` show `closed_at` instead.
    - `calm.user.notify` drops its "`blocked`" clauses.
    - `calm.report.blocks.{delete,move}` keep "takes no `message`"; the `lifecycle` clause goes.
    - Add a new `calm.track.close.md`.
  - `assistant/ordinary-head.md:3`, `launchpad-head.md:5`, `mechanics.md:10`: "lifecycle" becomes "open/closed state".
  - `templates/builtin/issue-development.md` lines 63, 102-106, 148-154: all flips go, and give-up
    becomes "close with a rationale". `investigation.md`, `small-change.md` and
    `investment-research.md` have no hits.

### D5. Activity terminal filter

`track_activity.rs:261` becomes `rows.track.closed_at.is_some()`. The tick sweeps every track:
`unarchived_track_ids` loses its `WHERE`, because done tracks are swept today and archived ones do
not exist. Items stay unfiltered, as today (`notifications.rs:117-119`).

### D6. Frontend (follows `fe/AGENTS.md` and the layer files)

- **core** (`fe/core/domain/track.ts`)
  - `lifecycle`, `archivedAt` and `terminalAt` become `closedAt: number | null`.
  - Delete `trackLifecycleSchema`, `isWaitingForUser`, `lifecycleRank`, `sortByLifecycleRank`,
    `isRunning`, `isTerminal` and `lifecycleLabel`.
  - Add `isClosed(track)`.
  - Add `railAreaTracks(sorted, activeTrackId, isUnread, showClosed)` in PR-2. It keeps
    `open ∨ unread ∨ active` unless `showClosed`.
  - `activeTracksOn` uses `closedAt ?? nowMs`.
- **Labels**
  - The rail and Today phrases are aria-label only (`row/public.tsx:67,75`). They say ", closed"
    for a closed track and nothing otherwise.
  - Mobile tracks meta (`mobile-tracks.tsx:84`) shows "Closed" or nothing.
  - The track header badge renders only when closed, with a neutral tone.
  - Today's Open group is `!isClosed`.
- **Show closed toggle.** It is per Area, stored in ui-preferences as `area-closed:${id}`, default
  false. That matches `area:${id}` expansion, and the menu item already lives in each Area's own
  menu (F25). It is a persisted preference, unlike #1870's in-memory Show more.
- **PR-1 keeps the rail's behaviour.** `visibleTracks` filters `closedAt === null`, as it filters
  `archivedAt === null` today.
- **With #1870 (PR-2).** `railAreaTracks` replaces `visibleTracks` before `limitAreaTracks` (the
  `sidebar.tsx:238` slot). So a hidden closed track is never counted in `Show N more`, and the
  active-row rule still holds. Waiting on you and Pinned are unchanged.
- **Layers.** Generated types come from `npm run gen:api`.
  - Frozen paths need `OWNERSHIP-CHANGE` trailers: `fe/core/api/generated/*`,
    `fe/core/api/schemas.ts`, `fe/core/events/invalidation-plan*.ts` (the `track.lifecycle_changed`
    plan goes).
  - The preference lives in `app/providers/ui-preferences.tsx`, and the menu and filter wiring in
    `app/shell/area-group.tsx`.

### D7. Migration (number assigned last; today's next free is 0123)

```sql
ALTER TABLE tracks ADD COLUMN closed_at INTEGER NULL;
UPDATE tracks SET closed_at = terminal_at WHERE lifecycle IN ('done','canceled','failed');
ALTER TABLE tracks DROP COLUMN lifecycle;
ALTER TABLE tracks DROP COLUMN terminal_at;
ALTER TABLE tracks DROP COLUMN archived_at;
ALTER TABLE track_vcs_commits DROP COLUMN lifecycle;
UPDATE track_vcs_commits SET event_id = event_id + 1
 WHERE event_id IN (SELECT l.id FROM events l JOIN events u ON u.id = l.id + 1
                     WHERE l.kind = 'track.lifecycle_changed' AND u.kind = 'track.updated'
                       AND u.scope_track = l.scope_track);
DELETE FROM events WHERE kind = 'track.lifecycle_changed';
```

- On 4140: `archived_at` is set on 0 rows (`Q "select count(*) from tracks where archived_at is not null"` → 0),
  and all 22 terminal rows have `terminal_at` (F1). 22 rows close and 6 stay open.
- DROP COLUMN is feasible (F23). There is no table rebuild, so the inbound FKs from `cards`,
  `tasks` and others do not matter.
- Event ids: the 2 commits that cite a deleted row move to its paired `track.updated` (F22).
  The commit hash is never recomputed, so moving `event_id` breaks no stored hash.
  `task_candidate_decisions` cites decision events only. After the migration, 0 commits cite a
  missing event (must-red row 7).
- Released migrations stay byte-frozen. `head_schema_fixture.rs:7-63` gains the filename, and the
  migration-replay seed `tests/fixtures/migration_replay/core.json` gains `closed_at`.
- Raw SQL sweep: `git grep -n -E "lifecycle|terminal_at|archived_at" -- 'crates/*/src/**'` covers
  every string in F9, F12-F16 and F20-F22, plus `TRACK_SELECT_COLUMNS` (`calm-truth/src/db/rows.rs:66-75`),
  the create insert (`track.rs:124-133`), `routes/today.rs:281`, and the dev `bin/replay.rs:278-365`
  force-lifecycle endpoint, which is deleted.

## 3. Source-invariant gate scan

| Gate / pinned artifact | Touched | Why |
|---|---|---|
| `tests/goldens/track_fsm_edges.json` + `tests/cases/track_fsm_golden.rs` | yes, deleted | FSM gone |
| `tests/goldens/events/track_lifecycle_changed.{full,min}.json` | deleted | kind gone |
| `tests/goldens/events/track_updated.{full,min,legacy_template_id,legacy_template_input}.json` | regenerate | `closed_at` |
| `tests/cases/event_serde_goldens.rs:1231-1236`, `:1316-1317` | yes | `ALL_KIND_TAGS` 54→53, file count 82→80 |
| `tests/goldens/mcp_tool_registry.json` (9 tools, F17) + `calm.track.close` | regenerate | `REGEN_MCP_TOOL_REGISTRY_GOLDEN` |
| `tests/fixtures/plan_upsert_input_schema.json:62-63` | yes | lifecycle enum |
| `tests/goldens/issue_development_planner_prompt.txt`, `assistant_prompt{,_launchpad}.txt` | regenerate | prompt text |
| `plugin_host/manifest.rs:1845-1875` `shipped_git_forge_give_up_uses_retained_lifecycle_tool` | rewrite | give-up = `calm.track.close` |
| `planner_card.rs` `planner_prompt_names_only_tools_the_planner_role_can_see` | runs | the new tool must be Planner-visible |
| `tests/vectors/gate_denials/*.json`, `principal_delta/0{1..5}_*.json` | yes | `track.lifecycle_changed` frames; commit needs `FROZEN-VECTOR-CHANGE:` (`scripts/ci/frozen-vector-gate.sh`) |
| `tests/fixtures/migration_replay/core.json`, `tests/cases/head_schema_fixture.rs` | yes | new migration |
| `tests/fixtures/events/{schema_forward_compat,track-grid-layout-trace}.events.json` | check | carry `archived_at` (lenient decode) |
| `calm-truth/tests/track_write_point_registry.rs` | no | pins workspace columns only; `:428` is a near-miss example |
| `tests/cases/fork_guard_exemption_invariant.rs:64-66` | optional | forbidden list names `TrackLifecycle` |
| `scripts/ci/ratchets/*` | no hits | `report_write_boundary_selftest.sh` seds a copy of `track_report/write.rs`; re-run it |
| `gate-sync-event-version-lockstep.sh` | runs, no bump | D4 |
| `gate-web-compat-version-lockstep.sh` | yes | 33→34 in both declarations |
| `gate-1316-terminology-ratchet.sh`, `gate-prose-ratchet.sh` | re-baseline if counts move | deleted Rust literals lower `long_literal` |
| `fe/tools/mutation/manifest.json` (PR-1) | yes | s2a-track-activity-state-from-lifecycle, s2a-activity-overlay-plugin-gate-dropped, s2a-needs-attention-lifecycle-or, n1829-decoder-accepts-v1 (`track.ts`), s2a-track-row-name-from-lifecycle, s2a-badge-running-tone-restored, s2a-today-groups-by-working, s4-today-second-count-from-phase, s3-mobile-track-row-from-lifecycle, n1829-row-placeholder-text, s2b-mobile-painter-no-indicator |
| `docs/oracle/app-dataflow.yaml` CAP-APP-032 (155-163), INV-APP-118 (636-679); `owner-aliases.yaml:104-105` (PR-1); `a11y-contract.yaml` INV-A11Y-061 (PR-2) | yes | `fe/tools/oracle/validator.ts` checks cited line ranges |
| `docs/oracle/capabilities-e2e.yaml:22` | yes | cites `bin/replay.rs` route lines; the force-lifecycle route goes |
| `fe/web/src/app/events/README.md:88` | yes | `track.lifecycle_changed` row |
| `tests/cases/candidate_review_dispatch.rs:27` | yes | pins `lifecycle_allows_scheduling` in the dispatch response |
| `fe/core/api/generated/{wire.ts,openapi.json}` | regenerate | `openapi-drift` job |
| `e2e/cases/110-multitask-golden-path.sh:41-83`, `fe/e2e/track-lifecycle-resume.spec.ts` | yes | tier 2 / Playwright |

## 4. CI gate list (`.github/workflows/ci.yml`)

- **changes**: classify affected CI surfaces.
- **lint**:
  - local Rust gate safety selftest; cargo fmt; frozen-vector gate selftest and gate; single migrations/ dir gate
  - harness raw status UPDATE gate
  - runtimes ratchet selftest, retirement ratchet and dropped-table ratchet
  - report-write boundary selftest and gate; append-seam boundary selftest and gate
  - no parallel operation-event entrance; token authority audit; no `raw_repo` escape
  - pgid escape gate; append-seam escape probes; pgid accessor ratchet
  - web compat lockstep; sync event version lockstep (+ discriminates); template rename residual
  - terminology ratchet (+ discriminates); prose ratchet (+ discriminates)
  - clippy `--workspace --all-targets --features calm-server/codex-e2e -D warnings`
- **rust-build**: nextest archive.
- **rust-shards**: 8 shards.
- **rust-main**: main push.
- **worker-boundary**.
- **rust**: aggregate.
- **web-unit**: legacy retired check.
- **fe-unit-lint**: npm ci, engines, lint, build.
- **fe-unit-test**: 3 shards.
- **fe-unit**: aggregate.
- **fe-browser**: PWA check, `test:browser`.
- **fe-mutation-plan**, **fe-mutation-shard**, **fe-mutation**.
- **e2e-release-build**.
- **fe-e2e**: Playwright against the docker stack.
- **stack-e2e-tier-1**: `./e2e/run.sh --tier 1` plus mobile pairing.
- **openapi-drift**: `npm run gen:api` + `git diff --exit-code -- fe/core/api/generated/`.

## 5. Caller sweep by directory

`A='TrackLifecycle|track_lifecycle|trackLifecycle|terminal_at|terminalAt|archived_at|archivedAt|lifecycle_changed|lifecycle_allows'`,
`B='\.lifecycle\b|\blifecycle\??:|lifecycleLabel|isWaitingForUser|isRunning\(|isTerminal\(|visibleTracks|can_resume|canResume'` (`git grep -P`),
`W='lifecycle|blocked|reviewing'` (`git grep -w -E`, prose). Counts are lines / files at a0eb9cd6f.

| Dir | A | B | W | Notes |
|---|---|---|---|---|
| crates | 1280 / 142 | 354 / 100 | — | non-test A: 471 / 69 (`grep -v -E "tests?\.rs:\|/tests/\|_tests\.rs:"`) |
| fe/core | 87 / 9 | 62 / 8 | — | |
| fe/web | 82 / 48 | 241 / 66 | — | about 51 test files |
| fe/e2e | 3 / 2 | 6 / 2 | 6 / 2 | |
| fe/tools | 2 / 1 | — | — | mutation manifest |
| plugins | 0 | 0 | 0 | `git-forge/manifest.json:290-292` only says ratify |
| templates (`crates/calm-server/templates`) | 0 | 0 | 9 / 1 | issue-development.md |
| prompts (`crates/calm-server/prompts`) | 0 | 0 | 41 / 19 | |
| scripts | 0 | 0 | — | the `lifecycle` hits are e2e-isolated modes |
| .github | 0 | 0 | 0 | |
| e2e | 0 | 2 / 1 | 13 / 3 | case 110 (tier 2), README |
| mobile | 0 | 0 | — | the 9 `lifecycle` hits are androidx |
| docs/oracle | 1 / 1 | — | — | plus the line-range cites: `app-dataflow.yaml` (CAP-APP-032, INV-APP-118) and `capabilities-e2e.yaml:22` (`bin/replay.rs`) |
| fe/web/src/app/events/README.md | 1 / 1 | — | — | line 88 |

## 6. Must-red table

Each row names the test, the single production mutation, and the tests predicted to go red. PR-1
rows are 1-7; PR-2 rows are 8-9.

| # | Test (new unless marked) | Mutation (production only) | Predicted red |
|---|---|---|---|
| 1 | `track_activity::notifications::tests::pending_ratify_is_an_open_ask_until_answered` | drop the ratify arm in `notifications()` | that test + `review_ratify::ratify_request_raises_an_ask_and_resolve_clears_it` |
| 2 | `scheduler::tests::closed_track_does_not_claim` | `Track::is_open` returns `true` (shared by every F12 gate and the ratify precondition) | that test + `task_recovery` `recovery_refuses_on_a_closed_track` + `isolated_codex` `first_start_refuses_on_a_closed_track` + `track_report` `user_start_refuses_on_a_closed_track` + `review_ratify::ratify_request_refuses_a_closed_track` |
| 3 | `scheduler.rs` `acceptance_18_success_flip_rechecks_closed_after_its_snapshot` (reshaped, reopen hook) | success guard drops `closed_at IS NOT NULL` | that test + `acceptance_18_production_reconcile_keeps_the_child_guard_wired` |
| 4 | `dispatcher::tests::track_updated_with_closed_at_reconciles_the_child` | delete the `TrackUpdated` arm | that test |
| 5 | `track_close::planner_close_stamps_closed_at_and_refuses_a_lifecycle_key` | the shared parser ignores `lifecycle` again | that test |
| 6 | `migration_nnnn_closed_at::terminal_rows_close_at_terminal_at` (seeds done, failed, canceled and working rows) | `WHERE lifecycle = 'done'` | that test |
| 7 | `migration_nnnn_closed_at::no_commit_cites_a_missing_event` (seeds a commit citing a `track.lifecycle_changed` row) | delete the `track_vcs_commits` remap statement | that test |
| 8 | `track.test.ts` `railAreaTracks keeps open, unread and active tracks` | drop the `unread` clause | that test + `area-group.test.tsx` "shows a closed unread track" |
| 9 | `area-group.test.tsx` "Show N more never counts a hidden closed track" | apply `railAreaTracks` after `limitAreaTracks` | that test |

`acceptance_17_raw_lifecycle_writer_refuses_reopen_of_referenced_child` is kept and renamed. Its
mutation is deleting the guard in `track_update_tx`, and the predicted red set is that test.

## 7. Slices

| PR | Content | Size (hand-edited, estimate) | Preview |
|---|---|---|---|
| PR-1 | Migration, calm-types/truth/exec/server kernel and `calm-truth-test-harness` fakes (D1-D5), event deletion, `calm.track.close` + `neige track-close`, parser refusal, CLI render, prompts and template, goldens and vectors, and generated wire. FE compile fixes: decoders, labels, badge, Reopen, `activeTracksOn`, invalidation, Today Open, mobile meta, the rail filtering `closedAt === null`, and fixtures (about 51 test files). The existing mutation-manifest entries and oracle INV-APP-118 / CAP-APP-032. The e2e case 110 and the Playwright resume test | ~2k lines touched, plus about 1.2k deleted as whole files (`track_lifecycle.rs` ×2, FSM golden and test, lifecycle-badge) | no |
| PR-2 | `railAreaTracks`, `area-closed:${id}` preference, Show closed / Hide closed item, Close track action, a11y oracle INV-A11Y-061, new mutation entries, jsdom + browser tests | ~400 | owner preview |

- **Why PR-1 is not split to ~1k.** Deleting `TrackLifecycle` is one compile unit across 3 crates.
  The wire change forces the FE decoders into the same PR (`openapi-drift` + `fe-unit-lint`).
- Any earlier cut ("stop the auto-moves first") would need a throwaway FSM, for example
  planning→done, and a second prompt rewrite. That is fragmentation, not a smaller change.
- Reviewers get PR-1 as five file groups: migration+truth, kernel semantics, tool surface+prompts,
  wire+FE compile, pinned artifacts.

## 8. Overlap with the parallel #1873 session (items 1, 2, 3, 5, 6)

| File | #1873 | #1876 | Plan |
|---|---|---|---|
| `calm-server/src/routes/cards.rs:1010-1040` | `message` on `RatifyResolved` | removes the grant flip in the same block | **same hunk**: land #1873 first, then PR-1 rebases |
| `calm-types/src/event.rs` | `RatifyResolved.message` | deletes `TrackLifecycleChanged` | separate hunks |
| `calm-server/src/dispatcher/mod.rs` | observation `message` (~1633) | trigger kinds (84-90), arm (1035) | separate hunks |
| `templates/builtin/issue-development.md` | gh-CLI and preview lines | rewrites 61-64, 92, 102-106, 146-154 | adjacent: #1873 first |
| `tests/goldens/events/*`, `event_serde_goldens.rs:1316-1317` | adds a with-message golden (82→83) | deletes 2 (→81 after both) | regenerate after rebase |
| `goldens/mcp_tool_registry.json`, the planner prompt golden, `fe/core/api/generated/{wire.ts,openapi.json}`, `fe/core/api/schemas.ts` (+ contract test) | yes | yes | regenerate after rebase, never hand-merge |
| `claude_cards.rs`, `claude_planner/spawn.rs`, `track_publish.rs`, `forge_git.rs`, `calm.track.publish.md`, `plugins/git-forge/*` | yes | no references | none |

Merge order: #1873, then #1876 PR-1, then PR-2. #1873 has no migration, so the number stays last-assigned here.

## 9. KNOWN GAPS

- A child closed as a failure counts as success; the parent verdict is the check.
- A root whose Planner never starts stays open with no item. It was `failed` before (F14).
- The mobile Area page and Today do not hide closed tracks. Only the desktop rail does.
- Closing a track ends its worker terminals (F20). Today only `done` and archived tracks do that.
- A Planner cannot reopen. A user message to a closed track's Planner cannot schedule until the
  user reopens.
- A tab that resumes from a pre-migration cursor gets old `track.updated` frames with no
  `closed_at`, which read as open until the refetch the same event triggers.
- The 406 deleted `track.lifecycle_changed` rows lose only their `from`/`to` (F3).
- A managed track with no lease and no terminal can be re-pointed after its first Planner write
  (the draft-exit freeze is gone, F9).
- A child task that finishes is still not a live trigger for its parent (existing, F15). The
  300 s sweep covers it.

## 10. OWNER DECISIONS

Decided by the orchestrator (round 1): the child outcome stays derived (D3), the migration deletes
the `track.lifecycle_changed` rows (D7), and PR-1 at about 2k lines is accepted (§7).

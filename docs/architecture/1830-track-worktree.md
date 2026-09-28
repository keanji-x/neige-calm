# #1830 S1 — the Planner runs in a kernel-made track worktree

**Owner rules.** (1) Simple first, pain points only: the fewest mechanisms that cover the
observed symptoms; anything hypothetical is a one-line KNOWN GAP. (2) Compatibility means the
4140 database only.

**Outcome.** A new attached track gets one kernel-made git worktree and branch, based on the
upstream the way worker leases are (#1777). Every Planner turn, Codex or Claude, runs there.
Workers do not change in S1. Deleting the track removes the worktree and the branch.

Observed pain this covers (#1830): the working directory resets every turn, `../` cannot be
written, AGENTS.md comes from a stale, dirty checkout (4140: `main` 10 behind, 69 dirty files).

## 1. Facts

Verified at c7c7f82ae by reading the code, or by the command shown.

| # | Claim | Where | Verified |
|---|---|---|---|
| F1 | `tracks.workspace_path` is the only stored path; `TrackWorkspace` = kind, path, frozen_at | `calm-types/src/model.rs:270`, `calm-truth/src/db/rows.rs:101-105` | read |
| F2 | Attached create (`TrackWorkspacePlan::AttachedFromCwd`) freezes the workspace at birth | `calm-truth/src/db/sqlite/track.rs:137-141`, route `routes/tracks.rs:991` | read |
| F3 | Re-point refuses an attached or frozen source, so an attached track never moves | `routes/tracks.rs:2128-2142` | read |
| F4 | Attached admission runs before the tx: absolute, exists, `show-toplevel` succeeds, `refs/heads/neige` free | `workspace_materialize.rs:119-168`, `routes/tracks.rs:931-937` | read |
| F5 | Managed materialization runs after the track tx commits and before the planner starts; a failure is non-2xx and leaves the row | `routes/tracks.rs:911-913`, `:1616-1634` | read |
| F6 | An idempotent create replay re-materializes (`adopt_prior_track`) | `routes/tracks/create.rs:531-555` | read |
| F7 | Worker `repo_root` = `show-toplevel` of `workspace_path`; lease = `<repo_root>/.claude/worktrees/<track>/<card>`, branch `neige/<track>/<card>` | `operation/workspace_lease/mod.rs:132-163`, `:1549-1583`, `:1599-1641` | read |
| F8 | Upstream fetch: bounded 20 s, fail-soft, single-flight, provenance keyed by (common dir, kernel ref); base = `choose_lease_start` (behind → upstream, ahead → HEAD, diverged → refused `attached-repo-diverged`) | `workspace_lease/upstream_fetch.rs:248`, `upstream.rs:323`, `:378` | read |
| F9 | `provision_workspace_worktree` is lease-shaped: `verify_worktree_base` requires HEAD == base_sha, stale-dir cleanup requires a lease path | `workspace_lease/mod.rs:1166-1246`, `base.rs:810-834`, `mod.rs:1691-1716` | read |
| F10 | Codex Planner: `thread/start` gets the payload `cwd` with `workspace-write`; the thread keeps that cwd for life | `operation/planner_harness_start_adapter.rs:913`, `:965`, `:1070-1076` | read |
| F11 | Codex `config/read` must use the thread's cwd; it re-reads `track.workspace.path` | `harness/run_loop.rs:2674-2690` | read |
| F12 | Claude Planner: session cwd = `track.workspace.path`; every turn spawns with `current_dir(cwd)`; Edit/Write allowed only under `cwd/**`; sandbox always on | `claude_planner/wiring.rs:69`, `session.rs:493`, `spawn.rs:21-32`, `:57-75` | read |
| F13 | Planner forge actions (`git.commit`, `gh.pr.*`) run in `track.workspace.path` | `mcp_server/transport.rs:974-981` | read |
| F14 | Track delete and area delete share one sweep: `release_workspace_leases_for_track_tx` → `WorkspaceTrackSweep` → `sweep_workspace_worktrees_for_track_repo` | `routes/tracks.rs:2865`, `:3061`; `routes/areas.rs:568`, `:648`; `workspace_lease/mod.rs:502-525`, `:574-720` | read |
| F15 | That sweep only enumerates `.claude/worktrees/<track_id>/`; the #1815 reclaim selects lease rows only | `workspace_lease/mod.rs:770-777`; `reclaim.rs:78-130` | grep `read_dir` |
| F16 | `remove_workspace_worktree` (Discard) is not lease-shaped: unlink symlink leaf, refuse foreign registration, `worktree remove --force`, `branch -D` | `workspace_lease/mod.rs:1346-1452` | read |
| F17 | No re-point, freeze or terminal write touches a frozen attached row; the one workspace writer carries the latch | `calm-truth/src/db/sqlite/track_workspace.rs:11-47`, `:74-93` | read |
| F18 | 4140, 2026-09-05..09-28: 17 attached tracks created. 9 are live (8 `done`, draft `40b02ce4`; all frozen, none children, none archived; Planners 7 Codex, 2 Claude). 8 were deleted (`track.deleted` events), none of which ever held a lease. Every attached `cwd` is a repository toplevel (one was a user's linked worktree) | `sqlite3 -readonly …/calm.db` on `tracks`, `worker_sessions`, `events` | query |
| F19 | Draft `40b02ce4`: Codex planner, thread started with `cwd=/mnt/data2/kenji/neige-calm`, no leases | `operations.payload_json`, `worker_sessions` | query |
| F20 | neige-calm checkout: 3,411 tracked files, 48 MB; shared `.git` 705 MB (objects are not copied); 83 registered worktrees, 83 `refs/heads/neige/*` | `du -sh --exclude=.git`, `git ls-files \| wc -l`, `git worktree list \| wc -l` | command |
| F21 | #1815: Claude's Bash sandbox adds about 2 deny paths per registered worktree; E2BIG at about 590 worktrees | `gh issue view 1815` | read |
| F22 | Test fixtures attach repositories with no commit (`git init` only) | `tests/cases/area_defaults.rs:63-75` and other `tests/cases` files | grep |

## 2. Decisions

- **D1 Representation: one new nullable column `tracks.workspace_worktree_path`**, surfaced as
  `TrackWorkspace.worktree: Option<String>` (`serde(default, skip_serializing_if = "Option::is_none")`,
  so existing events and payload hashes stay byte-identical). `workspace_path` keeps meaning the
  user's checkout. *Why:* only the Planner moves in S1. Reinterpreting `workspace_path` would
  change about 25 readers, and workers would resolve `show-toplevel` to the track worktree and
  nest their worktrees inside it. `None` is a real state (managed, child, and pre-#1830 attached
  tracks), not a missing value.
- **D2 One accessor, `TrackWorkspace::agent_cwd()`**, which returns `worktree` when set, else
  `path`. This is the cwd of every conversation agent on the track: Planner, assistant and plain
  chat. *Why:* `installation_cwd` (F11) is per track, not per card, so every card on a track needs
  the same cwd.
- **D3 Path and branch:** `<repo_root>/.claude/worktrees/track-<track_id>` and branch
  `neige/track-<track_id>`. *Why:* one segment below `worktrees/`, so it never parses as a lease
  path (`<track>/<card>`) and no lease sweep reads it (F15). `refs/heads/neige/track-<id>` cannot
  collide with `refs/heads/neige/<id>/<card>` (a 32-hex id is never `track-…`), so S2 can put
  workers on this branch while old slice branches still exist. Already covered by the
  `.claude/worktrees/` exclude (`workspace_lease/mod.rs:1454`).
- **D4 Which tracks:** every track the create route mints on the attached branch
  (`cwd` given). This covers any template and investigation tracks. Not managed, child,
  launchpad or area-chat tracks, and not a managed→attached re-point. *Why:* one rule. The cost
  is a 48 MB checkout plus one sandbox registration per track (F20, F21). 4140 creates
  17 attached tracks per 23 days, and live ones grow by about 12 per month against a ceiling of
  about 590. Child tracks: 0 in 4140.
- **D5 When:** (a) Before the track tx, next to `validate_attached_workspace`: resolve
  `repo_root`, run `refresh_upstream(repo_root)` (bounded, fail-soft), then
  `choose_lease_start`. `Diverged` is refused with `diverged_refusal` (409), and an unborn HEAD
  or other chooser error with 400. Nothing is minted. (b) In the tx, the new plan variant
  `TrackWorkspacePlan::AttachedWithTrackWorktree(repo_root)` writes the path through the existing
  single writer. (c) After commit, where managed workspaces materialize (F5),
  `ensure_track_worktree` runs `git worktree add -b <branch> <path> <base>` in
  `spawn_blocking`. A failure here is non-2xx and leaves the row, as with managed (F5). The
  replay (`adopt_prior_track`, F6) runs the same ensure: a registered directory is Ok, an
  existing branch is added without `-b`, otherwise the base is chosen again (locally).
  *Why:* refusals happen before any row, as with the other attached checks (F4). The fetch
  receipt is shared with the first worker's lease, which then uses the same upstream.
- **D6 Dirty user checkout: irrelevant.** `worktree add` checks out a commit and never reads
  the user's working tree. The Planner does not see the user's uncommitted edits, by design.
- **D7 Removal: track delete and area delete only.** `WorkspaceTrackSweep` also reads
  `workspace_worktree_path`, and `sweep_workspace_worktrees_for_track_repo` removes it first,
  independent of lease rows, with
  `remove_workspace_worktree` (Discard: `--force`, `branch -D`) on the target
  {repo_root = path's third ancestor, path, branch}. It emits no `WorktreeRemoved` event: that
  event is card-scoped, and the track worktree has no card. A dirty track worktree is discarded,
  as lease worktrees are. *Why:* F14 already covers both deletes. Removal on reaching a terminal
  state would mean re-creating the worktree on reopen, which is S3's post-merge reclaim.
- **D8 Re-point and freeze: unchanged.** Attached tracks are frozen at birth (F2, F3), so a row
  with a worktree is never re-pointed. The column is written only by
  `track_workspace_write_tx` in the create tx; a re-point writes `None`.
- **D9 The readers that move to `agent_cwd()`** are those that name where the Planner runs
  (table in §3). Every reader for workers, terminals, gates, files and staleness stays on
  `workspace.path`.
- **D10 4140 rows:** the migration adds the column NULL with no backfill. The 9 attached tracks
  keep running in `/mnt/data2/kenji/neige-calm` and `/mnt/data2/kenji/galxe/jugge`. Moving them
  would break resume: a Codex thread keeps its start cwd (F10), and a Claude `--resume` session
  is stored per cwd. To move the draft `40b02ce4`, delete it and create it again.

## 3. Readers of the track workspace (grep of `workspace_path|workspace\.path|workspace_kind|workspace\.kind`)

| Reader | Where | S1 |
|---|---|---|
| Planner start payload, message-less create | `routes/tracks.rs:1648-1654` | `agent_cwd()` |
| Planner start payload, first-message create and retry | `routes/tracks/create.rs:495`, `:572-573` | `agent_cwd()` (a replay keeps its recorded `prior.cwd`) |
| Card reset restart | `routes/cards.rs:1409-1415` | `agent_cwd()` |
| Assistant conversation on a track | `routes/track_conversations.rs:182-188` | `agent_cwd()` |
| Claude session cwd, including recovery (`harness/mod.rs:270`) | `claude_planner/wiring.rs:69` | `agent_cwd()` |
| Codex `config/read` cwd | `harness/run_loop.rs:2678` | `agent_cwd()` |
| Model defaults for a card ("the same value planner-harness-start puts on its payload") | `routes/models.rs:273-296` | `agent_cwd()` |
| Planner forge cwd (Planner arm only) | `mcp_server/transport.rs:974-981` | `agent_cwd()`, so a Planner `git.commit` lands on the track branch, not the user's checkout |
| Launchpad start and today summary | `routes/today.rs:524-530`, `today_summary.rs:342-350` | unchanged (managed: `agent_cwd() == path`) |
| Child bootstrap cwd | `child_track_adapter.rs:113-124`, `:407`; `scheduler/mod.rs:1533-1549` | unchanged (child: `None`) |
| Re-point restart | `routes/tracks.rs:2229-2360` | unchanged (unreachable for rows with a worktree, D8) |
| Worker lease `repo_root`, managed last-chance materialize | `workspace_lease/mod.rs:141-157` | unchanged: user's checkout, so workers never nest in the track worktree |
| Worker upstream refresh | `workspace_lease/upstream_fetch.rs:597-627` | unchanged |
| Worker success commit | `mcp_server/tools/emit.rs:196` | unchanged |
| Candidate staleness (`plan.list`) | `mcp_server/tools/plan.rs:781-785` | unchanged (measured against workers' base) |
| Gate fallback cwd | `task_verify_adapter/mod.rs:441-487` | unchanged |
| Terminal default cwd (cards, terminal tasks, Claude restart) | `terminal_adapter.rs:225-240`; callers `:292`, `:677`, `claude_restart_adapter.rs:176` | unchanged (KNOWN GAP) |
| Teardown sweep | `workspace_lease/mod.rs:586-610` | also reads the new column (D7) |
| FE file reads | `routes/fs.rs:294`, `:329`; FE `fileRoot={track.cwd}` `fe/web/src/app/router/public.tsx:2338`, `:2382` | unchanged (KNOWN GAP) |
| Planner attachments, managed recycle | `planner_attachments/mod.rs:53-60`, `workspace_recycle.rs:19-24` | unchanged (managed only) |
| Replace admission and routing (kind only) | `task_replace/route.rs:40`, `admission.rs:217` | unchanged |
| Freeze points | `workspace_lease/mod.rs:252`, `calm-truth/.../card.rs:566`, `track.rs:299` | unchanged |

## 4. S1 change list

- `calm-truth/migrations/0119_track_worktree.sql` (number assigned last):
  `ALTER TABLE tracks ADD COLUMN workspace_worktree_path TEXT NULL;`.
- `calm-types/src/model.rs`: `TrackWorkspace.worktree`, `agent_cwd()`. Regenerate the OpenAPI and
  `fe/core/api/generated/wire.ts` output. Then fix every `TrackWorkspace { … }` literal (about 15 in
  non-test code) as the compiler reports them.
- `calm-truth/src/db/rows.rs`: `TRACK_SELECT_COLUMNS` and `_W` (in lockstep), `TrackRow`, `From`.
- `calm-truth/src/db/sqlite/track_workspace.rs`: the whole-value writer and
  `track_workspace_read_tx` carry the fourth column.
- `calm-truth/src/db/sqlite/track.rs`: the variant `AttachedWithTrackWorktree(PathBuf)` derives
  the path from the minted id. `AttachedFromCwd` (fixtures, `Repo::track_create`) stays
  worktree-less.
- `calm-server/src/operation/workspace_lease/track_worktree.rs` (new, about 120 lines):
  `track_worktree_path_for`, `track_branch_for`, `admit_track_worktree_base` (async: fetch +
  chooser) and `ensure_track_worktree` (sync). It reuses `git_worktree_registration`,
  `git_ref_exists`, `ensure_workspace_worktree_root_excluded` and `isolated_git_command`, but
  not `provision_workspace_worktree` (F9).
- `routes/tracks.rs`: admission before the tx, the plan variant, ensure after
  `materialize_workspace`, and the Planner-start `agent_cwd()`. `routes/tracks/create.rs`:
  `:495`, `:572-573`, and ensure inside `adopt_prior_track`.
- `routes/cards.rs`, `routes/track_conversations.rs`, `routes/models.rs`,
  `claude_planner/wiring.rs`, `harness/run_loop.rs`, `mcp_server/transport.rs`: `agent_cwd()`.
- `workspace_lease/mod.rs`: `WorkspaceTrackSweep.track_worktree`, read at `:586` and removed in
  `sweep_workspace_worktrees_for_track_repo`.
- `prompts/planner.md`, one bullet (and the golden
  `tests/goldens/issue_development_planner_prompt.txt`):
  > On an attached Track your working directory is the Track's own git worktree (branch
  > `neige/track-<track id>`), made from the upstream when the Track was created. The Track's
  > `cwd` is the user's checkout: read it if you must, never write to it. Commit with
  > `git.commit`. Workers still run in their own worktrees and do not see your uncommitted edits.
- Test fixtures that attach a commit-less `git init` repository (F22) get an initial commit.
  Assertions that the Planner payload `cwd` equals the track `cwd` on an attached create
  (`track_create_first_message.rs:323` `first_message_payload_cwds` and its users) now expect
  the worktree path.

## 5. Gates that constrain the shape

- `crates/calm-truth/tests/track_write_point_registry.rs:12`, `:20-35`: add
  `workspace_worktree_path` to `WORKSPACE_COLUMNS` and update the pinned whole-value writer
  text. There must still be no second writer.
- `crates/calm-server/tests/cases/head_schema_fixture.rs:58-62`: list `0119`.
- Released migrations are byte-frozen: add a new file and edit none.
- `TRACK_SELECT_COLUMNS` / `_W` lockstep (`rows.rs:63-75`): the columns bind by name at run
  time.
- Generated artifacts: `TrackWorkspace` is `ts(export)`, so OpenAPI and `wire.ts` must be
  regenerated.
- `scripts/local-ratchet-gates.sh` (terminology and prose ratchets; this doc is in scope).
- Not triggered: `gate-sync-event-version-lockstep.sh` (no event-version stamp) and
  `scripts/ci/ratchets/*` (append seam and report write boundary).

## 6. Tests

New cases in `crates/calm-server/tests/cases/track_worktree.rs`, run against real routes and
real git (a bare origin plus a clone).

| Test | Pins | Mutation that must turn it red |
|---|---|---|
| T1 `attached_create_makes_the_track_worktree_at_the_upstream` (clone one commit behind origin): row `worktree == <clone>/.claude/worktrees/track-<id>`; worktree HEAD == origin tip on `neige/track-<id>`; the clone's HEAD and `status --porcelain` unchanged; `planner-harness-start` payload `cwd` == worktree | D5 (c), D3, `routes/tracks.rs:1654` | M1 drop the ensure call after commit; M2 payload `cwd: track.workspace.path`; M3 base = `resolve_head_base` instead of `choose_lease_start` |
| T2 `diverged_checkout_is_refused_before_any_row`: local commit plus a moved origin → 409 naming `attached-repo-diverged`; no track row in the area, no `refs/heads/neige/track-*`, no directory | D5 (a) | M4 skip the admission (the ensure after commit still refuses, but the row exists) |
| T3 `claude_planner_turn_runs_in_the_track_worktree` (`claude_planner_wiring.rs` stack, attached `cwd`; the fake also records `pwd`): the fake's cwd == worktree and argv has `Edit(/<worktree>/**)` | `claude_planner/wiring.rs:69` | M5 wiring passes `workspace.path` |
| T4 `track_delete_removes_the_track_worktree_and_branch` (with an untracked file in it): directory gone, not in `worktree list`, ref gone, the clone untouched | D7 | M6 drop the removal in `sweep_workspace_worktrees_for_track_repo` |
| T5 `planner_git_commit_lands_on_the_track_branch`: a Planner `git.commit` forge action moves `neige/track-<id>`, and the clone's HEAD is unchanged | `transport.rs:981` | M7 Planner arm returns `workspace.path` |

Ordinary (not mutation-verified) tests: `agent_cwd()` for `None` / `Some`; the replay ensure is
idempotent; a commit-less repository gets 400 at admission; `installation_cwd` uses the worktree.
Each mutation changes one production line. Predicted red sets: M1 → T1, T3, T4, T5 (each first
asserts that the worktree exists); every other mutation → only its own test. T1 creates without
a first message; the first-message arm is covered by the updated `first_message_payload_cwds`
assertions.

## 7. KNOWN GAPS

- The Planner cannot `git commit` from a shell. For Codex, `workspace-write` makes the linked
  worktree's gitdir (in the common dir) read-only. For Claude it depends on the sandbox (not
  resolvable from code). S1 answer: `git.commit` (T5).
- The terminal default cwd, the FE file view (`fs.rs`) and the gate fallback still use the
  user's checkout. A file the Planner links to resolves there.
- Workers are unchanged, so they do not see the Planner's edits. That is S2.
- Child tracks and managed→attached re-points get no worktree (0 in 4140).
- The worktree is removed only on delete. `done` tracks keep theirs (the 4140 net growth is
  about 12 per month; 8 of 17 attached tracks were deleted).
- If someone removes the worktree by hand, the Planner's spawn fails. There is no re-ensure at
  planner start; delete the track.
- A track delete that races the ensure after commit can leak one worktree (the same window
  managed materialization has).
- An attached `cwd` that is a subdirectory runs the Planner at the worktree root (every 4140 attach
  was a toplevel).
- Pre-#1830 Claude sessions that re-render instructions (`wiring.rs` at recovery) read the new
  bullet while still running in the checkout (2 `done` tracks).

## 8. S1 / S2 / S3 boundary

- **S1 (this doc):** column, accessor, create-time worktree and branch, Planner/assistant/forge
  cwd, removal on delete, prompt bullet.
- **S2:** the lease targets the track worktree (one per track), a per-attempt kernel commit, a
  clean-tree check before a worker starts, #1785 carry deleted. S2 decides whether the terminal,
  gate and FE file defaults and child tracks move to the worktree, and gives the Planner a
  writable gitdir if one is still needed.
- **S3:** push equal to the candidate, `gh.pr.create`, and reclaim of the worktree and branch
  after merge or on reaching a terminal state (#1815 pattern).

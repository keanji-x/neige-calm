# #1830 S1 — the Planner runs in a kernel-made track worktree

**Owner rules.** (1) Simple first, pain points only: the fewest mechanisms that cover the
observed symptoms; anything hypothetical is a one-line KNOWN GAP. (2) Compatibility means the
4140 database only. (3) S1–S3 land before anything is deployed to 4140; no intermediate
state is designed for.

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
| F5 | Managed materialization runs after the track tx commits and before the planner starts; a failure is non-2xx and leaves the row | `routes/tracks.rs:911-913`, `:1616-1634` | read |
| F6 | `adopt_prior_track` re-materializes for all three resume arms (Replay, GenuineRetry, message-less) and maps a failure to 409 `idempotency_key_exhausted`, on which the FE rotates the key and mints a new track | `routes/tracks/create.rs:514-607`, `fe/core/domain/track.ts:473` | read |
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
| F23 | Claude Code 2.1.280 with `--setting-sources project` and cwd `<repo>/.claude/worktrees/track-x` loads only the worktree's AGENTS.md, not the parent repository's (a marker in each file: the nested cwd reported only the child marker, the root cwd only the parent marker) | orchestrator probe | probe |

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
  `.claude/worktrees/` exclude (`workspace_lease/mod.rs:1454`). The path is derived once,
  `track_worktree_path_for` in `calm-truth/src/db/sqlite/track.rs`, because the create
  transaction (which mints the id) needs it; the server reuses it.
- **D4 Which tracks:** every track the create route mints on the attached branch
  (`cwd` given). This covers any template and investigation tracks. Not managed, child,
  launchpad or area-chat tracks, and not a managed→attached re-point. *Why:* one rule. The cost
  is a 48 MB checkout plus one sandbox registration per track (F20, F21). 4140 creates
  17 attached tracks per 23 days, and live ones grow by about 12 per month against a ceiling of
  about 590. Child tracks: 0 in 4140.
- **D5 When:** (a) In the create tx, the new plan variant
  `TrackWorkspacePlan::AttachedWithTrackWorktree(repo_root)` writes the path through the existing
  single writer (`repo_root` from `git_repo_root_for_track_cwd`, run before the tx). (b) After
  commit, where managed workspaces materialize (F5, `create_track_structure`, shared by the
  message-less and first-message mints), `ensure_track_worktree` does it all:
  `refresh_upstream(repo_root)` (bounded, fail-soft), then in `spawn_blocking` a registered
  directory is Ok, an existing branch is added without `-b`, else `choose_lease_start` picks the
  base and `git worktree add -b <branch> <path> <base>` runs. `Diverged` (`diverged_refusal`) or
  an unborn HEAD fails the create non-2xx and leaves the row, exactly like a managed
  materialization failure (F5). Resume arms (F6): `PriorArm::Replay` skips the ensure (its
  recorded payload proves the mint's ensure succeeded); GenuineRetry and message-less resume run
  it through `adopt_prior_track(.., ensure_worktree: bool)` (Replay passes `false`), after the
  managed re-materialization and while the track delete guard is still held, as that
  materialization is. It surfaces its own error, never the `idempotency_key_exhausted`
  mapping, which would make the FE mint a second track.
  *Why:* one function, one place, the contract F5 already has.
- **D6** A dirty user checkout is irrelevant: `worktree add` checks out a commit, not the
  user's working tree.
- **D7 Removal: track delete and area delete.** `WorkspaceTrackSweep` also reads
  `workspace_worktree_path`. `sweep_workspace_worktrees_for_track_repo` removes it before its
  early returns (`workspace_lease/mod.rs:671`, `:688`, `:713`), independent of lease rows, with
  `remove_workspace_worktree` (Discard: `--force`, `branch -D`) on the target {repo_root = the
  path's third ancestor, path, branch}. A failure is `warn!`-logged and never propagated: the
  area loop uses `?` (`mod.rs:765`), so an error would abort the other tracks' sweeps. No
  `WorktreeRemoved` event (it is card-scoped). A dirty track worktree is discarded, as lease
  worktrees are. *Why:* F14 already covers both deletes.
- **D8 Re-point and freeze: unchanged.** Attached tracks are frozen at birth (F2, F3), so a row
  with a worktree is never re-pointed. The column is written only by
  `track_workspace_write_tx` in the create tx; a re-point writes `None`.
- **D9 The readers that move to `agent_cwd()`** are those that name where the Planner runs
  (table in §3). All others stay on `workspace.path`.
- **D10** Existing rows keep their cwd: the column is added NULL, with no backfill (a Codex
  thread and a Claude session are bound to their start cwd, F10, F12).

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
| Track file reads (Report file links, the track panel): `readfile`, `readfile-raw` | `routes/fs.rs:294`, `:329` | `agent_cwd()`: their relative paths are the Planner's |
| Teardown sweep | `workspace_lease/mod.rs:586-610` | also reads the new column (D7) |

Unchanged (`workspace.path`): launchpad and today summary (`routes/today.rs:524-530`,
`today_summary.rs:342-350`; managed, so equal anyway), child bootstrap
(`child_track_adapter.rs:407`, `scheduler/mod.rs:1533-1549`), re-point restart
(`routes/tracks.rs:2229-2360`, unreachable here by D8), worker lease `repo_root`
(`workspace_lease/mod.rs:141-157`, so workers never nest in the track worktree), worker upstream
refresh (`upstream_fetch.rs:597-627`), worker success commit (`emit.rs:196`), candidate staleness
(`plan.rs:781-785`), gate fallback (`task_verify_adapter/mod.rs:441-487`), terminal default cwd
(`terminal_adapter.rs:225-240`), managed-only
readers (`planner_attachments/mod.rs:53-60`, `workspace_recycle.rs:19-24`), replace routing
(kind only), freeze points (`workspace_lease/mod.rs:252`, `card.rs:566`, `track.rs:299`).

## 4. S1 change list

- `calm-truth/migrations/0120_track_worktree.sql` (number assigned last):
  `ALTER TABLE tracks ADD COLUMN workspace_worktree_path TEXT NULL;`.
- `calm-types/src/model.rs`: `TrackWorkspace.worktree`, `agent_cwd()`. Regenerate the OpenAPI and
  `fe/core/api/generated/wire.ts` output; `fe/core/api/schemas.ts` decodes the optional field
  (the zod ↔ ts-rs conformance test requires it). The create route's 409 description names the
  `attached-repo-diverged` refusal and its recovery (reconcile, retry under the same key). Then fix every `TrackWorkspace { … }` literal (about 15 in
  non-test code) as the compiler reports them.
- `calm-truth/src/db/rows.rs`: `TRACK_SELECT_COLUMNS` and `_W` (in lockstep), `TrackRow`, `From`.
- `calm-truth/src/db/sqlite/track_workspace.rs`: the whole-value writer and
  `track_workspace_read_tx` carry the fourth column.
- `calm-truth/src/db/sqlite/track.rs`: the variant `AttachedWithTrackWorktree(PathBuf)` derives
  the path from the minted id with `track_worktree_path_for` (D3). `AttachedFromCwd` (fixtures,
  `Repo::track_create`) stays worktree-less.
- `calm-server/src/operation/workspace_lease/track_worktree.rs` (new, about 160 lines):
  `track_branch_for`, `track_worktree_target` (the stored path back to {repo_root, path,
  branch}, shape-checked), `ensure_track_worktree` (async fetch, then the git work in
  `spawn_blocking`; registered and present is done, anything else is left to `git worktree
  add` to refuse) and `remove_track_worktree` (D7). It reuses `git_worktree_registration`,
  `git_ref_exists`,
  `ensure_workspace_worktree_root_excluded`, `remove_workspace_worktree` and
  `isolated_git_command`, but not `provision_workspace_worktree` (F9).
- `routes/tracks.rs`: `repo_root` before the tx, the plan variant, ensure after
  `materialize_workspace`, and the Planner-start `agent_cwd()`. `routes/tracks/create.rs`:
  `:495`, `:572-573`, and the ensure on GenuineRetry and message-less resume (D5).
- `routes/cards.rs`, `routes/track_conversations.rs`, `routes/models.rs`,
  `claude_planner/wiring.rs`, `harness/run_loop.rs`, `mcp_server/transport.rs`: `agent_cwd()`.
- `workspace_lease/mod.rs`: `WorkspaceTrackSweep.track_worktree`, read at `:586` and removed in
  `sweep_workspace_worktrees_for_track_repo`.
- Test fixtures that attach a commit-less `git init` repository (F22) get an initial commit.
  The `first_message_payload_cwds` users at `track_create_first_message.rs:1424`, `:1476` are
  managed→attached re-points (no worktree) and stay unchanged. In practice that is
  `support::git_helpers::attached_repo_fixture`; `area_defaults.rs` (F22) attaches no track.
- Existing tests whose premise #1830 changes: the #1147 recycle fingerprints
  (`track_workspace_recycle.rs`) leave out the kernel-owned worktree, branch and the
  `packed-refs` git writes on `branch -D`; `a_neige_branch_created_after_attach_still_blocks_the_first_worker`
  is built on a pre-#1830 row, because a track's own `neige/track-<id>` makes git refuse
  `refs/heads/neige`; the `tracks` column snapshot in `track_projection_policy_patch.rs` lists
  the new column.

## 5. Gates that constrain the shape

- `crates/calm-truth/tests/track_write_point_registry.rs:12`, `:20-35`: add
  `workspace_worktree_path` to `WORKSPACE_COLUMNS` and update the pinned whole-value writer
  text. There must still be no second writer.
- `crates/calm-server/tests/cases/head_schema_fixture.rs:58-62`: list `0120` (a new, byte-frozen file; `0119` is #1838's `report_tags` on main).
- `crates/calm-server/tests/cases/track_projection_policy_patch.rs`: the `tracks` column snapshot.
- `TRACK_SELECT_COLUMNS` / `_W` lockstep (`rows.rs:63-75`); regenerated OpenAPI and `wire.ts`.
- `scripts/local-ratchet-gates.sh` (terminology and prose ratchets; this doc is in scope).
- Not triggered: `gate-sync-event-version-lockstep.sh`, `scripts/ci/ratchets/*`.

## 6. Tests

New cases in `tests/cases/track_worktree.rs` (in `track_suite`): real routes, real git (bare
origin plus a clone). T3 is in `claude_planner_wiring.rs` (`planner_harness_suite`); T5 is
`tests/cases/git_forge_track_worktree.rs`, a module of `mcp_git_forge_plugin.rs` (the real MCP
socket and git-forge plugin, the track minted by the real create route over the same caches).

| Test | Pins | Mutation that must turn it red |
|---|---|---|
| T1 `attached_create_makes_the_track_worktree_at_the_upstream` (clone one commit behind origin): row `worktree == <clone>/.claude/worktrees/track-<id>`; worktree HEAD == origin tip on `neige/track-<id>`; the clone's HEAD and `status --porcelain` unchanged; `planner-harness-start` payload `cwd` == worktree; a file only in the worktree reads through `/api/tracks/{id}/workspace/readfile` | D5 (c), D3, `routes/tracks.rs:1654`, `routes/fs.rs:294` | M1 drop the ensure call after commit; M2 payload `cwd: track.workspace.path`; M3 base = `resolve_head_base` instead of `choose_lease_start`; M8 `readfile` resolves against `workspace.path` (`readfile-raw` shares the fix, no second test) |
| T1b `a_first_message_create_starts_the_planner_in_the_track_worktree` (clone one commit behind): worktree HEAD == origin tip; payload `cwd` == worktree | `routes/tracks/create.rs:495` | M1, M3; M7 `create.rs:495` passes `workspace.path` |
| T3 `claude_planner_turn_runs_in_the_track_worktree` (`claude_planner_wiring.rs` stack, attached `cwd`; the fake also records `pwd`): the fake's cwd == worktree and argv has `Edit(/<worktree>/**)` | `claude_planner/wiring.rs:69` | M1; M4 wiring passes `workspace.path` |
| T4 `track_delete_removes_the_track_worktree_and_branch` (with an untracked file in it): directory gone, not in `worktree list`, ref gone, the clone untouched | D7 | M1; M5 drop the removal in `sweep_workspace_worktrees_for_track_repo` |
| T5 `planner_git_commit_lands_on_the_track_branch`: a Planner `git.commit` forge action moves `neige/track-<id>`, and the clone's HEAD is unchanged | `transport.rs:981` | M1; M6 Planner arm returns `workspace.path` |

Ordinary (not mutation-verified) tests: `agent_cwd()` for `None` / `Some` (`calm-types`; that
a worktree-less workspace omits the field is pinned by the event goldens and the FE contract
test);
`a_genuine_retry_reuses_the_registered_track_worktree` — the GenuineRetry `#N` path after a
harness-start failure finds the directory the failed attempt made already registered, keeps it
and starts there (the Replay arm does not ensure at all);
`a_diverged_checkout_fails_the_create_and_makes_no_worktree` (non-2xx,
`attached-repo-diverged`, row left, no worktree or branch);
`a_commit_less_repository_fails_the_create_and_makes_no_worktree`;
`the_codex_config_read_names_the_track_worktree` — `installation_cwd` uses the worktree (the
fixtures fake shared daemon now records each `config/read` cwd).

Each mutation changes one production line. Predicted red sets, over `track_suite`,
`planner_harness_suite` (`claude_planner_wiring`), `mcp_git_forge_plugin` and
`domain_api_suite` (`track_workspace_recycle`):
M1 → T1, T1b, T3, T4, T5 (each first asserts that the worktree exists), the diverged and the
commit-less tests (no ensure, so 201) and the genuine-retry test (its premise is that the failed
attempt made the worktree); M3 → T1, T1b (both are behind the origin) and the diverged test
(HEAD is taken, so 201); the commit-less test stays red under M3 as without it
(`resolve_head_base` fails on an unborn HEAD too); M5 → T4 and
`deleting_a_area_recycles_its_managed_workspaces_and_spares_attached_ones` (it asserts that area
delete removed the worktree); every other mutation → only its own test.
Existing expectations: `a_replay_survives_the_attached_directory_being_deleted`
(`track_create_first_message.rs:1810`) stays 201 (Replay skips the ensure); the former
`a_retry_after_a_failure_survives_the_attached_directory_ceasing_to_validate` (`:1867`) is now
`a_retry_after_a_failure_fails_once_the_attached_directory_is_no_repository`: the GenuineRetry
ensure runs against the `.git`-less directory and fails non-2xx, not `idempotency_key_exhausted`.

## 7. KNOWN GAPS (after S3)

- A worktree removed by hand makes the Planner's spawn fail; nothing re-ensures it at planner
  start, and a retry's ensure leaves the stale registration to `git worktree add`, which
  refuses. Delete the track.
- A track delete racing the ensure after commit can leak one worktree (the window managed
  materialization already has).
- An attached `cwd` below the repository toplevel runs the Planner at the worktree root (every
  4140 attach was a toplevel).
- A managed→attached re-point gets no worktree.
- A refused message-less create (e.g. diverged) leaves a track row with no Planner; the user
  deletes it.

## 8. S1 / S2 / S3 boundary

- **S1:** column, accessor, create-time worktree and branch, Planner/assistant/forge cwd,
  removal on delete.
- **S2:** workers lease the track worktree (one per track), a per-attempt kernel commit, a
  clean-tree check before a worker starts, #1785 carry deleted. S2 also decides whether the
  terminal, gate and FE file defaults and child tracks move to the worktree, and whether the
  Planner needs a writable gitdir to commit from its shell (Codex `workspace-write` makes the
  linked worktree's gitdir read-only; Claude's sandbox is not resolvable from code; until then,
  `git.commit`).
- **S3:** push equal to the candidate, `gh.pr.create`, and reclaim of the worktree and branch
  after merge or on reaching a terminal state (#1815 pattern).

# #1830 S2 — workers run where the Planner runs

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) S1–S3 land before anything is deployed to
4140. Attached create on a non-git directory is refused, so it is not designed for.

**Outcome (owner decision, 2026-09-28).** Every codex or claude task runs directly in its track's
`agent_cwd()`:

- An attached track runs in its S1 track worktree
  ([`1830-track-worktree.md`](1830-track-worktree.md)), on `neige/track-<id>`.
- A managed track runs in its managed directory, on `main`.

A track runs one worker at a time. After every attempt, whether it completed, failed or was
stopped, the kernel commits the directory and pins the commit as a candidate. The next attempt
starts from that commit. A worker does not start on a dirty tree, and the refusal lists the
files.

S2 deletes the per-card lease worktree path (`.claude/worktrees/<track>/<card>`,
`neige/<track>/<card>`, worker upstream bases, provisioning, lease removal, the #1815 reclaim),
the carry resolution and the legacy `git.commit:auto`. Attached tracks without a worktree are
refused at worker start. S2b (§9), which also lands before deploy, deletes `calm.task.replace`
and the budget knob. Isolated (`calm.task.dispatch`) and `terminal` tasks do not change.

## 1. Facts

Verified at 2bd0ce8eb (S1 head) by reading the code, or by the query or command shown (4140
`calm.db`, `events.max(id)=70638`, 2026-09-28).

| # | Claim | Where | Verified |
|---|---|---|---|
| G1 | Both worker adapters call, in order: `prepare_workspace_lease_target_tx` (per-card path and branch, repo root from `workspace_path`, managed materialize as a last chance), `carry::resolve_task_lease_base_tx`, `acquire_workspace_lease_tx`. The lease path is the worker cwd; spawn provisions a worktree | `workspace_lease/mod.rs:133-164`; `codex_adapter/mod.rs:787-850`, `:1606-1636`; `claude_adapter/mod.rs:779-860`; `claude_adapter/workspace.rs:7-38` | read |
| G2 | `verify_worktree_base` checks HEAD = base, realpath = `canonical_path`, and HEAD on the target branch. A carry base (`C'`, not HEAD) would fail it | `base.rs:810-850`; `carry.rs:66-90` | read |
| G3 | `workspace_leases_active_path_idx`: UNIQUE(`path`) WHERE state IN (`held`,`releasing`) | `migrations/0056_workspace_leases.sql:30-32` (re-created `0115:69`) | read |
| G4 | `BaseSource::Commit` is accepted by the CHECK and has no producer | `base.rs:44-62`; `0115` CHECK | read |
| G5 | Delivery and candidate rows carry `lease_id NOT NULL REFERENCES workspace_leases`. A gate reads the card's latest lease `path`; delivery provenance reads the lease's `base_sha`, `canonical_path` and `git_common_dir`. `task_git_deliveries.reason` is nullable and immutable; only the retry replay compares it | `0113_task_git_deliveries.sql:20`, `:26`, `:58`; `task_verify_adapter/mod.rs:466-474`; `delivery.rs:165-215`; `action.rs:208-219` | read |
| G6 | Managed materialize writes an owner marker in `.git`, runs `git init` on `main`, and makes an empty initial commit. The 4140 managed directory is `status`-clean | `workspace_materialize.rs:224-340`, `:429` | read + command |
| G7 | The budget is `COALESCE(tracks.task_budget, settings.task_budget_default, env, 1)`, applied in the ready set and in the claim tx. 4140 has `task_budget_default = 4`, and 6 of the 8 attached tracks that ran workers had concurrent leases | `scheduler/mod.rs:57`, `:130-141`, `:1038-1052`, `:1276-1307`; `settings`, `workspace_leases` | read + query |
| G8 | The first delivery row is written only for a success report, in the report tx. The report releases the lease in a second tx | `decision_sink.rs:203-215`, `:248-256`; `delivery.rs:337-362` | read |
| G9 | The delivery script needs HEAD on `refs/heads/$2`; the branch is `workspace_slice_branch_for` and the message names only card and track. The candidate's `repo_root` is an inverse parse of the per-card path and is for humans only | `delivery.rs:112-119`, `:183`; `forge_git.rs:51-73`; `candidate.rs:29-30`, `:44-64`, `:122` | read |
| G10 | Other releases: the reaper releases after its fail tx (and also on race-lost); timeout/cancel cleanup releases after the kill and retries until it clears its marker; compensation releases before `mark_failed` and `fail_spawn`; card delete releases in its own tx; boot reclaim releases only leases from an older boot | `reaper/mod.rs:476-567`; `scheduler/mod.rs:2064-2090`; `codex_adapter/mod.rs:1025-1094`; `driver.rs:712-718`; `scheduler/mod.rs:1749-1807`; `routes/cards.rs:1576`; `workspace_lease/mod.rs:401-437` | read |
| G11 | A delivery is driven by `schedule_pass` (resumed before readiness) and by the 300 s tick. Task terminal events and `task.git_delivery_settled` poke the scheduler; `workspace.released` does not | `scheduler/mod.rs:992-999`; `scheduler/git_delivery.rs:174-234`; `dispatcher/mod.rs:1007-1035` | read |
| G12 | Delivery retry admits a retryable failed row whose workspace exists and submits it, with no check that another attempt is running | `git_candidate/action.rs:345-377` | read |
| G13 | Prepare-time refusals end `spawn-failed: refused: …` (480 chars kept), which wakes the Planner even for a gated task | `upstream.rs:378-402`; `scheduler/mod.rs:1748-1807`; `dispatcher/mod.rs:223-232` | read |
| G14 | The legacy `git.commit:auto` runs only when there is no delivery row, and only on the per-card path it recomputes | `emit.rs:150-160`, `:248-278` | read |
| G15 | S1 teardown `remove_track_worktree` uses the Discard `remove_workspace_worktree`: symlink-leaf unlink and prune, foreign-registration refusal, `worktree remove --force`, `branch -D`, and a plain directory removal when the repository is gone | `track_worktree.rs:131-142`; `workspace_lease/mod.rs:1368-1452`, `:1168` | read |
| G16 | Planner `git.commit` runs in `agent_cwd()` and does `git add -A` + commit. git-forge has no discard action. Codex `workspace-write` makes the gitdir unwritable, and worker prompts already say the platform commits | `transport.rs:974-981`; `plugins/git-forge/manifest.json:12-248`; `shared_codex_appserver.rs:1234-1255`; `prompts/worker/head-*.md:6` | read |
| G17 | The kernel builds no sandbox deny list; Claude Code derives it from registered worktrees (#1815). A child of an attached track inherits `parent.path` with no worktree | `claude_planner/spawn.rs:21-32`; `child_track_adapter.rs:113-126` | read |
| G18 | 4140 counts: 9 attached tracks, all pre-#1830 (8 `done`, 1 `draft`); 0 non-terminal tasks; 0 non-terminal operations; 0 held leases; 0 unsettled deliveries; 0 child tracks. On disk: 12 per-card worktrees and 88 `neige/<t>/<c>` branches in 4 repositories | `tracks`, `tasks`, `operations`, `workspace_leases`, `task_git_deliveries`; `git worktree list`, `for-each-ref` | query + command |

## 2. Decisions

- **D1 Where a worker runs.** `prepare_worker_lease_tx(tx, track, card, workspace_root)`
  replaces the target, carry and base calls in both adapters.
  - A managed track materializes if needed (kept from G1) and runs in `workspace.path`.
  - An attached track with a worktree runs in that worktree.
  - An attached track without one (the 9 pre-#1830 tracks, and children of attached parents)
    fails with `refused: track-without-worktree: this track predates per-track worktrees;
    create a new track to run codex or claude tasks`.
- **D2 The lease row stays, one per attempt, at that directory** (G5: removing it would need a
  migration). Its base is the directory's HEAD (`BaseSource::Commit`, G4), with its realpath and
  common dir. The row keeps the freeze at first lease and `workspace.leased`/`released`.
  Removing the carry call is required: a carry base is not HEAD, so it would fail G2 for every
  replace successor. `carry:"none"` is ignored until S2b.
- **D3 Spawn only verifies** (`verify_worktree_base`); nothing is created or rolled back.
- **D4 The branch comes from the track row:** `neige/track-<id>` when it has a worktree, else
  `main` (the constant materialize uses, G6). This is used for the delivery's `$2`
  (`delivery.rs:183`) and the `facts.rs:135` fallback.
  `candidate.repo_root` is the lease path, meaning the checkout the candidate was made in. It is
  for humans only (G9), so there is no inverse parse.
- **D5 One track-idle predicate,** `track_idle_tx(tx, track_id, except_attempt)`. It holds when,
  apart from `except_attempt`:
  - no task is `dispatched`/`running`/`verifying`;
  - no lease is `held`/`releasing`;
  - no delivery is unsettled.

  Each term covers something the others miss:
  - status: a gate reading the tree after release, and a task claimed before its lease exists;
  - lease: a canceled worker not yet killed;
  - delivery: a commit not yet landed.

  It is used in two places, with G3's unique index as the backstop:
  - the claim tx, on top of the budget, which stays until S2b (`except` = the claimed row);
  - delivery retry admission (G12), with `except` = the retried attempt. Otherwise the retry
    refuses: "refused: the track is running another attempt; wait for it, then retry".
- **D6 Clean-tree check** in `prepare_worker_lease_tx`, before the lease INSERT, so a refusal
  writes no row and nothing commits the Planner's files.
  - It runs `git -c core.fsmonitor=false status --porcelain -z --untracked-files=normal` through
    `isolated_git_command`, under a deadline, using `run_git` (moved out of `carry.rs`).
  - Untracked files count; ignored files do not. `--untracked-files` is forced so that
    `status.showUntrackedFiles=no` cannot hide files that `git add -A` would commit.
  - A dirty tree fails with `refused: track-worktree-dirty: <n> uncommitted path(s). Commit them
    with git.commit or undo them, then recover or re-declare the task: <paths…>`, paths last.
  - A git failure fails with `refused: track-worktree-unavailable: <git's message>`.
  - Both take G13's wire.
- **D7 The release happens inside the transaction that ends the attempt, and freezes the
  outcome there.** `release_workspace_lease_for_card_tx(tx, card, outcome)` becomes the one
  release. It releases a held lease and, when the attempt has no delivery row, inserts the first
  one with its own column `outcome` set (the migration `0121_task_git_delivery_outcome.sql`):
  `ALTER TABLE task_git_deliveries ADD COLUMN outcome TEXT NULL CHECK (outcome IS NULL OR outcome IN
  ('completed','failed','canceled','spawn-failed','interrupted'))`, plus a `BEFORE UPDATE OF
  outcome` trigger that aborts, because the 0113 immutability trigger lists its columns by name.
  The column is nullable only because the 45 settled rows on 4140 predate it. Every row S2 writes
  carries a value: a retry row copies its predecessor's. `reason` keeps its single meaning, the
  Planner's retry reason. Callers:
  - **Report** (`calm.task.complete` / `fail`): inside the report tx (`decision_sink.rs`) with
    `completed` / `failed`. This replaces the second-tx release (G8), whose crash window would
    leave a `held` lease that D5 then blocks on forever. The success row keeps being written
    there.
  - **Reaper:** inside its fail tx with `failed`. The race-lost arm keeps its own tx, with the
    outcome read from the now-terminal `tasks.status`.
  - **Timeout / `calm.plan.cancel` of a running task:** at the cleanup after the kill, with the
    terminal `tasks.status` (`failed` / `canceled`). The terminal flip does not release, because
    the worker may still write. Until its marker clears, the cleanup is retried every tick.
  - **Compensation:** the `release_workspace_lease` step, with `spawn-failed`.
  - **Card delete:** its delete tx, with `interrupted`.
  - **Boot reclaim** (a lease from an older boot): with `interrupted`.
  - **Track/area delete:** no delivery row.

  The dispatcher pokes the scheduler on `workspace.released` so the new row is submitted (G11).
- **D8 Commit message:** `neige: attempt <attempt_id> <outcome> (delivery <id>)`, where the
  outcome is the row's own `outcome` column (D7). The payload builder reads nothing else, so
  every builder of one row produces the same text.
- **D9 No lease-driven removal remains.** Compensation and every release only flip the row, so
  the compensation `rm` hazard goes away with the path. S1 teardown keeps the Discard
  `remove_workspace_worktree` and its safety checks (G15). Only the lease- and reclaim-specific
  branches are deleted: KeepWork, `RemovalOutcome`, `remove_workspace_worktree_for_lease*` and
  the lease identity checks.
- **D10 Track/area delete** keeps the S1 worktree removal and the candidate-ref deletion. The
  per-card directory and slice-branch sweep is deleted. G18's residue is cleaned once at deploy
  (§8).
- **D11 Gates are unchanged:** they read the lease path, which is now `agent_cwd`.
- **D12 Delete the legacy `git.commit:auto`** (G14: it is dead without the per-card path).
- **D13 Prompt wording** (`prompts/planner.md`, shared by Codex and Claude).
  - `:73` becomes: "Your working directory is the track's git checkout (on an attached track,
    its worktree on `neige/track-<id>`). Codex and claude tasks run there one at a time, each
    from the kernel's commit of the previous attempt. Do not edit files while one is
    dispatched, running or verifying. A task starts only on a clean tree (`git status
    --porcelain` empty; ignored files do not count): commit with `git.commit` or undo first. A
    `track-worktree-dirty` failure lists the files; clean them, then recover or re-declare.
    `track-without-worktree` means create a new track."
  - `:77`: "After every attempt, completed, failed or stopped, the kernel commits…"
  - `:72`: "…its gate runs in the track's checkout…"
  - `head-*.md:6`: "the platform commits after you report".
  - The replace bullet (`:76`) changes in S2b.
- **D14 Deferred S1 §8 items:**
  - The terminal default cwd and the Claude-restart fallback stay on `workspace.path`. Moving
    them would add an uncommitted writer to the worker tree.
  - The gate fallback stays.
  - Children of managed parents work; children of attached parents are refused (D1).
  - The Planner needs no writable gitdir (G16).
- **D15 Sandboxes do not change** (G17). The gain is 1 worktree and 1 branch per attached track
  and none per attempt, which is what #1815's E2BIG counts.

## 3. How each attempt ends

| End | Terminal tx | Worker stopped | Release + delivery row |
|---|---|---|---|
| `calm.task.complete` / `fail` | report tx | after its turn | same report tx (`completed` / `failed`) |
| worker dies unreported | reaper fail tx | already | same tx (`failed`) |
| liveness timeout, running cancel | flip + cleanup marker | sweep kills it | cleanup after kill (`failed` / `canceled`) |
| spawn fails after prepare | compensation, then `fail_spawn` | never ran, or killed | compensation step (`spawn-failed`) |
| machine reboot mid-run | stays running until the timeout | already | boot reclaim (`interrupted`) |
| dirty tree or no worktree at prepare | `fail_spawn` | never ran | no lease, no row |

The only end without a commit is a delivery that runs and fails (a merge in progress, a switched
branch, or a git error). It settles `failed` and wakes the Planner. The tree stays dirty, and the
next start refuses with the file list; the fix is `calm.task.delivery retry` (D5-fenced) or the
Planner's `git.commit`.

## 4. S2 change list

Additions (about 300 production lines, one migration):

- `calm-truth/migrations/0121_task_git_delivery_outcome.sql` (D7; the number after S1's 0120 is
  assigned last), `DeliveryRow.outcome`, `DELIVERY_COLUMNS`, both INSERTs.
- `workspace_lease/mod.rs`: `prepare_worker_lease_tx` (D1, D2, D6); `track_idle_tx` (D5); the
  outcome parameter and delivery insert in `release_workspace_lease_for_card_tx` (D7); the
  branch rule (D4); `run_git`.
- Adapters (`codex_adapter/mod.rs`, `claude_adapter/{mod,workspace}.rs`): call
  `prepare_worker_lease_tx`, verify only at spawn, and drop the carry notice.
- Release call sites (D7): `decision_sink.rs`, `reaper/mod.rs`, `scheduler/mod.rs` cleanup, the
  adapters' compensation, `routes/cards.rs:1576`, `plugin_host/callbacks.rs:537`, boot reclaim.
- Fence call sites: the `scheduler/mod.rs` claim tx and `git_candidate/action.rs` retry (D5).
  `dispatcher/mod.rs` gets the `workspace.released` poke.
- `delivery.rs`: the branch and message (D4, D8). `candidate.rs`: `repo_root`. `facts.rs:135`.
- Prompts (D13). `observation.rs:363-366`: "Accept with calm.task.verdict when the task
  completed". `workspace_materialize.rs:171-215`: reword the refusal message (the
  `refs/heads/neige` check stays).

Deletions: about 2,150 production lines and 4,400 test lines.

| What | Prod lines |
|---|---|
| Per-card path in `workspace_lease/mod.rs`: target, `release_by_id`/`remove_artifact`, provisioning and stale-dir cleanup, KeepWork/`RemovalOutcome`/`*_for_lease*`, the per-card sweep (entries, slice branches, `worktree.removed` events), path parsing, `workspace_lease_path_for`/`workspace_slice_branch_for` | ~750 |
| `base.rs`: `resolve_lease_base`, the provision, sweep and removal identity checks, `WorktreeBase::LegacyUnpinned`. The symlink and foreign-registration helpers stay for G15 | ~230 |
| #1815 reclaim: `workspace_lease/reclaim.rs`, `scheduler/worktree_reclaim.rs` | 588 |
| Worker upstream fetch: `refresh_track_upstream`, the `before_insert` hook and its driver call. Pre-slice recovery arms in both adapters (0 in-flight ops) | ~110 |
| Carry: `carry.rs` less `run_git` (`resolve_task_lease_base_tx`, merge-tree, `CarryNotice`), `carry_plan_tx`, `prompts/worker/carry-notice.md`, the carry test seam `test_seams.rs:188-205` | ~300 |
| Legacy `git.commit:auto`: `emit.rs:170-306`, `:326-344` | ~160 |

Tests deleted:
- `workspace_lease/tests.rs`: about 2,000 of its 2,842 lines.
- `worktree_reclaim{,_guards,_kept}.rs`: 1,355 lines.
- `task_replace_carry.rs`: 580 lines.
- The `emit.rs` unit tests: about 450 lines.

Rewritten, not deleted: the lease helpers in `test_seams.rs` and the `kernel_lease` fixture in
`git_delivery.rs` take the lease at `agent_cwd`.

## 5. Gates and registries (S2)

- `tests/goldens/issue_development_planner_prompt.txt:72,73,77`
  (`REGEN_PLANNER_PROMPT_GOLDEN=1`), and `worker_prompt_{cli,mcp}.txt:6`.
- `dispatcher/tests.rs`: the observation text.
- `scripts/gate-prose-ratchet.sh`: update the baseline if the counts move.
- `tests/cases/head_schema_fixture.rs:58-62`: list `0121` (a new, byte-frozen file).
- Not triggered: `track_write_point_registry`, OpenAPI,
  `docs/oracle/*.yaml`, trybuild, and the FE.

## 6. Tests

New file `tests/cases/track_worker_cwd.rs` in `mcp_integration_suite`. It uses real routes, real
git (a bare origin plus a clone for attached tracks), and the fixture codex worker, which writes
files and reports through MCP. Tracks are minted by the real create route.

| Test | Pins | Mutation that must turn it red |
|---|---|---|
| T1 `a_dirty_worktree_refuses_the_worker_and_lists_the_files` (attached): a tracked edit, an untracked file and an ignored file. Result: `spawn-failed: refused: track-worktree-dirty`, naming the first two and not the ignored one, and no lease or card row. After the Planner's `git.commit`, a re-declared task runs on that commit | D6 | M1: the check reports clean. Red first: today the worker gets a fresh per-card worktree |
| T2 `a_failed_attempt_is_committed_and_the_next_task_continues_from_it` (attached): the worker writes `a.txt` and calls `calm.task.fail`. `neige/track-<id>` gains a commit whose message names the attempt and `failed`, the delivery row's `outcome` is `failed`, the candidate ref points at it, and the tree is clean. A task declared after that settlement has `base_sha` equal to that commit and reads `a.txt` | D7, D8 | M2: the report tx inserts no row on failure; M3: the report tx writes `outcome = 'completed'` for every report |
| T3 `an_attached_track_registers_one_worktree_and_one_branch`: two successful tasks, the second declared after the first settles. Both worker cwds are the track worktree. `git worktree list` and `refs/heads/neige/*` each grow by exactly 1 from before the create, and `.claude/worktrees/<track>/` does not exist | D1, D2 (acceptance) | M4: `prepare_worker_lease_tx` uses `workspace.path` for attached tracks |
| T4 `a_track_runs_one_worker_at_a_time`: `tracks.task_budget = 4` (as on 4140). Task a is made running through the `git_delivery.rs` fixture's `running_task`, and independent task b is ready. A scheduler pass leaves b `pending` | D5 status term | M5: the status term is dropped |
| T4b `the_next_task_waits_for_the_previous_commit` (budget 4): a writes a file and reports. A `pre-commit` hook in the repository blocks until a release file exists, which holds the real delivery unsettled. A pass leaves b `pending`. After the file is created, the delivery settles and b is claimed with base = a's commit | D5 delivery term | M6: the delivery term is dropped |
| T5 `a_worktree_less_attached_track_refuses_workers`: a fixture `AttachedFromCwd` row (no worktree, as on 4140) gives `spawn-failed: refused: track-without-worktree`; the checkout's `status` and HEAD are unchanged | D1 | M7: the attached arm falls back to `workspace.path` |
| T6 `a_managed_track_worker_commits_on_main_in_its_directory`: the worker cwd is `workspace.path`; a candidate commit lands on `main`; no worktree is registered | D1, D4 | M8: the branch rule gives every track `neige/track-<id>` |
| T7 `delivery_retry_waits_while_another_attempt_runs`: a's delivery settled `failed` (the `git_delivery.rs` observation-failure setup), b is running, and a retry is refused. After b ends, the retry is admitted | D5 retry | M9: retry skips `track_idle_tx` |

Ordinary tests, all on attached tracks:
- `calm.plan.cancel` of a running task commits `canceled` after the kill.
- The reaper path commits `failed`.
- The clean check lists `status.showUntrackedFiles=no` files (unit).

Each mutation changes one production line. Every leasing test asserts its lease path at the first
lease. Predicted red sets:
- M1 → T1 and the `showUntrackedFiles` unit test.
- M2 → T2.
- M3 → T2 only (the cancel and reaper tests write their outcome at another call site).
- M4 → T1–T4b, T7, and the cancel and reaper tests (all attached).
- M5 → T4.
- M6 → T4b.
- M7 → T5.
- M8 → T6.
- M9 → T7.

The lease term of D5 has no must-red test (holding a kill needs a missing hook); G3 backs it up.

## 7. KNOWN GAPS

- Managed tracks lose parallel workers: every track runs one at a time (4140 ran budget 4).
- The 9 pre-#1830 attached tracks and every child of an attached track cannot run codex or
  claude tasks.
- A failed attempt wakes the Planner twice: `task.failed`, then its candidate settlement.
- A reported worker's late write can land after the kernel commit and overlap the next attempt.
- A delivery that never settles (for example, the kernel cannot spawn git) holds the track.
- A managed track re-pointed to attached before its first lease has no worktree and is refused.
- A hand-removed track worktree fails D6 (`track-worktree-unavailable`); delete the track.
- Until S2b, the budget still admits several ready tasks into the claim, where D5 turns them
  back, and `BudgetQueued` does not explain the wait.

## 8. Deploy note and S3 boundary

- **At deploy (4140, once):** after the new kernel is running, run the following in each
  repository from G18.
  - `git worktree list --porcelain | awk '/^worktree .*\/\.claude\/worktrees\/[0-9a-f]{32}\/[0-9a-f]{32}$/{print $2}' | xargs -r -n1 git worktree remove --force`
  - `git for-each-ref --format='%(refname:short)' 'refs/heads/neige/' | grep -E '^neige/[0-9a-f]{32}/[0-9a-f]{32}$' | xargs -r git branch -D`

  Candidate refs stay.
- **S3:** a kernel push of `neige/track-<id>` whose tip equals the latest candidate commit, then
  `gh.pr.create`. After merge, or on a terminal lifecycle, remove the track worktree, its branch
  and its candidate refs. Moving the Planner's terminal into the worktree is not needed for
  acceptance and is left out.

## 9. S2b (follows S2, before deploy)

- **Delete `calm.task.replace`:** a successor already starts from the previous commit, so a
  cancel plus a new task does the same job. Delete:
  - `src/task_replace/` (1,093), `track_report/replace.rs`, `PersistPurpose::Replace` /
    `planner_replace`, `tools/task_replace.rs`;
  - the replace routing (`scheduler/mod.rs:1375-1392`), `candidate.carry`
    (`plan.rs:749-797`) and the recovery `Replaced` refusal;
  - `WorkerCleanupReason::Superseded` and `task_cancel_pending_with_detail_tx`;
  - 2 prompt files, and the `planner.md:76` bullet ("For another round, declare a new task…").

  The `task_replacements` table stays unread (no migration); `BaseSource::Attempt` decodes one
  lease row. Gates: `report_write_boundary.sh:26`, the MCP tool registry golden, the tool-name
  lists, `gate_binding.rs:718` and `target.rs:586-590`. About 1,500 production and 1,100 test
  lines.
- **Delete the budget knob:** D5 is the whole rule. Delete `DEFAULT_TRACK_TASK_BUDGET`, the env
  var, the `task_budget_default` setting (route, AppContext threading, FE control) and the
  `task_budget` PATCH field; the column stays unread. The ready set uses `track_idle`.
  `BudgetQueued{occupied,effective}` becomes `TrackBusy{message}`, which needs OpenAPI,
  `wire.ts`, the FE gates and a browser check. `tree_task_budget` is untouched. About 350
  production lines (grep estimate).

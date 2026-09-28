# #1830 S2 — workers run where the Planner runs

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) S1–S3 land before anything is deployed to
4140. Attached create on a non-git directory is refused, so it is not designed for.

**Outcome (owner decision, 2026-09-28).** Every codex or claude task runs directly in its track's
`agent_cwd()`:

- An attached track runs in its S1 track worktree
  ([`1830-track-worktree.md`](1830-track-worktree.md)), on `neige/track-<id>`.
- A managed track runs in its managed directory, on `main`.

A track runs one task at a time. After every attempt, whether it completed, failed or was
stopped, the kernel commits the directory and pins the commit as a candidate. The next attempt
starts from that commit. A worker does not start on a dirty tree; the refusal lists the files.

Deleted: the per-card lease worktree path (`.claude/worktrees/<track>/<card>`, `neige/<track>/<card>`,
worker upstream bases, provisioning, removal, reclaim), the task budget knob, `calm.task.replace`
with its carry, and the legacy `git.commit:auto`.

Attached tracks without a worktree are refused at worker start. Isolated
(`calm.task.dispatch`) and `terminal` tasks do not change.

## 1. Facts

Verified at 2bd0ce8eb (S1 head) by reading the code, or by the query or command shown (4140
`calm.db`, `events.max(id)=70638`, 2026-09-28).

| # | Claim | Where | Verified |
|---|---|---|---|
| G1 | Both worker adapters: `prepare_workspace_lease_target_tx` (per-card path and branch, repo root from `workspace_path`, managed materialize as a last chance) → `carry::resolve_task_lease_base_tx` → `acquire_workspace_lease_tx`; the lease path is the worker cwd; spawn provisions a worktree | `workspace_lease/mod.rs:133-164`; `codex_adapter/mod.rs:787-850`, `:1606-1636`; `claude_adapter/mod.rs:779-860`; `claude_adapter/workspace.rs:7-38` | read |
| G2 | `verify_worktree_base` checks HEAD = base, realpath = `canonical_path`, and HEAD on the target branch | `base.rs:810-850` | read |
| G3 | `workspace_leases_active_path_idx`: UNIQUE(`path`) WHERE state IN (`held`,`releasing`) | `migrations/0056_workspace_leases.sql:30-32` (re-created `0115:69`) | read |
| G4 | `BaseSource::Commit` is accepted by the CHECK and has no producer | `base.rs:44-62`; `0115` CHECK | read |
| G5 | `task_git_deliveries.lease_id` and the candidate's `lease_id` are `NOT NULL REFERENCES workspace_leases`; a gate reads the card's latest lease `path`; delivery provenance reads the lease's `base_sha`, `canonical_path` and `git_common_dir` | `migrations/0113_task_git_deliveries.sql:20`, `:58`; `task_verify_adapter/mod.rs:466-474`; `delivery.rs:165-215` | read |
| G6 | Managed materialize: an owner marker in `.git`, `git init` on `main`, and an empty initial commit; the 4140 managed dir is `status`-clean | `workspace_materialize.rs:224-340`, `:429` | read + command |
| G7 | Budget = `COALESCE(tracks.task_budget, settings.task_budget_default, NEIGE_TRACK_TASK_BUDGET, 1)`, applied in the ready set and in the claim tx (in-flight = dispatched/running/verifying + active isolated candidate verifications). Its read side is `BudgetQueued{occupied,effective}`. The setting has a FE control | `scheduler/mod.rs:57`, `:130-141`, `:1038-1052`, `:1276-1307`; `calm-truth/.../task_projection.rs:126-132`, `:396-452`; `routes/settings.rs:24-40`; `fe/core/domain/settings.ts:23` | read |
| G8 | 4140: `task_budget_default = 4`; no track sets `task_budget`; 6 of the 8 attached tracks that ran workers had concurrent leases | `settings`, `workspace_leases` self-join | query |
| G9 | The first delivery row is written only for a success report, in the report tx | `decision_sink.rs:203-215`; `delivery.rs:337-362` | read |
| G10 | The delivery (`GIT_DELIVERY_SCRIPT`) commits in `lease.path`: HEAD must be on `refs/heads/$2`, then `git add -A`, a commit if anything is staged, and `update-ref refs/neige/candidates/…`. The branch is `workspace_slice_branch_for`; the message names card and track only. The candidate `repo_root` is the inverse of the per-card path, and any other path is an error | `delivery.rs:112-119`, `:183`; `forge_git.rs:51-73`; `candidate.rs:44-64`, `:122` | read |
| G11 | Lease releases: after a report; the reaper (worker died); timeout/cancel cleanup after the kill; card delete; boot reclaim (older machine boot); compensation; track/area delete | `decision_sink.rs:248-256`; `reaper/mod.rs:548-567`; `scheduler/mod.rs:2064-2090`; `routes/cards.rs:1576`; `plugin_host/callbacks.rs:537`; `workspace_lease/mod.rs:401-437`, `:313-341`; `codex_adapter/mod.rs:1025-1094` | read |
| G12 | Unsettled deliveries are driven on every `schedule_pass` and on the 300 s tick. Task terminal events and `task.git_delivery_settled` poke the scheduler; `workspace.released` does not | `scheduler/mod.rs:992-999`; `scheduler/git_delivery.rs:174-234`; `dispatcher/mod.rs:1007-1035` | read |
| G13 | Prepare-time refusals end `spawn-failed: refused: …` (480 chars kept), which wakes the Planner even for a gated task | `upstream.rs:378-402`; `scheduler/mod.rs:1748-1807`; `dispatcher/mod.rs:223-232` | read |
| G14 | Legacy `git.commit:auto` runs only when there is no delivery row, and only in the per-card path it recomputes (`workspace_lease_path_for`) | `emit.rs:150-160`, `:248-278` | read |
| G15 | Planner `git.commit` runs in `agent_cwd()` and does `git add -A` + commit; git-forge has no discard action; Codex `workspace-write` makes the gitdir unwritable; worker prompts already say the platform commits | `transport.rs:974-981`; `plugins/git-forge/manifest.json:12-248`; `shared_codex_appserver.rs:1234-1255`; `prompts/worker/head-*.md:6` | read |
| G16 | The kernel builds no sandbox deny list; only the Claude Planner is sandboxed, and Claude Code derives deny paths from registered worktrees (#1815) | `claude_planner/spawn.rs:21-32`; `routes/claude_cards.rs:313-338` | read |
| G17 | Terminal default cwd is `workspace.path`; a child of an attached track inherits `parent.path` with no worktree | `terminal_adapter.rs:216-240`; `child_track_adapter.rs:113-126`; `calm-truth/.../track.rs:187-191` | read |
| G18 | 4140: 9 attached tracks, all pre-#1830 (8 `done`, 1 `draft`); 0 non-terminal tasks; 0 non-terminal operations; 0 `held`/`releasing` leases; 0 unsettled deliveries; 0 child tracks; managed workers on 2 tracks (4 leases) | `tracks`, `tasks`, `operations`, `workspace_leases`, `task_git_deliveries` | query |
| G19 | 4140 per-card residue on disk: 12 registered worktrees and 88 `neige/<t>/<c>` branches across 4 repositories (neige-calm: 9 and 83) | `git worktree list`, `for-each-ref` per leased track repo | command |
| G20 | 4140 `task_replacements`: 1 row, whose predecessor is `done`. Outside replace, only the reclaim clause and the recovery `Replaced` refusal read it | `task_replacements`; `reclaim.rs:52-59`; `task_recovery/admission.rs:148-158` | query + read |

## 2. Decisions

- **D1 Where a worker runs.** `prepare_worker_lease_tx(tx, track, card, workspace_root)`
  replaces the target, carry and base calls in both adapters. It picks the directory by track
  kind:
  - managed: materialize if needed (kept from G1), then `workspace.path`, branch `main`;
  - attached with a worktree: the worktree, branch `neige/track-<id>`;
  - attached without one (the 9 pre-#1830 tracks, children of attached parents):
    `refused: track-without-worktree: this track predates per-track worktrees; create a new track
    to run codex or claude tasks`.

  `main` is the one constant materialize already uses (G6). *Why:* this is the owner's rule, and
  running in the user's checkout is never an option.
- **D2 The per-attempt lease row stays and points at that directory.** The base is its HEAD
  (`BaseSource::Commit`, G4), `canonical_path` is its realpath, and the common dir is recorded.
  *Why:* deliveries and candidates reference the row, and gates and provenance read it (G5).
  Carrying this on the task row instead would mean a migration and rewriting those readers.
  The row keeps what it does: one row per attempt, the freeze at the first lease, and
  `workspace.leased` / `workspace.released`.
- **D3 Spawn only verifies.** `verify_worktree_base` (G2) replaces provisioning. The directory
  already exists, made by S1 ensure or by managed materialize. *Why:* nothing is created, so
  nothing needs to be cleaned, pruned or rolled back.
- **D4 One fence: the claim transaction admits a task only when its track is idle.** Idle means
  all of the following:
  - no current task is `dispatched`/`running`/`verifying` (the existing in-flight count,
    including active isolated candidate verifications);
  - no `held`/`releasing` lease;
  - no unsettled delivery.

  The ready set uses the same predicate with capacity 1 or 0. G3's unique index is the backstop.
  The dispatcher also pokes the scheduler on `workspace.released`.
  *Why each term is needed:*
  - the status term covers a gate reading the tree after release, and a task claimed before its
    lease;
  - the lease term covers a canceled or timed-out task whose worker is not yet killed;
  - the delivery term covers a done task whose commit has not landed.

  None of them implies another.
- **D5 The budget knob is deleted**, because D4 is the whole rule. That covers
  `DEFAULT_TRACK_TASK_BUDGET`, `NEIGE_TRACK_TASK_BUDGET`, the `task_budget_default` setting (route,
  AppContext threading, FE control), and the `task_budget` PATCH field and its readers. The
  `tracks.task_budget` column stays, unread, so there is no migration.
  `BudgetQueued{occupied,effective}` becomes `TrackBusy{message}` ("Queued behind the track's
  current attempt"), which needs OpenAPI, `wire.ts` and FE updates. `tree_task_budget` is a
  separate cap on child-track trees and is untouched.
- **D6 The clean-tree check** is in `prepare_worker_lease_tx`, before the lease INSERT, so a
  refusal writes no row and the release hook (D7) never commits the Planner's files.
  - It runs `git -c core.fsmonitor=false status --porcelain -z --untracked-files=normal` through
    `isolated_git_command`, under a deadline, using the bounded `run_git` moved out of
    `carry.rs`.
  - Untracked files count and ignored files do not. `--untracked-files` is forced so that a
    user's `status.showUntrackedFiles=no` cannot hide files that `git add -A` would later commit.
  - A dirty tree fails with `refused: track-worktree-dirty: <n> uncommitted path(s). Commit them
    with git.commit or undo them, then recover or re-declare the task: <paths…>`. The paths come
    last because only 480 chars are kept.
  - A git failure fails with `refused: track-worktree-unavailable: <git's message>`.
  - Both use G13's wire: `spawn-failed`, then a Planner wake.
- **D7 Commit after every attempt: a delivery row for every lease, written at release.** The
  success report keeps its row in the report tx (G9). `release_workspace_lease_tx` and
  `complete_workspace_lease_release` insert the first delivery row when the lease owner op's
  attempt has none, in the same tx. D4's poke then drives it (G12). *Why:* release is the one
  point where every path (§3) knows the worker has stopped writing. There are 2 functions there,
  against about 8 terminal-flip sites.
- **D8 The commit message is** `neige: attempt <attempt_id> <outcome> (delivery <id>)`. The
  outcome comes from `tasks.status` when the payload is built: done or verifying → `completed`,
  failed → `failed`, canceled → `canceled`, anything else → `interrupted`. There is no column.
  Every builder of one row sees a terminal status, so the op hash is stable.
- **D9 `lease_target(lease)` maps a lease path to its repository and branch.** A path of the
  form `track_worktree_target` gives the track branch and repo root. Any other path is a managed
  directory: the repository is the path itself and the branch is `main`. It is used at
  `delivery.rs:183`, `candidate.rs:122` and `facts.rs:135`. A historical per-card path (all 4140
  deliveries are settled, G18) reads as no branch.
- **D10 Compensation and every other release only flip the lease row.** The removal steps
  (`remove_workspace_artifact`, removal inside `release_workspace_lease_by_id`) are deleted.
  No lease-driven removal remains, so the `rm` hazard of a non-lease path is gone with it. The
  only removal left is S1's discard of the track worktree on track or area delete.
- **D11 Track and area delete** keep the S1 track-worktree removal and the candidate-ref
  deletion. The per-card directory and branch sweep is deleted. G19's residue is removed once, at
  deploy (§8).
- **D12 Gates do not change.** They read the lease path, which is now `agent_cwd`. D4 keeps a
  worker out while a gate runs.
- **D13 Delete `calm.task.replace` entirely.** A successor already starts from the previous
  commit, so a cancel plus a new task does the same job. The `task_replacements` table stays
  (no migration). Nothing reads it once the reclaim clause and the recovery `Replaced` refusal
  go (G20). `BaseSource::Attempt` stays as the decoder for one released lease row.
- **D14 Delete the legacy `git.commit:auto`.** It only runs on the per-card path, which is gone
  (G14).
- **D15 Prompt wording.** `prompts/planner.md` is shared by Codex and Claude.
  - `:73` becomes: "Your working directory is the track's git checkout (on an attached track,
    its worktree on `neige/track-<id>`). Codex and claude tasks run there one at a time, each
    starting from the kernel's commit of the previous attempt. Do not edit files while one is
    dispatched, running or verifying. A task starts only on a clean tree (`git status
    --porcelain` empty; ignored files do not count), so commit your edits with `git.commit` or
    undo them first. A `track-worktree-dirty` failure lists the files: clean them, then recover
    or re-declare the task. `track-without-worktree` means create a new track."
  - `:76` (replace) becomes: "For another round, declare a new task; it starts from the
    previous attempt's commit, including a failed one. To drop that work, say so in its goal."
  - `:77` becomes: "After every attempt, completed, failed or stopped, the kernel commits…"
  - `:72` becomes: "…its gate runs in the track's checkout…"
  - `head-*.md:6` becomes: "the platform commits after you report".
  - `target.rs:586-590`: the gate.cwd refusal says "then declare a new task".
- **D16 Deferred S1 §8 items:**
  - The terminal default cwd becomes `agent_cwd()` (`terminal_adapter.rs:225-240`), so the
    Planner's preview or push shell sees its own tree (G17).
  - The gate fallback stays. Its worker branch already reads the lease row; its unbound branch
    is legacy.
  - A child of a managed parent works. A child of an attached parent is refused (D1; 4140 has 0).
  - The Planner needs no writable gitdir: it uses `git.commit`, and read-only git is enough to
    undo (G15).
- **D17 Sandboxes do not change** (G16). The gain is 1 registered worktree and 1 branch per
  attached track, and none per attempt, which is what #1815's E2BIG counts. Workers need no
  write access to the gitdir.

## 3. How each attempt ends

| End | Task flip | Worker stopped when | Lease released by | Delivery row |
|---|---|---|---|---|
| `calm.task.complete` | done / verifying | after its turn (as today) | report handler | report tx, submitted at once |
| `calm.task.fail` | failed | after its turn | report handler | release (D7) |
| worker dies unreported | failed (reaper) | already | reaper | release |
| liveness timeout, `calm.plan.cancel` of a running task | failed / canceled + cleanup marker | sweep kills it | timeout cleanup | release |
| spawn fails after prepare | failed (`spawn-failed`) | never ran, or killed | compensation | release (usually `no_change`) |
| machine reboot mid-run | running until the timeout | already | boot reclaim | release (`interrupted`) |
| kernel crash after release, before submit | — | — | — | durable row; the next pass submits it |
| dirty tree, or no worktree, at prepare | failed (`spawn-failed`) | never ran | no lease | none (D6, D1) |

The only end without a commit is a delivery that runs and fails (a merge in progress, a switched
branch, or a git error). It settles `failed` and wakes the Planner as today. The tree stays dirty,
and the next start refuses with the file list. No kernel recovery commit is added: the delivery
is that commit (`calm.task.delivery retry`, or the Planner's `git.commit`).

## 4. Change list

Additions, about 300 production lines:

- `workspace_lease/mod.rs`:
  - `prepare_worker_lease_tx` (D1, D2, D6);
  - `lease_target` (D9);
  - the release hook (D7);
  - `run_git`, moved out of `carry.rs`.
- `codex_adapter/mod.rs`, `claude_adapter/{mod,workspace}.rs`: call `prepare_worker_lease_tx`,
  and verify only at spawn (D3).
- `scheduler/mod.rs`: the idle predicate in the ready set and in the claim (D4).
  `dispatcher/mod.rs`: the `workspace.released` poke.
- `delivery.rs`: the branch comes from `lease_target`, and the message is D8's. `candidate.rs`
  and `facts.rs`: `lease_target`.
- `terminal_adapter.rs`: `agent_cwd()`. `task_projection.rs`: `TrackBusy` (D5).
- Prompts (D15). `calm.plan.list.md` loses `candidate.carry`.
  `observation.rs:363-366` says "Accept with calm.task.verdict when the task completed". The
  `observation.rs:345` comment changes, and so does the `neige/<track>/<card>` wording in
  `workspace_materialize.rs:171-215` (the `refs/heads/neige` refusal stays: it also blocks
  `neige/track-<id>`).

Deletions: about 4,200 production lines and 5,800 test lines. The budget part is an estimate
from a reference grep.

| What | Prod lines |
|---|---|
| Per-card path in `workspace_lease/mod.rs`: target, `release_by_id` / `remove_artifact`, provisioning and stale-dir cleanup, `WorktreeRemoval`/`RemovalOutcome`/KeepWork, the per-card sweep (entries, slice branches, identity check, `worktree.removed` events), path parsing, `workspace_lease_path_for` / `workspace_slice_branch_for` | ~850 |
| `base.rs`: `resolve_lease_base`, the provision, sweep and removal identity checks, symlink-leaf handling, `WorktreeBase::LegacyUnpinned` | ~280 |
| #1815 reclaim: `workspace_lease/reclaim.rs`, `scheduler/worktree_reclaim.rs` | 588 |
| The worker upstream fetch (`refresh_track_upstream`, the `before_insert` hook and its driver call); frozen pre-slice recovery arms in both adapters (G18: 0 in-flight ops) | ~110 |
| `src/task_replace/` + `lib.rs:585` | 1,093 |
| `carry.rs` (less `run_git`); `track_report/replace.rs`; `PersistPurpose::Replace` / `planner_replace`; `tools/task_replace.rs` | ~500 |
| Replace callers: `decision_sink.rs:456-510`, `scheduler/mod.rs:1375-1392`, `plan.rs:749-797`, recovery `Replaced` (`admission.rs:148-158`, `refusal.rs:31,47,69,152`), `WorkerCleanupReason::Superseded`, `task_cancel_pending_with_detail_tx`, `test_seams.rs:188-205`, 3 prompt files | ~220 |
| Legacy `git.commit:auto`: `emit.rs:170-306`, `:326-344` | ~160 |
| Budget knob (D5): scheduler budget code, the settings key and route, AppContext / read-surface threading, the PATCH field, the FE settings control | ~350 |

Tests deleted: `task_replace{,_carry,_refusals}.rs` (1,688), `worktree_reclaim{,_guards,_kept}.rs`
(1,355), most of `workspace_lease/tests.rs` (~2,000 of 2,842), the `emit.rs` unit tests (~450),
and budget cases (~300).

Test support rewritten, not deleted: `test_seams.rs` lease helpers and the `git_delivery.rs`
fixture `kernel_lease` take the lease at `agent_cwd` (managed fixture track). The suite's
assertions stay.

Kept: the lease table and row, boot reclaim, S1's `choose_lease_start` (track-worktree create only),
the staleness view, managed materialize and recycle, and the history decoders
(`BaseSource::Attempt`, `worktree.*` events).

## 5. Gates and registries

- `scripts/ci/ratchets/report_write_boundary.sh:26`: drop `pub(crate)|planner_replace`.
- `tests/goldens/mcp_tool_registry.json`: drop the `calm.task.replace` entry (`:1480-1495`); the
  `calm.plan.list` hash changes (regenerate).
- `tests/goldens/issue_development_planner_prompt.txt:72,73,76,77`
  (`REGEN_PLANNER_PROMPT_GOLDEN=1`); `worker_prompt_{cli,mcp}.txt:6`.
- Tool-name lists `mcp_tools_list_role_filter.rs:37` and `mcp_assistant_tool_gate.rs:64`;
  `gate_binding.rs:718`; `dispatcher/tests.rs` (observation text); settings and schedulability
  tests (D5).
- The OpenAPI and `fe/core/api/generated/wire.ts` generators (`TrackBusy`, the removed setting).
  Then the `fe/` gates (`fe/AGENTS.md`) and a browser check of the settings page and the
  queued-task view.
- `scripts/gate-prose-ratchet.sh`: update the baseline if the deletions move its counts.
- Not triggered: no migration (so `head_schema_fixture` and `track_write_point_registry` do not
  change), `docs/oracle/*.yaml`, trybuild, the FE mutation manifests.

## 6. Tests

New file `tests/cases/track_worker_cwd.rs` in `mcp_integration_suite`. It uses real routes,
real git (a bare origin plus a clone for attached tracks), and the fixture codex worker, which
writes files and reports through MCP. Tracks are minted by the real create route.

| Test | Pins | Mutation that must turn it red |
|---|---|---|
| T1 `a_dirty_worktree_refuses_the_worker_and_lists_the_files` (attached). The worktree has a tracked edit, an untracked file and an ignored file. The task ends `spawn-failed: refused: track-worktree-dirty`, naming the first two and not the ignored file, with no lease or card row. After the Planner's `git.commit`, a re-declared task runs on that commit | D6 | M1: the check reports clean. Red first: today the worker gets a fresh per-card worktree |
| T2 `a_failed_attempt_is_committed_and_the_next_task_continues_from_it`. The worker writes `a.txt` and calls `calm.task.fail`. `neige/track-<id>` gains a commit whose message names the attempt and `failed`; the candidate ref points at it; the tree is clean. A task declared after that settlement has `base_sha` = that commit and reads `a.txt` | D7, D8 | M2: the release hook is skipped; M3: the outcome is always `completed` |
| T3 `an_attached_track_registers_one_worktree_and_one_branch`. Two successful tasks, the second declared after the first settles. Both worker cwds are the track worktree. `git worktree list` and `refs/heads/neige/*` each grow by exactly 1 from before the create, and no `.claude/worktrees/<track>/` exists | D1, D2 (acceptance) | M4: `prepare_worker_lease_tx` uses `workspace.path` instead of the worktree |
| T4 `a_track_runs_one_task_at_a_time`. Task a is made running through the `git_delivery.rs` fixture's `running_task` seam, and independent task b is ready. A scheduler pass leaves b `pending` | D4 status term | M5: the status term is dropped |
| T4b `the_next_task_waits_for_the_previous_commit`. `abort_event_listener_for_test` + `report_only` (the `git_delivery.rs:1763` pattern) leave a's delivery unsettled. A pass leaves b `pending`; after `reboot` + `wait_settled`, b is claimed with base = a's commit | D4 delivery term | M6: the delivery term is dropped |
| T5 `a_worktree_less_attached_track_refuses_workers`. A fixture `AttachedFromCwd` row (no worktree, as on 4140) gives `spawn-failed: refused: track-without-worktree`, and the checkout's `status` and HEAD are unchanged | D1 | M7: the attached arm falls back to `workspace.path` |
| T6 `a_managed_track_worker_commits_on_main_in_its_directory`. The worker cwd is `workspace.path`. A candidate commit lands on `main` there; no worktree is registered | D1, D9 | M8: `lease_target` gives a managed path the track branch |

Each mutation changes one production line. Every leasing test asserts its lease path at the
first lease. Predicted red sets:

- M1 → T1.
- M2 → T2 and the ordinary cancel and reaper tests.
- M3 → T2 and the same two ordinary tests (they assert the message).
- M4 → T1–T3.
- M5 → T4.
- M6 → T4b.
- M7 → T5.
- M8 → T6.

No test needs a new hook. T2, T3 and T6 declare their next task only after a settlement, so
they do not depend on D4.

Ordinary tests:

- `calm.plan.cancel` of a running task commits `canceled` after the kill.
- The reaper path commits `failed`.
- `lease_target` maps both shapes (unit).
- The clean check lists `status.showUntrackedFiles=no` files (unit).
- The terminal default cwd is `agent_cwd()`.
- `TrackBusy` shows for a pending task behind an unsettled delivery.

D4's lease term has no must-red test (holding a kill needs a missing hook); G3 backs it up.

## 7. KNOWN GAPS

- Managed tracks lose parallel workers: every track runs one task at a time (4140 ran budget 4).
- The 9 pre-#1830 attached tracks, and any child of an attached track, cannot run codex or
  claude tasks. Create a new track.
- A failed attempt wakes the Planner twice: once for `task.failed`, once for its candidate
  settlement.
- A gate or Planner edit that leaves non-ignored files makes the next start refuse, naming them.
- A delivery that never settles (for example, the kernel cannot spawn git) holds the track.
- A worker that writes after it reports races the kernel commit, as today. D6 catches it at the
  next start.
- A managed track re-pointed to attached before its first lease has no worktree and is refused.
- A hand-removed track worktree fails D6 (`track-worktree-unavailable`). Delete the track (S1).
- `task_replacements` (1 row) and the unread `tracks.task_budget` column remain as history.

## 8. Deploy note and S3 boundary

- **At deploy (4140, once):** after the new kernel is running, remove G19's per-card residue.
  For each repository listed in G19:
  `git worktree list --porcelain | awk '/^worktree .*\/\.claude\/worktrees\/[0-9a-f]{32}\/[0-9a-f]{32}$/{print $2}' | xargs -r -n1 git worktree remove --force`,
  then `git for-each-ref --format='%(refname:short)' 'refs/heads/neige/' | grep -E '^neige/[0-9a-f]{32}/[0-9a-f]{32}$' | xargs -r git branch -D`.
  Candidate refs stay; they pin candidates.
- **S3:** a kernel push of `neige/track-<id>` whose tip equals the latest candidate commit, then
  `gh.pr.create`. After merge, or on a terminal lifecycle, remove the track worktree, its branch
  and its candidate refs (the #1815 pattern). Managed directories keep their recycle on delete.

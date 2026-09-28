# #1830 S2 — workers run in the track worktree

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) S1–S3 land before anything is deployed to
4140. Attached create on a non-git directory is refused, so it is not designed for.

**Outcome.** A codex or claude task on a track that has a track worktree (S1,
[`1830-track-worktree.md`](1830-track-worktree.md)) runs in that worktree on `neige/track-<id>`,
one at a time. After every attempt, whether it completed, failed or was stopped, the kernel
commits the worktree and pins the commit as a candidate. The next attempt starts from that commit.
A worker does not start on a dirty tree; the refusal lists the files. The #1785
`calm.task.replace` carry mechanism is deleted. Tracks without a track worktree are unchanged.

## 1. Facts

Verified at 2bd0ce8eb (S1 head) by reading the code, or by the query shown (4140 `calm.db`,
`events.max(id)=70638`, 2026-09-28).

| # | Claim | Where | Verified |
|---|---|---|---|
| G1 | Both worker adapters take the target from `prepare_workspace_lease_target_tx` (`<repo_root>/.claude/worktrees/<track>/<card>`, branch `neige/<track>/<card>`, repo root from `workspace_path`), then `carry::resolve_task_lease_base_tx`, then `acquire_workspace_lease_tx`; the lease path is the worker cwd | `workspace_lease/mod.rs:133-164`; `codex_adapter/mod.rs:787-814`, `:842-850`; `claude_adapter/mod.rs:779-810`, `:847` | read |
| G2 | At spawn, `provision_workspace_worktree` with a registered, present target only runs `verify_worktree_base` (HEAD = base, realpath = `canonical_path`, HEAD on the target branch) | `workspace_lease/mod.rs:1188-1215`, `base.rs:810-850` | read |
| G3 | `workspace_leases_active_path_idx`: UNIQUE(`path`) WHERE state IN (`held`,`releasing`) | `migrations/0056_workspace_leases.sql:30-32` (re-created `0115:69`) | read |
| G4 | `BaseSource::Commit` is accepted by the CHECK and has no producer | `base.rs:44-62`; `0115` CHECK | read |
| G5 | Track budget: `DEFAULT_TRACK_TASK_BUDGET = 1`, but `COALESCE(tracks.task_budget, settings.task_budget_default, env)`; `verifying` occupies budget | `scheduler/mod.rs:57`, `:130-141`, `:1038-1052`, `:1276-1307` | read |
| G6 | 4140: `settings.task_budget_default = 4` (since 2026-09-05); no track sets `task_budget`; 6 of the 8 attached tracks that ran workers had overlapping leases (concurrent workers) | `settings`, `workspace_leases` self-join | query |
| G7 | The first delivery row is written only for a success report (`if success && rows == 1`), in the report tx | `decision_sink.rs:203-215`; `git_candidate/delivery.rs:337-362` | read |
| G8 | The delivery commits in `lease.path` with `GIT_DELIVERY_SCRIPT` (provenance, no in-progress op, HEAD on `refs/heads/$2`, `git add -A`, commit if staged, `update-ref refs/neige/candidates/<track>/<card>/<delivery>`); branch = `workspace_slice_branch_for`, message names card and track, not attempt or status | `delivery.rs:112-119`, `:165-215` (`:183`); `calm-types/src/forge_git.rs:51-73` | read |
| G9 | The candidate row's `repo_root` is the inverse of the per-card lease path; any other path is an error | `git_candidate/candidate.rs:44-64`, `:122` | read |
| G10 | A report releases the lease row right after the report tx; other releases: reaper (worker died), timeout/cancel cleanup after the worker is killed, card delete, boot reclaim (older machine boot), compensation, track/area delete | `decision_sink.rs:248-256`; `reaper/mod.rs:548-567`; `scheduler/mod.rs:2064-2090`; `routes/cards.rs:1576`; `plugin_host/callbacks.rs:537`; `workspace_lease/mod.rs:401-437`, `:313-326` | read |
| G11 | Unsettled delivery rows are driven by `schedule_pass` → `resume_git_deliveries` (every pass) and the 300 s reconcile tick; task terminal events and `task.git_delivery_settled` poke the scheduler, `workspace.released` does not | `scheduler/mod.rs:992-999`, `:60`; `scheduler/git_delivery.rs:174-234`; `dispatcher/mod.rs:1007-1035` | read |
| G12 | Settlement wake: ungated → `ungated_candidate`, not verifying → `gate_already_terminal`, else `deferred_to_gate`; the success self-report wake is deferred when a delivery row exists, a failure's never is | `scheduler/git_delivery.rs:349-363`; `dispatcher/mod.rs:196-234` | read |
| G13 | Worker op compensation runs `remove_workspace_artifact` + `release_workspace_lease` (both Discard); for a lease path that is not `<track>/<card>`-shaped, Discard falls back to `remove_dir_all(lease.path)` | `codex_adapter/mod.rs:1025-1094`; `claude_adapter/mod.rs:1233-1239`; `workspace_lease/mod.rs:1341-1363` (`:1354`), `:1168` | read |
| G14 | The #1815 reclaim selects released lease rows by SQL; a `failed` attempt is "finished" only when `task_replacements` names it | `workspace_lease/reclaim.rs:52-59`, `:78-130` | read |
| G15 | Gates run in the card's latest lease path and check it in place: HEAD = candidate commit, clean `status`, provenance; `gate.cwd` is refused for candidate-bound attempts | `task_verify_adapter/mod.rs:455-502`; `target.rs:563-660` (`:586-593`) | read |
| G16 | Prepare-time refusals (e.g. `attached-repo-diverged`) end the task `spawn-failed: refused: …` (status_detail keeps 480 chars), which wakes the Planner even for a gated task; a spawn failure before preparation is recoverable | `upstream.rs:378-402`; `scheduler/mod.rs:1748-1807`; `dispatcher/mod.rs:223-232`; `task_recovery/admission.rs:470-520` | read |
| G17 | Planner `git.commit` runs in `agent_cwd()` = the track worktree; `GIT_COMMIT_SCRIPT` = `git add -A` + commit; git-forge has no discard/reset/restore action | `mcp_server/transport.rs:974-981`; `plugins/git-forge/manifest.json:12-248` | read |
| G18 | Codex workers and Planners run `workspace-write` with no `writable_roots`: the linked worktree's gitdir is not writable. Worker prompts already say "Do not `git commit` … the platform commits after `calm.task.complete`" | `shared_codex_appserver.rs:1234-1255`; `prompts/worker/head-mcp.md:6`, `head-cli.md:6` | read |
| G19 | The kernel builds no sandbox deny list: only the Claude Planner is sandboxed (Claude Code derives deny paths from registered worktrees, #1815); Claude workers get hooks-only settings | `claude_planner/spawn.rs:21-32`; `routes/claude_cards.rs:313-338` | read |
| G20 | Terminal default cwd = `workspace.path` (user's checkout); `calm.terminal.open` always sends an empty cwd | `terminal_adapter.rs:216-240`; `mcp_server/tools/terminal.rs:537-558` | read |
| G21 | A child of an attached track inherits `parent.path` with `worktree: None` | `child_track_adapter.rs:113-126`; `calm-truth/src/db/sqlite/track.rs:187-191` | read |
| G22 | 4140 attached tracks: 9 live (8 `done`, 1 `draft`), all pre-#1830 (no worktree column yet); 0 non-terminal tasks anywhere; 0 `held`/`releasing` leases (92 released, 39 with `delivery_policy` NULL); 0 child tracks; isolated workers only on managed tracks (4); managed tracks: 2 ever leased (4 leases) | `tracks`, `tasks`, `workspace_leases`, `operations` | query |
| G23 | 4140 `task_replacements`: 1 row, predecessor `done` (track `5fe197ee…`, `done`); 1 lease with `base_source='attempt'`, released | `task_replacements`, `workspace_leases` | query |
| G24 | Legacy `git.commit:auto` runs only when an attempt has no delivery row: a lease with `delivery_policy` NULL (none active on 4140, every new lease is `kernel`) | `mcp_server/tools/emit.rs:150-160`, `:170-306`; `workspace_lease/mod.rs:246` | read + query |

## 2. Decisions

- **D1 Mode = whether the track row has `workspace_worktree_path`.** Set → track mode (this
  document). NULL → the per-card lease path, unchanged: managed, child and the 9 pre-#1830
  attached tracks (G22: no in-flight work, so no "old runs" exist). *Why:* the lease path must
  stay for managed tracks anyway, so the pre-#1830 rows cost nothing and need no refusal.
- **D2 One lease row per attempt, at the track worktree path.** A new
  `prepare_worker_lease_tx(tx, track, card, workspace_root) -> (WorkspaceLeaseTarget, LeaseBase)`
  replaces the two-call sequence in both adapters. In track mode, the target is S1's
  `track_worktree_target` (path, `neige/track-<id>`, repo root). The base is the worktree's
  `HEAD`, `BaseSource::Commit` (G4, which gets its first producer),
  `canonical_path = canonicalize(<root>/.claude/worktrees)/track-<id>`, and the common dir.
  *Why:* the delivery, candidate, gate and plan-view machinery all key on the lease row, so the
  row stays and only its target moves. No migration is needed. Spawn-side provisioning is
  unchanged: for a present worktree it verifies only (G2).
- **D3 One worker at a time per track worktree, enforced by the kernel.** S2 cannot rely on
  budget 1: 4140 runs with 4, and workers did overlap (G6). One helper,
  `effective_track_budget`, caps a track-mode track at 1, both where the scheduler builds the
  ready set (`track_budget`, `:1038`) and in the claim tx (`:1290`). The claim tx also refuses
  (race-lost, so the task stays `pending`) while the track has a `held`/`releasing` lease (a
  worker that may still write) or an unsettled delivery (a commit not yet made). G3's unique
  index is the backstop. `dispatcher/mod.rs` also pokes the scheduler on `workspace.released`,
  so a pending task is not left waiting for the 300 s tick.
- **D4 The clean-tree check** is in `prepare_worker_lease_tx`, before the lease INSERT. That
  point is inside the prepare transaction, so a refusal writes no row, and it comes before any
  lease exists, so the release hook (D5) can never commit the Planner's files. The check runs
  `git -c core.fsmonitor=false status --porcelain -z --untracked-files=normal` through
  `isolated_git_command` with a deadline, using the bounded `run_git` from `carry.rs`, which is
  the one part of that file kept. Untracked files count and ignored files do not.
  `--untracked-files` is passed so that a user's `status.showUntrackedFiles=no` cannot hide
  files that `git add -A` would later commit. A non-empty result is `CalmError::Conflict(
  "refused: track-worktree-dirty: <n> uncommitted path(s). Commit them with git.commit or undo
  them, then recover or re-declare the task: <paths…>")`, with the paths last because
  status_detail keeps 480 characters. A git failure, including a missing worktree, is
  `refused: track-worktree-unavailable: <git's message>`. Both take G16's wire: `spawn-failed`
  → `task.failed` → Planner wake → recover. *Why:* this is a precedent that is already wired,
  and a later check (provision) would release a lease and so trigger D5's commit.
- **D5 Per-attempt commit = a delivery row for every track-mode attempt, written at lease
  release.** The success report keeps writing its row in the report tx (G7; the replay-stable
  wake pairing depends on that, G12). `release_workspace_lease_tx` and
  `complete_workspace_lease_release` gain one step for a track-mode lease: if its attempt (the
  lease owner op's `idempotency_key`) has no delivery row, insert the first one
  (`insert_initial_delivery_tx`), in the same tx as the release. The row is then submitted by
  D3's poke → `resume_git_deliveries` (G11). *Why:* release is the one point where every path
  (§3) knows that the worker has stopped writing, and there are 2 release functions against about
  8 terminal-flip sites. Per-card leases keep today's success-only rule (managed behaviour
  unchanged).
- **D6 Commit message names attempt and outcome:** `neige: attempt <attempt_id> <outcome>
  (delivery <id>)`. `<outcome>` is derived when the payload is built, from `tasks.status`:
  done or verifying → `completed`, failed → `failed`, canceled → `canceled`, anything else →
  `interrupted`. *Why:* there is no column for it (no migration). The payload is built twice
  for one row only when no operation exists yet, and both builders see a terminal or
  success-mapped status, so the operation's semantic hash is stable.
- **D7 Track-aware lease target.** One helper, `lease_target(lease)`, returns S1's
  `track_worktree_target` when `lease.path` parses as the lease track's worktree, else the
  per-card target. It is used by the delivery branch (`delivery.rs:183`), the candidate
  `repo_root` (`candidate.rs:122`, which would otherwise fail the settlement, G9), the plan-view
  branch fallback (`facts.rs:135`) and removal. *Why:* one value, one meaning, and no new
  column.
- **D8 The track worktree is never removed through a lease.** In
  `remove_workspace_worktree_for_lease_as`, a track-mode lease is `Removed(false)` under
  Discard and `Refused` under KeepWork. This is the single choke point for compensation, for
  `release_by_id` and for the reclaim (G13 would otherwise `rm -rf` the worktree). The reclaim
  selection also skips rows whose `path` equals the track's `workspace_worktree_path`, so
  that it does not log a refusal per attempt per restart. Only track/area delete (S1 D7) or S3
  removes the worktree.
- **D9 Gates are unchanged.** A gate reads the card's lease path (now the track worktree) and
  still requires HEAD = candidate and a clean tree (G15). D3 keeps the next worker out while a
  task is `verifying`, and the prompt (D11) keeps the Planner out.
- **D10 Delete `calm.task.replace` entirely, not only its carry.** In track mode the successor
  already starts from the predecessor's commit, so an ordinary cancel plus a new task block
  covers what replace did. Its receipts gate nothing on 4140 (G23: the single predecessor is
  `done`). The table and `BaseSource::Attempt` stay as decode-only history (a migration is
  frozen, and one lease row reads `attempt`). Also subtract the legacy `git.commit:auto`
  (G24), which is dead on 4140 and would otherwise need a track-mode path.
- **D11 Planner and worker wording** (`prompts/planner.md`, shared by Codex and Claude):
  - `:73` becomes: "On an attached track your working directory is the track's git worktree
    (branch `neige/track-<id>`), and its codex and claude tasks run there one at a time, each
    starting from the kernel's commit of the previous attempt. Do not edit files while one of
    them is dispatched, running or verifying. A task starts only on a clean tree (`git status
    --porcelain` empty; ignored files do not count): commit your edits with `git.commit` or undo
    them before dispatch. A `track-worktree-dirty` failure lists the files; clean them, then
    recover or re-declare the task."
  - `:76` (replace) becomes: "When an attached producer needs another round, declare a new
    task; it starts from the previous attempt's commit, a failed one included. To drop that
    work, say so in its goal."
  - `:77`: "After every attached attempt, completed, failed or stopped, the kernel commits…"
  - `:72`: "…its gate runs in that worker's worktree…"
  - `head-*.md:6`: "the platform commits after you report".
  - `target.rs:586-590`: the gate.cwd refusal says "then declare a new task" instead of naming
    replace.
- **D12 Deferred items from S1 §8**, one line each:
  - Terminal default cwd moves to `agent_cwd()` (`terminal_adapter.rs:225-240`, one line), so
    that the Planner's preview and push shell sees its own tree (G20).
  - The gate fallback cwd stays: its worker branch already resolves to the track worktree through
    the lease row, and its unbound branch is legacy.
  - Child tracks stay on per-card leases (G21; 4140 has 0).
  - FE file reads already moved in S1.
  - The Planner needs no writable gitdir: it commits with `git.commit` and undoes by rewriting
    files (`git show HEAD:<path>` is read-only) (G17, G18).
- **D13 Sandboxes: no change.** The kernel builds no deny list (G19). The gain is that an
  attached track now registers 1 worktree and 1 branch instead of 1 + N of each, which is what
  #1815's E2BIG counts. Workers need no gitdir write, because the kernel commits (G18).

## 3. How each attempt ends (track mode)

| End | Task flip | Worker stopped when | Lease released by | Delivery row |
|---|---|---|---|---|
| `calm.task.complete` | done / verifying | after its turn (as today) | report handler | report tx (G7), submitted at once |
| `calm.task.fail` | failed | after its turn | report handler | release (D5) |
| worker dies unreported | failed (reaper) | already | reaper | release |
| liveness timeout, `calm.plan.cancel` running | failed / canceled + cleanup marker | sweep kills the worker | timeout cleanup | release |
| spawn fails after prepare | failed (`spawn-failed`) | never ran, or killed | compensation | release (usually `no_change`) |
| machine reboot mid-run | stays running until the timeout | already | boot reclaim | release (`interrupted`) |
| kernel crash after release, before submit | — | — | — | durable row; next pass submits |
| dirty tree at prepare | failed (`spawn-failed`) | never ran | no lease | none (D4) |

The one end with no commit is a delivery that runs and fails: an in-progress merge, a switched
branch, or a git error. That settles `failed`, wakes the Planner as today, and leaves the tree
dirty; the next start refuses with the file list. That is acceptable, and there is no separate
kernel recovery commit, because the delivery is that commit (`calm.task.delivery retry`, or the
Planner's `git.commit`).

## 4. Change list

Additions (about 250 production lines):

- `workspace_lease/mod.rs`: `prepare_worker_lease_tx` (it reads `workspace_worktree_path` in the
  same SELECT), with the clean check (D4) and `lease_target` (D7); the release hook (D5) in
  `release_workspace_lease_tx` and `complete_workspace_lease_release`; and the D8 guard. The
  bounded `run_git` moves here from `carry.rs`.
- `codex_adapter/mod.rs`, `claude_adapter/mod.rs`: call `prepare_worker_lease_tx`, and drop the
  carry notice.
- `scheduler/mod.rs`: `effective_track_budget`, and the two EXISTS in the claim tx (D3).
  `dispatcher/mod.rs`: the `workspace.released` poke.
- `git_candidate/delivery.rs`: branch via `lease_target`, and `delivery_message(attempt,
  outcome)` (D6). `candidate.rs`: `repo_root` via `lease_target`. `facts.rs:135`.
- `reclaim.rs`: skip track-mode rows (D8). `terminal_adapter.rs`: `agent_cwd()` (D12).
- Prompts (D11), `calm.plan.list.md` (drop `candidate.carry`), the `observation.rs:345` comment,
  and `observation.rs:363-366`: "Accept with calm.task.verdict when the task completed" (a
  failed attempt now settles a candidate too).

Deletions (about 2,000 production lines and 2,600 test lines):

| What | Lines |
|---|---|
| `src/task_replace/` (mod, admission, receipt, refusal, route, view) + `lib.rs:585` | 1,093 |
| `operation/workspace_lease/carry.rs` (except `run_git`, which moves) | ~250 |
| `track_report/replace.rs`; `PersistPurpose::Replace` + `planner_replace` in `track_report/write.rs` | 146 + ~40 |
| `mcp_server/tools/task_replace.rs` + registration | 68 |
| `decision_sink.rs:456-510` `commit_task_replace`; replace routing `scheduler/mod.rs:1375-1392`; `candidate.carry` `mcp_server/tools/plan.rs:749-797`; recovery `Replaced` refusal `task_recovery/admission.rs:150-156`, `refusal.rs:31,47,69,152`; reclaim clause `reclaim.rs:52-59`; `WorkerCleanupReason::Superseded`; `calm-truth task_cancel_pending_with_detail_tx` + export; carry test seam `test_seams.rs:188-205` | ~200 |
| prompts: `worker/carry-notice.md`, `task-replace/refusals.md`, `tools/calm.task.replace.md` | 23 |
| legacy `git.commit:auto`: `emit.rs:170-306`, `:326-344` | ~160 |
| tests: `tests/cases/task_replace{,_carry,_refusals}.rs`; `emit.rs` unit tests (`:423-875`); `git_delivery.rs` `slice1_lease_completing_after_slice2_stays_legacy`; replace cases in `worktree_reclaim_guards.rs`, `task_recovery/tests.rs` | ~2,600 |

Kept (worktree-less tracks): per-card lease target, provisioning, `resolve_lease_base` /
`choose_lease_start`, slice branches, the #1815 reclaim, and the per-track sweep.
`before_insert`'s upstream fetch is also kept; it is fail-soft, and track mode ignores its
result.

## 5. Gates and registries

- `scripts/ci/ratchets/report_write_boundary.sh:26`: drop `pub(crate)|planner_replace`.
- `tests/goldens/mcp_tool_registry.json`: the `calm.task.replace` entry (`:1480-1495`) goes, and
  the `calm.plan.list` description hash changes (regenerate).
- `tests/goldens/issue_development_planner_prompt.txt:72,73,76,77` (`REGEN_PLANNER_PROMPT_GOLDEN=1`),
  and `worker_prompt_{cli,mcp}.txt:6`.
- Tool-name lists `mcp_tools_list_role_filter.rs:37`, `mcp_assistant_tool_gate.rs:64`;
  `gate_binding.rs:718` (the refusal text); `dispatcher/tests.rs` (observation text).
- `scripts/gate-prose-ratchet.sh`: its counts may move with the deletions; update the baseline
  if they do.
- Not triggered: no migration (so `head_schema_fixture` and `track_write_point_registry` are
  unchanged), `docs/oracle/*.yaml`, trybuild, the FE mutation manifests.

## 6. Tests

The new file `tests/cases/track_worktree_workers.rs` goes in `mcp_integration_suite` beside
`git_delivery.rs`. It uses real routes, real git (a bare origin plus a clone), and the fixture
codex worker, which writes files and reports through MCP. The attached track is minted by the
real create route, so it has a worktree.

| Test | Pins | Mutation that must turn it red |
|---|---|---|
| T1 `a_dirty_track_worktree_refuses_the_worker_and_lists_the_files`: a tracked edit, an untracked file and an ignored file in the worktree. The task is `spawn-failed: refused: track-worktree-dirty`, which names the first two and not the ignored file, and leaves no lease or card row. After the Planner's `git.commit`, a re-declared task runs on the Planner's commit | D4 | M1: the check reports clean. Red first: today the worker runs in a per-card lease |
| T2 `a_failed_attempt_is_committed_and_the_next_task_continues_from_it`: the worker writes `a.txt` and calls `calm.task.fail`. `neige/track-<id>` gets a commit whose message names the attempt and `failed`, the candidate ref equals it, and the tree is clean. A task declared after that settlement has lease `base_sha` = that commit, and its worker reads `a.txt` | D5, D6 | M2: the release hook is skipped; M3: outcome is always `completed` |
| T3 `an_attached_track_registers_one_worktree_and_one_branch`: two successful tasks, the second declared after the first's candidate settles. Both worker cwds are the track worktree. `git worktree list` and `refs/heads/neige/*` each grow by exactly 1 from before the create, there is no `.claude/worktrees/<track>/` directory, and the two commits are on the track branch in order | D1, D2 (acceptance) | M4: `prepare_worker_lease_tx` ignores the worktree column |
| T4 `no_task_starts_while_a_gate_reads_the_track_worktree`: budget 4. A gated first task is `verifying` with its gate held; an independent second task stays `pending` until the gate ends | D3 cap | M5: `effective_track_budget` returns the budget unchanged |
| T4b `the_next_task_waits_for_the_previous_commit`: an ungated first task is `done`, and its delivery is held unsettled by blocking the fixture forge action. The second task stays `pending` until the delivery settles | D3 fence | M6: the claim tx drops its `(active lease OR unsettled delivery)` EXISTS |
| T4c `the_next_task_waits_until_a_canceled_worker_is_stopped`: `calm.plan.cancel` on a running first task, with the sweep's kill held. The second task stays `pending` until the lease is released | D3 fence | M6 |
| T5 `spawn_compensation_keeps_the_track_worktree`: the shared daemon is not running, so spawn fails and compensation runs. The premise is that the lease path is the track worktree. The directory, its registration and its branch survive, and the lease is released | D8 | M7: the D8 guard is removed |
| T6 `a_planner_terminal_opens_in_the_track_worktree` (`calm.terminal.open`) | D12 | M8: the default is `workspace.path` |

Each mutation changes one production line. Every must-red test that leases asserts, at its first
lease, that the path is the track worktree. Predicted red sets:

- M4 → T1–T5, T4b, T4c.
- M1 → T1.
- M2 → T2 and the ordinary canceled and reaper tests.
- M3 → T2 and the same two ordinary tests (they assert the message).
- M5 → T4.
- M6 → T4b, T4c.
- M7 → T5.
- M8 → T6.

T2–T3 declare the next task only after a settlement, so M5 and M6 cannot race them.

Ordinary tests:

- The canceled-running path commits with message outcome `canceled` after the kill.
- The reaper path commits with outcome `failed`.
- The reclaim skips a released track-mode lease.
- `lease_target` maps both shapes (unit).
- The clean check lists `status.showUntrackedFiles=no` files (unit, with a local config).

Existing tests whose premise S2 changes: those that create an attached track through the route
and assert the per-card lease path. Candidates are `forge_merge_crash_reboot.rs:701`,
`task_recovery_reads.rs:487-590`, `support/codex_fixture.rs:689`, and the E2E-only
`forge_template_e2e.rs:1469,2841` and `codex_forge_e2e.rs:132`. Each either moves to a fixture
`AttachedFromCwd` row, which stays worktree-less, or asserts the track worktree. The
per-card delivery suite (`git_delivery.rs`) uses fixture rows and keeps pinning lease mode.

## 7. KNOWN GAPS

- A failed attempt wakes the Planner twice: `task.failed`, then its candidate settlement.
- A gate or a Planner edit that leaves non-ignored files dirties the tree. The next start
  refuses and names the files.
- A delivery that stays unsettled (for example, the kernel cannot spawn git) holds the track's
  next worker until it settles.
- A worker that keeps writing after it reports races the kernel commit, as today. D4 catches
  the residue at the next start.
- Terminal and isolated tasks on a track-mode track share its budget of 1 (4140: 4 terminal
  tasks, no isolated ones on attached tracks).
- A hand-removed track worktree fails D4 (`track-worktree-unavailable`); delete the track (S1).
- A child of an attached track keeps per-card leases, and its Planner runs in the user's checkout.
- `task_replacements` (1 row) and `BaseSource::Attempt` (1 lease row) remain as history.

## 8. S2 / S3 boundary

- **S2:** everything above.
- **S3:** a kernel push of `neige/track-<id>` whose tip equals the latest candidate commit, then
  `gh.pr.create`. After merge or on a terminal lifecycle, S3 removes the track worktree, its
  branch and its candidate refs (the #1815 pattern). The per-card lease path and its reclaim
  stay for worktree-less tracks.

# #1830 S3 — publish the verified candidate, reclaim the track worktree when the track ends

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) S1, S2, S2b and S3 land before deploy.
(4) Credentials: child environments come from allowlists, configuration from typed inputs.

**Outcome.** The Planner of an attached track calls one kernel tool, `calm.track.publish`. The
kernel pushes `neige/track-<id>` to the checkout's upstream remote only when the branch tip is the
commit of a `done` attempt of this track. It then opens the PR, or reuses the open one, and
checks that the PR head is that commit. Once the track has ended (done, canceled, failed) and
its work is on the remote, the terminal sweeper removes the track worktree, its branch and its
candidate refs, reusing the delete path. If a later Planner turn needs the directory, it is made
again. Managed tracks are refused.

## 1. Facts

Verified at c3886d776 (S2 merged) by reading the code, or by the query or command shown (4140
`calm.db`, `events.max(id)=71732`, 2026-09-29).

| # | Claim | Where | Verified |
|---|---|---|---|
| H1 | Plugin forge tools are stateless lowerings: `lower()` maps a tool name and its arguments to argv. The plugin reads no database | `plugins/git-forge/main.rs:123-136` | read |
| H2 | `gh.pr.create` lowers to `gh pr create --repo --head --base --title --body`, `parked: true`, idem key `gh.pr.create:<repo>:<base>:<head>`, event `forge.pr.opened{pr_number:/number, head_sha:/headRefOid}` | `main.rs:220-292` | read |
| H3 | `gh pr create` has no `--json`: live stdout is a URL, so JSON extraction fails and the op is decided by the probe and the output probe | `gh pr create --help` (2.74.2); `forge_action_adapter/mod.rs:462-476`, `:885-925`, `:1095` | command + read |
| H4 | The forge child env is `env_clear` plus PATH, HOME, LANG, LC_ALL, TERM, the configured proxies and the passthrough keys GH_TOKEN, GITHUB_TOKEN, GH_ENTERPRISE_TOKEN, GITHUB_ENTERPRISE_TOKEN, SSH_AUTH_SOCK, GIT_SSH_COMMAND, GH_HOST, NO_PROXY. Probes get the same env | `forge_action_adapter/mod.rs:53-67`, `:352-375`; `terminal_adapter.rs:971-983` | read |
| H5 | `gh pr create --head` skips gh's own push; the head must already be on the remote (#1830) | `gh pr create --help` | command |
| H6 | The Planner's forge cwd is `agent_cwd()`, i.e. the track worktree | `mcp_server/transport.rs:958-982` | read |
| H7 | A kernel-built forge action needs no running plugin: a delivery submits its own argv under `GIT_FORGE_PLUGIN_ID` through `submit_forge_action_with_key` | `git_candidate/delivery.rs:413-432`; `transport.rs:833-878`; `tools/emit.rs:23` | read |
| H8 | A keyed operation row is permanent. A resubmit with the same key and payload returns the old op, even a failed one | `operation/driver.rs:129-139` | read |
| H9 | `parked` decides only whether the MCP call waits (`runtime.wait`) or returns `{op_id, parked:true}`. Every forge event kind is a success event, so a parked failure wakes no one | `transport.rs:919-936`; `dispatcher/mod.rs:71-75` | read |
| H10 | `task_candidates` has `track_id`, `producer_attempt_id` (= `tasks.id`), `commit_sha` and `ref_name`, and is immutable. Only the gate flip, or an ungated report, writes `done` | `migrations/0113…:53-69`; `0097…:107-108`; `git_candidate/verification.rs:40-41` | read |
| H11 | The delivery script commits only a non-empty index, so an attempt that changed nothing pins the same commit as its predecessor | `calm-types/src/forge_git.rs:51-73` | read |
| H12 | `head_upstream(repo_root)` gives `remote`, `merge` and the effective `url` of the checkout's HEAD branch upstream. `last_known_upstream` gives its last-known commit, and `is_ancestor` is available | `workspace_lease/upstream.rs:36-60`, `:89`, `:236`, `:406` | read |
| H13 | `gh -R` accepts https and scp/ssh URLs but not a filesystem path | `gh pr list -R https://example.invalid/a/b`, `-R git@example.invalid:a/b.git` (both reached the host), `-R /tmp/x.git` (refused) | command |
| H14 | S1 teardown: `WorkspaceTrackSweep` → `remove_track_worktree` (Discard: `worktree remove --force`, `branch -D`) plus `delete_candidate_refs_for_track` over the lease rows' common dirs. Best effort, never fails | `workspace_lease/mod.rs:314-366`; `track_worktree.rs:129-140`; `git_candidate/refs.rs:24-60` | read |
| H15 | `ensure_track_worktree`: a registered, present directory is a no-op; an existing branch is checked out again; otherwise a new branch starts at `choose_lease_start` | `track_worktree.rs:63-109` | read |
| H16 | done, canceled and failed are terminal. `terminal_at` is stamped on entry and cleared on reopen. The user's "Resume work" leaves any of them | `calm-types/src/model.rs:188-195`; `calm-truth/src/db/sqlite/track.rs:264-275`; `calm-types/src/track_lifecycle.rs:99-108` | read |
| H17 | The Planner still takes turns on a done track (only commit observations are dropped). Both providers issue a turn through `backend.turn_start` | `harness/run_loop.rs:2728-2770`, `:3058-3062`, `:3214`; `harness/backend.rs:45-63` | read |
| H18 | 4140: git-forge is **not** installed (plugins: wisburg, market, longbridge, barra, paper-trading). All 94 forge-action ops are kernel commits (41 `git.commit:auto`, 53 deliveries). There are 0 `forge.pr.*` events. Lifecycle: 37 `reviewing→done`, 2 `reviewing→failed`, 0 canceled, and 6 reopens from done (every one by the user on a live track) | `plugins`, `operations`, `events` | query |
| H19 | `terminal_sweeper::sweep(&AppState)` runs every 30 s (orphan, completed-track and thread arms) and is 736 lines long; tests drive `sweep` directly | `terminal_sweeper.rs:57`, `:80-94` | read |
| H20 | S2 helpers: `track_idle_tx` (no in-tree worker, held lease or unsettled delivery) and `dirty_paths` (`status --porcelain -z --untracked-files=normal`) | `workspace_lease/worker.rs:184`, `:283` | read |
| H21 | Forge ops carry `target_type='track'`, `target_id=<track>`, and `payload_json.idem_key`. `track_has_active_forge_action` fences on them | `workspace_lease/mod.rs:89-120`; `forge_action_adapter/mod.rs:86-102` | read |
| H22 | The FE labels every `calm.track.*` tool except `rename` as a read ("Reading the track") | `fe/core/domain/conversation.ts:994-1000`; `fe/core/keys/mcp-tools.ts:28-31` | read |
| H23 | A managed directory is `git init` on `main` with no remote | `workspace_materialize.rs:305-340` | read |
| H24 | The test `gh` shim treats `--repo` as a bare git dir. Its `pr create` returns an existing PR by head, and a new PR's `headRefOid` is the remote's branch tip | `tests/support/gh_shim.rs:103-205` | read |

## 2. Decisions

- **D1 A kernel tool, not a git-forge action.** `calm.track.publish` is Planner-only
  (`visible_to_roles: [Planner]` + `require_role`). *Why:* the equality rule needs the database,
  which a plugin lowering cannot read (H1). The plugin is not installed on 4140 (H18). The kernel
  already submits its own forge actions (H7), so the push and PR reuse the forge operation's go
  token, probe, env and deadline. One tool pushes and opens the PR, so the PR head is exactly
  what was pushed. `gh.pr.create` in the plugin is left unchanged (Q1).
- **D2 Inputs: `{idempotency_key, title, body}`.** The remote, the gh `--repo` (`url`, H13) and
  the base (`merge` without `refs/heads/`) come from `head_upstream(repo_root)` (H12), where
  `repo_root` is `track_worktree_target(..).repo_root`. With no upstream, or with remote `.`, the
  tool refuses `refused: publish-no-upstream: <repo_root> has no upstream remote to push to; set
  one with git branch --set-upstream-to and retry`.
- **D3 The equality rule.** The tip `git rev-parse --verify refs/heads/neige/track-<id>^{commit}`
  (isolated git, run in `repo_root`) must equal the `commit_sha` of a `task_candidates` row of
  this track whose attempt `tasks.status = 'done'` (H10). This is one SQL read and adds no new
  state. *Why "a done candidate equal to the tip" and not "the latest candidate":* since S2 every
  attempt commits. So a tip equal to a done attempt's commit means the branch holds nothing
  unverified. An attempt that changed nothing re-pins the same commit (H11), and "latest" would
  refuse that tip. Refusals, answered before any operation exists:
  - `refused: publish-not-a-candidate: neige/track-<id> is at <tip>, which no attempt of this
    track produced (latest candidate <sha>, attempt <id>, <status>). A commit made after the last
    attempt is not verified: let a task produce it, or undo it.`
  - `refused: publish-candidate-not-done: <tip> is the candidate of attempt <id>, which is
    <status>; only a done attempt's commit can be published.`
- **D4 The push pushes the SHA.** New `GIT_TRACK_PUBLISH_SCRIPT` in `calm-types/src/forge_git.rs`
  takes `$1 sha $2 branch $3 remote $4 url $5 base $6 title $7 body`. It first checks tip = `$1`
  and exits 20 with both SHAs otherwise, so a commit made after D3's check never reaches the
  remote. Then it runs `git -c core.hooksPath=/dev/null push --porcelain "$3" "$1:refs/heads/$2"`.
  - Never force. A non-fast-forward push fails with git's message.
  - No upstream tracking is set: `--head` needs none (H5).
  - Hooks are off because the forge env carries the forge tokens (H4), and a pre-push hook is
    repository code the worker could have written. The candidate already passed its gate.
- **D5 The PR.** After the push, the script reuses the open PR whose head is the branch; if there
  is none, it runs `gh pr create --repo "$4" --head "$2" --base "$5" --title "$6" --body "$7"`.
  Last, `gh pr view "$2" --repo "$4" --json number,headRefOid` must show `headRefOid == $1`
  (exit 21 otherwise). That line is the stdout `forge.pr.opened{pr_number, head_sha}` reads, so
  a live run extracts directly; `gh.pr.create` cannot, because its stdout is a URL (H3).
- **D6 The operation.** `submit_forge_action_with_key(GIT_FORGE_PLUGIN_ID, track, planner card,
  cwd = the track worktree, …)`, with these settings:
  - idem key `track.publish:<sha>:<idempotency_key>`: keys are permanent (H8), so a failed
    publish is retried with a new key;
  - `parked: false`: the Planner waits (300 s deadline) and gets success or failure inline, since
    a parked failure wakes no one (H9);
  - probe: landed iff `git ls-remote "$3" refs/heads/$2` prints `$1` and the open PR's
    `headRefOid` is `$1`; the output probe is D5's `gh pr view` line.

  Credentials are exactly H4. No key is added.
- **D7 Managed tracks, and attached tracks without a worktree, are out of scope** and get
  `refused: publish-needs-track-worktree: only an attached track with its own worktree can be
  published (a managed track has no remote)`. H23 shows a managed directory has no remote.
- **D8 One reclaim trigger: the track has ended.** It is `terminal_at` older than
  `TRACK_WORKTREE_RECLAIM_GRACE = 1 h` in a terminal lifecycle. It is not the merge, for three
  reasons:
  - a human merge on GitHub is invisible to the kernel;
  - after a merge the Planner still needs its checkout: `gh.issue.close` is a forge action run in
    it (H6), and its next turn spawns there;
  - the Planner marks done after merging (template F4).

  The grace covers the rest of the Planner's done turn and gives the user time for a quick
  "Resume work".
- **D9 The reclaim predicate (all must hold):**
  - the worktree directory exists;
  - no forge action is active (H21);
  - `track_idle_tx` holds (H20);
  - the tree is clean (`dirty_paths` is empty; ignored files do not count);
  - either the tip is **published** (a `succeeded` forge op of the track whose `idem_key` starts
    `track.publish:<tip>:`), or the tip is an ancestor of `last_known_upstream(repo_root).sha`
    (the branch holds nothing of its own, as with an investigation track, H12).

  Anything else is kept until track delete: short of deletion, the kernel never discards
  unpublished commits or uncommitted edits. *Why published, not merged:* a squash merge makes
  the tip a non-ancestor, and a published head stays on the forge (PR ref) whatever the merge.
  The published record is the operation row, so no new state is needed.
- **D10 The removal reuses the delete path.** The sweep takes
  `lock_key(state.track_delete_locks(), id)`, then re-reads that the lifecycle is still terminal
  (so a reopen in the window wins). It then builds the `WorkspaceTrackSweep` with the existing
  `workspace_track_sweep_for_track_tx` and calls `sweep_workspace_worktrees_for_tracks` (H14).
  That removes the worktree, the branch and all `refs/neige/candidates/<track>/…` refs. The
  column keeps its path, and no event is written (S1 D7).
  - Remote branch: left to `gh.pr.merge --delete-branch` (`main.rs:438`) or the forge's
    auto-delete.
  - Code: a new `workspace_lease/track_reclaim.rs`, called as a fourth arm of
    `terminal_sweeper::sweep` (30 s, `AppState` in hand, H19). A refused track is checked again
    next tick (one `rev-parse` while unpublished).
- **D11 A Planner turn makes a missing track worktree again.** In `run_loop.rs` just before
  `backend.turn_start` (both providers, H17): if `track.workspace.worktree` is set and the
  directory is missing, call `ensure_track_worktree(&track)` (H15), with a `warn!` on error so
  the turn fails as it does today.
  - After a reclaim, the branch is gone, so a new `neige/track-<id>` starts from the upstream.
    That covers a reopen (6 of 37 done tracks on 4140, H18) and any observation on a done track.
  - It also closes S1's hand-removed-worktree gap.
  - A new tip sits in the upstream, so D9 reclaims it again once the track has ended.
- **D12 Prompt.**
  - `prompts/planner.md`: a new bullet after `:77`: "To deliver an attached track, when
    `neige/track-<id>` is at a done attempt's commit call `calm.track.publish` (title, body): it
    pushes that commit and opens or reuses the PR against the upstream branch. It refuses a
    commit made after the last attempt. Merge as your template says, then set the track `done`;
    the kernel later removes the worktree, branch and candidate refs."
  - `templates/builtin/issue-development.md:69`: "…open a PR with calm.track.publish…".
- **D13 FE:** `TRACK_PUBLISH_TOOL` in `fe/core/keys/mcp-tools.ts`, and a branch before the prefix
  fallback in `conversation.ts` ("Publishing the track" / "Published the track"), as the file's
  own comment requires (H22).

## 3. Change list (about 350 production lines, no migration)

- `calm-types/src/forge_git.rs`: `GIT_TRACK_PUBLISH_SCRIPT`, `GIT_TRACK_PUBLISH_PROBE_SCRIPT` and
  `GIT_TRACK_PUBLISH_OUTPUT_PROBE_SCRIPT` (D4–D6).
- `mcp_server/tools/track_publish.rs` (new, about 200 lines): descriptor, role check, D2/D3/D7
  refusals, payload and submission (D6). `prompts/tools/calm.track.publish.md`. Register it in
  `tools/mod.rs`.
- `workspace_lease/track_reclaim.rs` (new, about 120 lines): selection SQL
  (`lifecycle IN ('done','canceled','failed') AND workspace_worktree_path IS NOT NULL AND
  terminal_at <= now - grace`), the D9 predicate and the D10 removal.
  `terminal_sweeper.rs`: one call. `worker.rs`: `dirty_paths` becomes `pub(super)`.
  `workspace_lease/mod.rs`: `workspace_track_sweep_for_track_tx` becomes `pub(super)`.
- `harness/run_loop.rs`: one call before `turn_start` to `track_worktree::ensure_present(&track)`
  (new, about 15 lines: `is_dir` fast path, then `ensure_track_worktree`) (D11).
- Prompts and template (D12). FE (D13).

## 4. Gates and registries

- `tests/goldens/mcp_tool_registry.json` (new entry and description hash);
  `tests/cases/mcp_tools_list_role_filter.rs:38-44` (Planner list);
  `tests/cases/mcp_assistant_tool_gate.rs:26-83` (denied, planner-reachable).
- Planner prompt golden `issue_development_planner_prompt.txt` (`REGEN_PLANNER_PROMPT_GOLDEN=1`).
- FE: `conversation.test.ts` gets the new label; run `(cd fe && npm ci && npm run lint && npm run
  build && npm test)`.
- `tests/support/gh_shim.rs`: `pr view --json number,headRefOid,state`.
- `scripts/local-ratchet-gates.sh` (prose and terminology ratchets).
- Not triggered: migrations and `head_schema_fixture`; `track_write_point_registry` (no
  `tracks` write); OpenAPI and `wire.ts` (no REST change); `scripts/ci/ratchets/*`;
  `gate-sync-event-version-lockstep.sh` (no new event kind).
- S2b edits `planner.md:76` in parallel. S3 inserts after `:77`, so rebase after S2b and
  regenerate the golden.

## 5. Tests

Fixtures: a local bare origin and a clone (`support::git_helpers::{init_bare_origin,
clone_for_track}`), with the track worktree made by the production `ensure_track_worktree`
(`test_seams::attach_track_worktree_for_test`). Candidates come from the real delivery, with the
test-played worker as in `track_worker_cwd.rs`. The `gh` shim goes on PATH under
`support::forge_env::{FORGE_ENV_LOCK, EnvGuard}`, and `--repo` is the bare origin's path
(H13, H24). No real repository and no network. The P and R tests live in the new
`tests/cases/track_publish.rs` in `mcp_integration_suite`. P4 is a unit test beside the script,
and R4 is in `claude_planner_wiring.rs` (`planner_harness_suite`).

| Test | Pins | Mutation (one production line) |
|---|---|---|
| P1 `publish_pushes_the_candidate_and_opens_its_pr`: a done attempt gives candidate C; publish returns `pr_number` and `head_sha == C`; origin `refs/heads/neige/track-<id>` == C; the shim PR's `headRefOid` == C; one `forge.pr.opened{head_sha: C}`; the clone's HEAD and status are unchanged | D4–D6 | M1: the script's `git push` line is dropped |
| P2 `publish_refuses_a_commit_made_after_the_last_attempt`: after C, the Planner's `git.commit` adds D. The refusal is `publish-not-a-candidate` and names D and C; origin has no track branch; no forge op row | D3 | M2: the candidate query ignores `commit_sha` |
| P3 `publish_refuses_a_failed_attempts_candidate`: the only attempt writes a file and calls `calm.task.fail`. The refusal is `publish-candidate-not-done` and names the attempt and `failed` | D3 | M3: the query drops `status = 'done'` |
| P4 `the_publish_script_refuses_a_moved_tip_before_pushing` (fixture repo and bare remote, `sh -c` the production constant with `$1` ≠ tip): exit 20, and the remote has no branch | D4 | M4: the script's tip check is dropped |
| R1 `an_ended_published_track_loses_its_worktree_branch_and_candidate_refs`: P1, then the Planner moves the track working→reviewing→done; `terminal_at` is backdated by SQL; one `terminal_sweeper::sweep`. The directory is gone and unregistered, the branch and `refs/neige/candidates/<id>/` are empty, `git worktree list` and `refs/heads/neige/*` match the counts before the create (acceptance), and the clone and origin are untouched | D8–D10 | M5: the sweeper arm is not called; M6: the reclaim skips candidate refs |
| R2 `an_ended_track_with_unpublished_commits_keeps_its_worktree`: a done attempt's C is never published; done, backdated, sweep: everything is kept | D9 published | M7: the published clause always holds |
| R3 `an_ended_track_with_uncommitted_edits_keeps_its_worktree`: no attempt (tip in the upstream) and one untracked file; done, backdated, sweep: kept | D9 clean | M8: the clean check is dropped |
| R4 `a_planner_turn_recreates_a_reclaimed_track_worktree`: the worktree and branch are removed with the production `remove_track_worktree`, then a Planner turn runs. The fake's recorded `pwd` is the worktree, which exists on a new branch at the origin tip | D11 | M9: the pre-turn ensure is dropped |
| R5 `an_ended_track_with_nothing_of_its_own_is_reclaimed`: no attempt, clean; done, backdated, sweep: the worktree and branch are gone | D9 upstream clause | M10: the upstream clause is dropped |

Predicted red sets over `track_publish`, P4's module and `claude_planner_wiring`:

| Mutation | Red |
|---|---|
| M1 | P1, R1 (R1 asserts P1's publish first) |
| M2 | P2 (P3 has no done attempt) |
| M3 | P3 |
| M4 | P4 |
| M5 | R1, R5 |
| M6 | R1 |
| M7 | R2 |
| M8 | R3 |
| M9 | R4 (S1's T3 has a present worktree) |
| M10 | R5 |

Ordinary tests:
- a managed track is refused, and so is a checkout without an upstream;
- a track inside the grace is kept, and so is one with an active forge op;
- a second candidate is pushed fast-forward and reuses the PR;
- a non-fast-forward push fails with git's message;
- a failed publish is retried under a new key.

## 6. KNOWN GAPS

- A Planner turn still running on an ended track when the reclaim fires loses its checkout for
  the rest of that turn. The next turn makes it again (D11). The 1 h grace makes this rare.
- An ended track with unpublished commits or uncommitted edits keeps its worktree until the track
  is deleted.
- The PR base is the checkout's upstream at publish time, not at track creation.
- A rewritten branch cannot be published (no force push). A remote track branch left by a merge
  without delete makes a later publish from a re-created worktree non-fast-forward.
- A Planner can still push or open a PR by hand (a terminal, or the plugin's `gh.pr.create`).
  The kernel only guarantees what `calm.track.publish` does.
- Push and gh authentication are whatever the kernel's HOME and H4's passthrough keys provide.
  Nothing new is added, and a failure is reported inline.
- After a reclaim, `plan.list` still names the attempts' candidate refs, which no longer resolve.

## 7. Open questions (owner)

- **Q1** Should git-forge `gh.pr.create` be deleted, so that the only PR path is the candidate-equal
  one? It is not needed for acceptance. It touches `forge_template_e2e`, `codex_forge_e2e` and
  `mcp_git_forge_plugin`. The design recommends a follow-up.
- **Q2** The issue-development template's `gh.pr.merge` and `gh.issue.*` need the git-forge plugin,
  which 4140 does not have (H18). Should it be installed at deploy?
- **Q3** Is the 1 h grace acceptable? The alternative, reclaiming immediately with D11 re-creating
  the worktree, trades the mid-turn gap for fewer knobs.

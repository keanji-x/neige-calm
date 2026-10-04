# #2058 S1 — a long-running track catches up with main and is delivered

**Owner rules.** (1) Pain points only; hypotheticals are one-line KNOWN GAPS. (2) Compatibility
means the 4140 database only. (3) Change a rule before adding a mechanism. (4) One consistent
agent-facing surface. (5) The kernel makes causes findable; it does not classify them.

**Review tier: L2.** Publish starts rewriting a pushed branch (an authority boundary), and the
task field adds a migration (a persistence boundary).

**Outcome.** The issue's two rule changes, refined by the code: `neige.track.publish` may replace
the track's own branch, leasing against a remote head an attempt of this track made; catching up
is an ordinary task with `start: "upstream"` (the kernel fetches, starts the checkout at the
upstream, and the worker replays the last done commit), delivered as one single-parent commit.

## 1. Problem (#2058, track `1022c771…`, PR #2042)

Main moved 6 commits during a 90-minute track; the PR conflicted and CI did not start. Three
rules blocked every legal path: the worker cannot write git metadata (`integrate-main-2042`,
event 86473), publish never forces, and CI rejects merge commits in the PR range. The
workaround cost about 80 minutes and 10 tasks: a terminal merge commit, a red ownership check, a
linear rewrite in a /tmp clone with a 111-line force-with-lease script, a ratify, and a
no-change task to rebind the candidate.

## 2. Facts

Verified at fd26e2267 by reading the code, or by the command shown.

| # | Claim | Where | Verified |
|---|---|---|---|
| K1 | Codex workers run `workspace-write`. The linked worktree's gitdir is not writable, so `fetch` and `merge` fail. Claude workers have no such restriction from the kernel (S2 G16) | `shared_codex_appserver.rs:1195`; 1830 S1 §8; #2058 event 86473 | read |
| K2 | A worker runs in the track worktree at its HEAD. The lease base is that HEAD (`BaseSource::Commit`), and spawn only checks that HEAD == base on the branch | `workspace_lease/worker.rs:166-219`, `:312-329`; `codex_adapter/mod.rs:1537`; `claude_adapter/mod.rs:1018` | read |
| K3 | The delivery stages the tree with `add -A` and commits on HEAD's branch only when the index is non-empty. The commit therefore has one parent, HEAD. It pins `refs/neige/candidates/<track>/<card>/<delivery>` | `calm-types/src/forge_git.rs:70-89`; `git_candidate/delivery.rs:147-149` | read |
| K4 | Publish requires the tip to equal a done candidate. It reads every `task_candidates` row of the track with its attempt's status | `builtin_plugins/dev/publish.rs:98-153`; `migrations/0113…:53-67` | read |
| K5 | The publish script pushes `$1:refs/heads/$2` without force, reuses an open PR, and exits 21 unless the PR head is `$1` | `forge_git.rs:106-123` | read |
| K6 | After release, a non-zero exit is settled by the probe. "Not landed" fails with a fixed text that drops the exit code. So a push that landed before a `gh` failure or exit 21 can leave the op `failed`, with no `forge.pr.opened` | `forge_action_adapter/mod.rs:900-928`, `:1082-1086`, `:1095-1146` | read |
| K7 | The only record of a published head is the succeeded op's `forge.pr.opened{track_id, pr_number, head_sha}`. Nothing records a push that landed under a failed op | `publish.rs:248-257`, `:316-325`; `forge_action_adapter/mod.rs:444-452` | read |
| K8 | The push runs in the forge child: `env_clear` plus an allowlist, cwd = the track worktree. `neige_git` drops the GH tokens, and https authentication goes through the user's global helper | `forge_action_adapter/mod.rs:53-67`, `:359-375`; `forge_git.rs:17-18`; S3 H15 | read |
| K9 | git 2.39.5, `push --force-with-lease=<ref>:<expect>`: an empty `<expect>` creates the ref only when it is absent. A matching `<expect>` forces. Any mismatch is rejected as `stale info`, even a fast-forward. An already up-to-date push succeeds. A push to a URL updates no `refs/remotes/*`, so there is no default lease | bare-repo experiment (scratchpad) | command |
| K10 | Task `base`: listed in `TASK_FIELDS`; valid only with `access: "read_only"` and as 40 hex characters. Prepare reads it into `ReaderFacts` for read-only attempts only, and it is rendered as one prompt line. The lease base is HEAD whatever `base` says, and a read-only lease records no base, so it has no delivery | `report_blocks/kinds.rs:175`; `task_execution.rs:103-123`; `0131…sql`; `worker.rs:141-147`, `:195-206`, `:11-12`; `task_prompt.rs:32-36` | read |
| K11 | The kernel fetch `refresh_upstream(repo_root)` is bounded at 20 s, single-flight, fail-soft, and backs off 60 s after a failure. `last_known_upstream` prefers its receipt (`KernelFetch`), else the human's tracking ref. Today it runs only when a track worktree is made. It must never run in `prepare_tx` (#1777) | `upstream_fetch.rs:71-75`, `:228`; `upstream.rs:199-296`; `track_worktree.rs:68` | read |
| K12 | `drive_spawn` submits the worker op after the claim tx, outside any tx. The payload is a pure function of the task row and leaves out newer columns | `scheduler/mod.rs:1103-1125`, `:142-160` | read |
| K13 | `prepare_tx` already runs git inside the op tx: the last-chance materialize and the clean-tree check | `worker.rs:178-192` | read |
| K14 | The lease CHECK accepts `base_source = 'upstream'` | `calm-truth/migrations/0115_workspace_lease_upstream_base.sql:47`; `workspace_lease/base.rs:40-58` | read |
| K15 | With a read-only gitdir, `git diff A B \| git apply --reject` works. It writes only the worktree, and a conflict becomes a `.rej` file | `chmod -R a-w .git` experiment (scratchpad) | command |
| K16 | Merging is gated per head: hold-for-ratify names a `head_sha`, and "a new head needs review again" | `templates/builtin/issue-development.md:130-135` | read |
| K17 | The ownership audit throws on any merge commit in `base..head` | `fe/tools/ownership/validator.ts:186-191` | read |
| K18 | A track's candidate refs are deleted only when the track is deleted | `git_candidate/refs.rs:1-20` | read |
| K19 | Prepare checks cleanliness and samples HEAD but not its branch; the branch is checked only at spawn | `worker.rs:191-195`; `base.rs:474-481` | read |
| K20 | Any push is a compare-and-swap against the value its own connection advertised: a writer landing after that, even under `--force`, gets `remote rejected`. The `ls-remote` → `push` window is two connections, which only the explicit lease covers | pre-push-hook experiment (scratchpad) | command |

## 3. Decisions

- **D1 Publish may replace its own branch.** The script reads the remote head first and leases
  against it. `$7` lists the commits of this track's candidates, from the read D3 of S3 already
  does (K4):
  ```
  cur=$(neige_git ls-remote "$3" "refs/heads/$2") || exit $?; cur=${cur%%[[:space:]]*}
  [ -z "$cur" ] || [ "$cur" = "$1" ] || case " $7 " in *" $cur "*) ;; *) exit 22;; esac
  neige_git push --porcelain --force-with-lease="refs/heads/$2:$cur" "$3" "$1:refs/heads/$2" >&2 || exit $?
  ```
  - **What the lease admits:** an absent branch (creation only) or a commit an attempt of this
    track made. Exit 22 comes before any push or `gh`. A writer landing between `ls-remote` and
    the push fails the lease (`stale info`, K9); one landing after the push's own advertisement is
    rejected by the remote's compare-and-swap (K20).
  - **Why not "the head the kernel last published":** nothing records a push that landed under a
    failed op (K6, K7). After a `gh` failure or exit 21, a lease taken from `forge.pr.opened`
    would reject every later publish of the track, and no tool could clear it. The candidate set
    already means "commits the kernel made for this track" and needs no new state.
  - **What does not change:** the probe and the output probe, so the semantic payload hash is the
    same and no persisted op conflicts (S3 H7). A fast-forward over this track's own head behaves
    as before; one over a foreign head is now refused too (K9).
- **D2 Findable failure (all forge actions).** After release, the not-landed reason keeps the
  action's status: `forge action exited with code <n>; probe reports not landed`. The
  dead-process text stays. The tool prompt names 22 ("the branch holds a commit no attempt of
  this track made: ask the user") and 21 ("the PR head did not move to the commit").
- **D3 No ratify for the replacing push.** The authority boundary: the push moves only
  `refs/heads/neige/track-<id>`, whose name and destination the kernel derives (S3 D2); the new
  head is a done candidate (S3 D3); the replaced head is absent or this track's own (D1), so no
  one else's commit is overwritten, and replaced commits stay in the repository (K18). What
  reaches the base branch is still gated at merge, by `head_sha` (K16). The #2042 ratify existed
  because the Planner pushed by hand, outside these checks.
- **D4 The task field `start`: `"checkout"` (default) | `"upstream"`.**
  - Validation: `"upstream"` requires `kind` codex or claude, `access: "read_write"` and
    the in-track `spawn` (`TASK_IN_TRACK_ROUTE`). Each error lists the valid choices.
  - Migration `0136_task_start.sql`:
    `ALTER TABLE tasks ADD COLUMN start TEXT NOT NULL DEFAULT 'checkout' CHECK (start IN ('checkout','upstream'))`.
  - The field is handled like `access`: it is frozen while the task is pending, kept out of the
    root hash and the drift fields (`task_context.rs:43-55`), and kept out of the worker payload
    (K12).
  - *Why a new field and not `base`* (the open question in the issue): `base` exists only on
    read-only tasks, holds a review's comparison commit, and never reaches the lease, the
    delivery or the candidate (K10). It would also need a commit id, which the Planner cannot
    learn fresh because it cannot fetch (K1).
- **D5 Fetch.** For a `start: "upstream"` task, `drive_spawn` awaits
  `refresh_upstream(repo_root)` before it submits (K11, K12). This is outside every
  transaction. Within one boot a re-drive fetches again (single-flight absorbs it). Receipts are
  in memory, and after a restart the boot drives pending ops through the driver, not
  `drive_spawn`: prepare then finds no `KernelFetch` receipt and refuses (fails closed).
- **D6 Prepare, after the clean-tree check** (`prepare_worker_lease_with_tx`; refusals take the
  `spawn-failed: refused: …` wire):
  0. `H` = HEAD, which must be on `refs/heads/neige/track-<id>` (`worktree_head_ref`, K19), else
     `track-worktree-unavailable: the checkout is on <ref>, not neige/track-<id>`. Nothing is
     reset off the track branch; the spawn check stays.
  1. `T` = the commit of the track's newest candidate whose attempt is done. Publish and prepare
     share this query (it moves out of `publish.rs`). With none, prepare refuses
     `track-nothing-to-replay: no attempt of this track is done; declare the task without start`.
  2. `U` = `last_known_upstream(repo_root)`, which must come from `KernelFetch`. Otherwise
     prepare refuses `track-upstream-unavailable: <name> <source> <sha>`. The same word covers
     a managed track and a checkout with no upstream.
  2a. The refusal of step 2, and the spawn failure of an attempt whose prepare committed
     (the scheduler's failed-op arm, `scheduler/mod.rs:1406-1411`, reading the `catch_up` record
     prepare writes into its output as `record_declared_head` does), ends with: "HEAD is at
     `<H>` (upstream `<U>` when reset); the track's work is in `<T>`; declare another
     `start:"upstream"` task".
  3. `M` = `merge-base T U`.
  4. `reset --keep U` through `isolated_git_command`, bounded at 20 s like the clean check
     (K13). Git runs the repository's filters without credentials.
  5. The lease base is `U`, with `base_source = 'upstream'` (K14).
  6. `CatchUpFacts {upstream, U, M, T, H}` are stored on the plan, as `ReaderFacts` are.

  `T` comes from the database, not HEAD. A re-drive in the same boot, a rolled-back prepare, a
  spawn failure after the reset, or a failed earlier catch-up all replay the same `T` again.
- **D7 The worker prompt** (`task_prompt.rs`): "This task starts from the upstream `<name>` at
  `<U>`, fetched by the kernel. Carry the track's work over: replay `git diff <M> <T>` here and
  resolve every conflict: `git apply --reject`, then `git merge-file` (versions from `git show`)
  for each rejected file. `git apply -3`, cherry-pick and merge need a writable gitdir. Leave no
  `.rej` files. Do not commit." When `H ≠ T`, add: "The checkout was at `<H>`; commits after
  `<T>` are not replayed." K15 shows that a read-only gitdir allows it.
- **D8 Delivery, candidate, gate and verdict are unchanged.** When the worker does not commit and
  the replay is non-empty, the delivery's commit `C'` has one parent, `U` (K3), and `U..C'`
  passes the merge-commit check (K17). A claude worker is not prevented from committing (K1);
  a merge it made is caught by that check.
- **D9 Prompts.** `planner.md:27` gains: "To bring the track onto the latest upstream, declare a
  codex or claude task with `start: "upstream"` and a gate. The kernel fetches and starts it at
  the upstream, and its worker replays your last done commit. Then publish again: publish
  replaces the track's own branch. After a failed catch-up, continue with an ordinary task only
  when its worker left partial work; if it never ran (the branch is at the upstream), declare
  another `start: "upstream"` task." The publish tool prompt replaces "(never forced)" with
  "replacing the branch only when it is absent or at a commit an attempt of this track made".
  `contracts.rs` describes `start`.

## 4. Oracle trace (Planner action → kernel effect → observable)

| # | Planner | Kernel | Observable |
|---|---|---|---|
| 1 | Sees the PR conflicting, or `candidate.upstream` behind | — | `neige.plan.list` `candidate.upstream {sha, behind}` (`git_candidate/staleness.rs`) |
| 2 | Upserts `{key:"catch-up-1", kind:"codex", start:"upstream", goal, gate, ready:true}` | Validates D4 and writes `tasks.start` | `plan.updated`; the task is pending |
| 3 | — | Claims when the track is idle (S2 D5); `drive_spawn` runs D5 | `task.dispatched` |
| 4 | — | Runs D6: `T`, `U`, `M`, `reset --keep U`, and a lease based on `U`, with the D7 prompt | `workspace.leased`; the worker card prompt names `U`, `M` and `T`. A refusal gives `task.failed spawn-failed: refused: track-…` and leaves HEAD where it was |
| 5 | — | The worker applies the diff, resolves, and reports | `task.completed` |
| 6 | — | Releases and delivers: commits `C'` (parent `U`), then runs the gate | `task.git_delivery_settled`; candidate `C'`; the task is `done` |
| 7 | `neige.track.publish {key, title, body}` | S3 D3 passes (`C'` is done). D1 reads remote `P` (the old candidate, ours) and force-with-lease pushes `C'`. The open PR on the same branch is reused, and its head becomes `C'` | Result `{pr_number, head_sha: C'}`; `forge.pr.opened{head_sha: C'}`. On GitHub the same PR shows a force-push, one commit, and a new `synchronize` CI run |
| 8 | Reviews the new head (K16), then merges under ratify with `expected_head_sha = C'` | As today | `ratify.*`, `forge.pr.merged` |
| 5′ | (the worker fails) | The delivery commits the partial work `F` as `failed` | `task.failed`; publish gives `publish-candidate-not-done`. Next, an ordinary task may continue from `F`, or another `start:"upstream"` task replays `T` |
| 4′ | (spawn fails after the reset) | Compensation delivers a `failed` candidate pinned at `U` (`codex_adapter/mod.rs:1063`) | `task.failed spawn-failed: … HEAD is at upstream <U>; the track's work is in <T>; declare another start:"upstream" task` (D6 2a). An ordinary task here would build on `U` without the track's work |
| 7′ | (someone pushed to the branch) | The script exits 22 before the push | `publish-failed: … exited with code 22; probe reports not landed`; the Planner asks the user |

**The candidate rule ("publish-not-a-candidate").** The catch-up commit is a kernel delivery,
so it is the catch-up attempt's candidate (step 6) and the #2042 rebind task disappears. Until
the catch-up is done, the tip is a failed or unsettled candidate, and publish refuses it.

## 5. Rejected alternatives

- Start in `base`: read-only only, another meaning, and a sha the Planner cannot fetch (K10, K1).
- Lease = the last `forge.pr.opened` head: a failed op leaves the remote ahead of it (K6, K7).
- A merge commit from main: the audit rejects it (K17); a writable gitdir would let a worker
  rewrite any ref. A kernel rebase with no worker: conflicts need one.
- Merge on `T` and re-parent onto `U` in the delivery: changes the delivery script and the base.
- A separate "move the branch" tool: a window in which the branch has lost its work and no task
  holds it. A ratify per replacing push: merge is already gated by head (D3).

## 6. Hazards

| Hazard | Introduced? | Mechanism |
|---|---|---|
| A replacing push discards a commit someone else pushed | yes | D1 lease (ownership check plus atomic CAS) |
| A refused replace reads as an opaque failure | yes (exit 22) | D2 |
| The catch-up resets the branch away from HEAD; later failed or Planner commits leave it | yes | `T` from the database; the prompt names `H`; kernel commits stay in candidate refs (K18) |
| A catch-up onto a stale upstream does not resolve the conflict | yes | D5 fetch plus the `KernelFetch`-only refusal |
| A spawn failure after the reset leaves the branch at `U` | yes | Replaying `T` again is safe (D6); publish refuses `U` |
| An ordinary task after a never-ran catch-up builds on `U` without the track's work, and D1 then replaces the PR with it | yes | rule + findable text (D6 2a, D9, trace 4′); backstop in KNOWN GAPS |
| The reset moves a branch other than the track's | yes | D6 step 0; spawn check kept (K19) |
| A restart between fetch and prepare refuses the catch-up | yes | fails closed with `track-upstream-unavailable` (D5) |
| Git holds the op tx during the reset | yes (same class as K13) | bounded at 20 s |
| Stderr of a failed forge action is dropped | no (S3 §7) | none (D2 surfaces the exit code only) |
| The PR base is the checkout's upstream at publish time | no (S3 gap) | none |
| The tip moves between the S3 D3 check and the push | no (S3 gap) | none |
| `candidate.upstream` lags until a fetch | no (S2 as-built) | D5 refreshes it as a side effect |
| A Planner pushes by hand from a terminal | no (S3 gap) | none |
| Nothing wakes the Planner when the PR conflicts or CI does not start | no (#2058 S2) | none here |

## 7. Slices (both L2; they land together, because slice 2's commit is the one slice 1 replaces)

**Slice 1: publish replaces its own branch** (`forge_git.rs`, `publish.rs`, the forge adapter
reason, and the tool prompt; about 25 production lines). Tests are in `tests/cases/track_publish.rs`
and use the existing fixtures (bare origin, `gh` shim):
- **R1** `publish_replaces_its_own_branch_with_a_non_descendant_candidate`. Done candidate `P` is
  published. The test resets the worktree to origin main, standing in for a catch-up, and a done
  attempt makes `C'`. Publish succeeds. The remote branch and the shim PR head are `C'`, and
  there is still one PR.
- **R2** `a_non_fast_forward_publish_fails_and_leaves_the_remote` is the existing test, renamed
  `…foreign_commit…`. It also asserts the message contains `exited with code 22`. The remote is
  unchanged, and `gh` is not invoked.
- **R3** The first publish creates the branch (existing P1 stays green).
- **R4** `a_writer_between_the_lease_read_and_the_push_is_never_overwritten`. A `git` shim on PATH
  next to the `gh` shim passes everything through; on `ls-remote` while a trigger file exists, it
  first lets the real `ls-remote` answer (`P`, ours), then pushes a foreign commit `X` to the
  origin from another clone and deletes the trigger. Publish fails, and the remote stays `X`. (A
  pre-push hook cannot stand in: it runs after the push's advertisement, so even `--force` is
  rejected there, K20.)

Predicted red sets: MA1 (push without `--force-with-lease`) → {R1, C1} (C1 publishes a
replacement); MA2 (the ownership `case` always matches) → {R2}; MA3 (D2 reverted to the fixed
text) → {R2}; MA4 (the lease replaced by unconditional `--force`) → {R4}. The tests that pin the old
text use the dead-process path or `contains` (`forge_action_adapter.rs:1284,2954,3102`,
`forge_merge_crash_reboot.rs:406`, `git_forge_track_worktree.rs:273`) and stay green.

**Slice 2: the `start: "upstream"` task** (migration, `calm-types` validation, the
`contracts.rs` schema, `calm-truth` task row and projection, `scheduler::drive_spawn`,
`worker.rs` D6, `task_prompt.rs`, the shared candidate query, `planner.md`, the FE strict
task schema in `fe/core/domain/report.ts`, `docs/using-neige-calm.md`; about 230 production
lines). The tests go in a new `tests/cases/track_catch_up.rs`, with the fixtures of
`track_worker_cwd.rs`: a test-played worker, real git, and the production
`ensure_track_worktree`. The upstream moves `O0 → O1` after the worktree is made. O1 edits line
1 and the track's done `T` edits line 2 of a 3-line `shared.txt`, a deliberate conflict: plain
`git apply --reject` rejects that hunk, and the test-played worker writes the resolved file and
removes the `.rej`:
- **C1** `a_catch_up_starts_at_the_fetched_upstream_and_delivers_one_linear_commit`. At spawn,
  HEAD = `O1`, and the prompt names `O1`, `M = O0` and `T`. The worker applies the diff. `C'` has
  exactly one parent, `O1`, and holds both edits. The lease has `base_source = 'upstream'`. Then
  publish replaces the remote branch, and `git log --merges O1..C'` is empty.
- **C2** `a_second_catch_up_replays_the_last_done_commit`. The first catch-up fails with a partial
  `F` on `O1`. The second one's prompt names `T`, not `F`, and it starts at `O1`.
- **C3** `a_catch_up_whose_fetch_fails_is_refused`: the origin is unreachable after the
  worktree is made. Result: `track-upstream-unavailable`, no lease, and HEAD unchanged.
- **C4** `a_catch_up_without_a_done_attempt_is_refused` (`track-nothing-to-replay`, HEAD
  unchanged). A managed track gives `track-upstream-unavailable`.
- **C6** `a_catch_up_on_a_foreign_branch_is_refused_and_moves_nothing`: the idle worktree is
  switched to a clean human branch with an unpublished commit. Result: `track-worktree-unavailable`
  naming that branch; the human branch and `neige/track-<id>` are both unchanged.
- **C7** `a_catch_up_whose_spawn_fails_names_the_upstream_and_the_work`
  (`fail_next_thread_start_for_test`, `shared_codex_appserver.rs:3736`): the task fails with
  `spawn-failed` text containing "HEAD is at upstream `<O1>`; the track's work is in `<T>`;
  declare another start:"upstream" task".
- **C5** (`kinds_tests`) `start` with terminal, `read_only`, the child-track `spawn` or an
  unknown value is rejected, and the error lists the choices.

Predicted red sets: MB1 (prepare skips the reset) → {C1, C2}; MB2 (`drive_spawn` skips the
fetch; the create-time receipt still says `O0`) → {C1, C2, C3}; MB3 (`T` = HEAD) → {C2}; MB4
(any upstream source is accepted) → {C3}; MB5 (D6 step 0 dropped) → {C6}; MB6 (the scheduler's
failed-op arm omits the D6 2a sentence) → {C7}.

**Gates.** `head_schema_fixture.rs` (list 0136); the `tasks` snapshot in
`track_projection_policy_patch.rs`; `task_context_migration_tests.rs`;
`tests/goldens/mcp_tool_registry.json` (the `contracts.rs` schema, the publish description);
`issue_development_planner_prompt.txt` (`REGEN_PLANNER_PROMPT_GOLDEN=1`); FE `report.test.ts`
and `(cd fe && npm ci && npm run lint && npm run build && npm test)`;
`scripts/local-ratchet-gates.sh` (prose and terminology). Not triggered: the event version
lockstep (no event changes), `deferred_write_tx_invariant`, `scripts/ci/ratchets/*`, OpenAPI.

## 8. KNOWN GAPS

- Review comments on replaced commits become "outdated" on GitHub.
- A replay that changes nothing pins an upstream commit as the candidate. Publishing it empties
  the PR, and GitHub may mark it merged.
- A replay leaves `.rej` files if the worker does not remove them; the gate is the check.
- If the user switches the main checkout's branch between a catch-up and a publish, the replay
  base and the PR base differ.
- A Planner `git.commit` made after `T` stays only in the branch reflog after a catch-up.
- A catch-up op that goes Stuck after the reset leaves HEAD at `U`; the next catch-up replays `T`.
- If the Planner ignores the D9 rule and runs an ordinary task from `U`, the merge review of its
  `head_sha` (K16) is the backstop; the track's work stays in candidate refs (K18).
- A restart after the fetch and before prepare refuses the catch-up (D5); declare it again.
- A `reset --keep` killed at 20 s can leave `index.lock`; the next clean check reports
  `track-worktree-unavailable` until it is removed.

## 9. 4140 evidence (orchestrator, 2026-10-04)

- All 7 `forge.pr.opened` heads are candidates of their own track (`EXISTS = 1` for every row,
  including #2042's final head `f5d4bccbc`), so D1 admits every existing remote branch.
- Publish ops (`idempotency_key LIKE '%track.publish:%'`; keys carry the prefix
  `dev.neige.git-forge:<track>:<card>:`): 7, all succeeded, none failed after landing.
- `tasks`: done 118, failed 23, canceled 8. `DEFAULT 'checkout'` keeps today's behaviour.
- git 2.39.5 on the host, the version K9 and K20 were run with.

## 10. Decisions (orchestrator, 2026-10-04; all four recommendations accepted)

1. No ratify for a replacing publish (D3): merge is the gate, and the lease protects every
   other writer.
2. A catch-up replays the newest done commit; later failed or Planner commits leave the branch
   (D6). They cannot be published anyway (S3 D3), and the prompt names them.
3. A failed kernel fetch refuses the catch-up (D6.2) instead of replaying onto a stale upstream.
4. The surface is `start: "checkout" | "upstream"`, modelled on `access`.

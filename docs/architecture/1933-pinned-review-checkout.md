# #1933 — pinned review checkouts

**Owner rules.**
1. Cover the observed pain with the fewest mechanisms. Hypotheticals are one-line KNOWN GAPS.
2. Compatibility is the 4140 database only.
3. A task option applies to every same-kind path, and errors list the valid choices.
4. The kernel owns generic lifecycle and authorization; the dev template owns review semantics.
   Nothing infers "review" from goal text.

**Outcome.** A codex task block gains a typed `workspace` field. `track` (the default) behaves
as today. `pinned {head, base?}` runs against a private, detached `git worktree` of the track's
repository at exactly `head`. The worker can read it but not write it.

The kernel allocates its work, artifact and build directories, renders the repository remote
in the worker header, and runs its gate in the checkout. Pinned trees are removed when the track is closed and quiet, or when it is deleted. There is no
migration, no new column and no frontend change.

**Owner decisions (2026-10-01).** D1: no `track_idle` bypass. D2: Codex only. D3: artifacts
live until track delete. D5: acceptance on a dev kernel, both channels Codex. D6: a migration
number, if ever needed, is assigned at merge. D7: subtract persistence. D8: fix the stale
`event.rs:602` comment. Round 1: no per-attempt reclaim; trees are reclaimed at close or delete.

## 1. Problem and evidence

PR #1927 (track `e0646de4…`, frozen head `d448361f…`, 4140 at `a4a16f14`, 2026-10-01). Two Codex
reviewers each hand-ran `git clone --shared --no-checkout` and a detached checkout. Their first
`gh pr view` failed because the clone's `origin` was a local path, so both added
`--repo keanji-x/neige-calm`. The Planner hand-assembled base/head, the checkout, the report dir,
the target dir and the gate cwd in goal text. Nothing reclaimed `/tmp/neige-1545-pr1927-{a,b}-r1`.

## 2. Facts (origin/main `bcd4dec59`)

| # | Fact | Where |
|---|---|---|
| F1 | Tasks are declared as report `task` blocks. `calm.plan.upsert` is a retired shim that writes nothing | `track_report_blocks/contracts.rs:258-357`; `tools/plan.rs:398-412` |
| F2 | `TASK_FIELDS` and `validate_task` reject unknown keys, and a test pins the schema to `TASK_FIELDS` | `calm-types/src/report_blocks/kinds.rs:152-173`; `contracts.rs:653-664` |
| F3 | The root hash covers only the keys present in a payload. Fields that are hashed but not stored (`refs`, `no_gate_reason`) are an explicit test exclusion | `calm-types/src/task_recovery.rs:15-48`; `task_context.rs:1383-1410` |
| F4 | The claim freezes the root ref (block id plus root hash) in `tasks.claim_context_json`. In the claim tx it re-reads the root block from the report card and treats any hash change as a lost race | `scheduler/mod.rs:941-1050` (fence `:1029-1048`); `task_context.rs:292-330`, `:1159-1176` |
| F5 | Agent tasks may not set `gate.cwd`. Their gate cwd is the worker card's latest `workspace_leases.path`, in any lease state | `calm-types/src/report_blocks/tasks.rs:283-287`, `:579-595`; `task_verify_adapter/mod.rs:441-462` |
| F6 | No CLI command declares tasks | `mcp_server/cli/commands.rs:81-247` |
| F7 | `track_idle` (read by `compute_ready` and the claim) is false in three cases: a codex/claude task is in flight; a lease is held whose owner op is not `stuck`; or a delivery is unsettled | `calm-truth/src/db/sqlite/track_idle.rs:20-57`; `scheduler/mod.rs:151`, `:1079` |
| F8 | Prepare calls `prepare_worker_lease_tx`, then `acquire_workspace_lease_tx`. That is the only lease INSERT. `lease_owner` is the op id, and `delivery_policy = base.map(Kernel)` | `codex_adapter/mod.rs:762-866`, `:814`; `workspace_lease/worker.rs:92-125`; `workspace_lease/mod.rs:168-205` |
| F9 | The prepare TxOutput is committed in the same tx as the lease INSERT, and `operations` rows are never deleted. Readers already join on it: the terminal gate's `$.data.cwd` (terminal tasks only) and terminal disposal | `operation/repo_sqlite.rs:273-320`; `grep -rn "DELETE FROM operations" crates` finds 0; `task_verify_adapter/mod.rs:463-477`; `terminal_disposal.rs:31` |
| F10 | Codex checks the checkout in `app_server_interact`. Recovery from `SpawnStarted` calls `spawn_side_effect` directly, which launches without that check | `codex_adapter/mod.rs:868-886`, `:888-951`, `:1555-1565`; `operation/driver.rs:540` |
| F11 | Codex workers run `workspace-write` with approval `never`, and cwd is TxOutput `cwd`. Resume sends `threadId` plus shell-env config only. A thread whose `thread_id` is persisted is reused without `thread/start` | `shared_codex_appserver.rs:1225-1246`, `:3472-3500`; `codex_adapter/mod.rs:1164-1166` |
| F12 | Claude workers have no permission boundary | `claude_adapter/mod.rs:356-375`; `routes/claude_cards.rs:313-341` |
| F13 | The first prompt holds `Goal/Context/Acceptance` and the completion-ID footer. The system prompt says "the platform commits after you report" | `codex_adapter/mod.rs:1485-1525`; `prompts/worker/head-mcp.md:6` |
| F14 | Only `delivery_policy='kernel'` inserts a delivery. A NULL-policy lease is gate `Unbound{LegacyLease}`, whose wire doc is "`delivery_policy IS NULL`". An unbound gate passes its verdict through even when `stop_group` fails | `workspace_lease/release.rs:126-150`; `task_verify_adapter/target.rs:199-227`, `:657-700`; `calm-types/src/verify_target.rs:39-50` |
| F15 | The frontend closed enum lives in the frozen `fe/core/api` | `fe/core/api/schemas.ts:829-843`; `fe/module-file-inventory.yaml:44` |
| F16 | Completion releases the lease but does not end the session: a completed Codex turn stays alive. Only the sweeper's closed-track arm ends sessions, and only those whose task is not in flight | `decision_sink.rs:148`, `:195`; `calm-provider/src/provider/codex.rs:220`; `terminal_sweeper.rs:69-125` |
| F17 | Closing stops scheduling only; in-flight tasks and gates continue. Delete stops no gate (`stop_group` has no caller outside `task_verify_adapter`). After commit, delete removes the track worktree | `1876-track-open-closed.md` D2; `routes/tracks.rs:2743-2810`, `:3093-3160`; `workspace_lease/mod.rs:247-363` |
| F18 | Boot reclaim only flips rows, and only for an older-boot lease whose owner op is not recoverable. Recoverable owners are re-driven. #1830 S2 deleted the #1815 disk reclaim | `release.rs:233-262`; `operation/driver.rs:351`, `:1047` |
| F19 | Worker forge actions run in the worker's lease path | `mcp_server/transport.rs:1029-1090` |
| F20 | Dev publish resolves the repository remote as `track_worktree_target(..).repo_root` (the main root), then `head_upstream(main root)`. Track branches have no upstream, so `head_upstream` of the track worktree is `None` | `builtin_plugins/dev/publish.rs:163-200`; `track_worktree.rs:92`; `upstream.rs:84-96` |
| F21 | `git worktree add` runs repository hooks under the allowlisted environment, and a test pins this for the track worktree | `tests/cases/lease_git_env.rs:1-40` |
| F22 | isolated-codex-v1 was deleted in #1893 S4, so no second lease kind remains | `71112fccd`; `calm-types/src/event.rs:602` (stale) |
| F23 | Review semantics live in the dev template, which says nothing about checkouts. The typed data root is `Config.data_dir` | `templates/builtin/issue-development.md:73`; `config.rs:26`, `:174-182` |

**4140** (`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`, 2026-10-01): 118 tasks
(codex 66, claude 48, terminal 4), none non-terminal. 110 leases, all `released`; one already
has a base with NULL policy, so NULL policy is not a pinned marker. Every lease has its owner
operation (`left join operations … where o.id is null` returns 0). Of 429 task blocks in all
`track_vcs_objects` blobs, 0 have `workspace` or `context.neige_workspace`. Nothing changes
meaning.

## 3. Design

### 3.1 Contract (calm-types)

```json
"workspace": { "kind": "track" }
"workspace": { "kind": "pinned", "head": "<full sha>", "base": "<full sha>" }
```

- `TaskWorkspace` is tagged by `kind` (`deny_unknown_fields`). If omitted it means `track`.
- It is registered on `TASK_FIELDS`, in the `contracts.rs` properties and tombstone list, and in
  `TASK_ROOT_HASH_FIELDS`. It is not a drift field, because it is not stored (F3).
- `head` is required and `base` is optional. Both are full commit ids (40 or 64 lowercase hex).
- `pinned` is accepted only for `kind: codex` on the track's own route.
- Errors (`invalid_declaration`) list the valid choices, for example:
  - `workspace.kind must be one of: track, pinned`
  - `workspace pinned requires kind: codex (got claude)`
- #1921 adds `track_read` here instead of `context.neige_workspace`.
- There is no CLI mirror (F6).

### 3.2 Persistence: none (D7)

- **Task workspace, read at prepare.** Prepare reads the root block that the claim froze (F4):
  `frozen_root_block_tx` checks it against the hash in `claim_context_json`, then reads
  `workspace`. That function is the claim fence's inline read, moved so both callers share it.
  - Because `workspace` is hashed, an equal hash proves the value was admitted.
  - A changed hash refuses with `declaration-changed-in-flight`.
  - `build_worker_payload` is unchanged.
- **Lease kind.** TxOutput carries `workspace`, `head`, `base` and `checkout`, and is committed
  with the lease row (F9). The one reader is `lease_workspace_tx`: it reads
  `json_extract(o.tx_output_json,'$.data.workspace')` through `lease_owner`. Its callers are
  the forge fence now and #1917 later.
- **Delivery policy.** `acquire_workspace_lease_at_path_tx` takes the policy from the plan
  instead of `base.map(Kernel)` (`mod.rs:202`): track gives `Kernel`, pinned gives NULL.

### 3.3 Layout and lifecycle (reuses `workspace_leases`)

`<data_dir>/pinned/<track_id>/` (F23) contains:

| Path | Lifetime |
|---|---|
| `checkouts/<card_id>/` | the attempt's detached worktree, which the worker cannot write; removed at close or delete |
| `work/` | the worker cwd, writable, shared by the track's pinned attempts |
| `work/target/` | the build dir, one per track; removed at close or delete |
| `work/artifacts/<card_id>/` | reports; removed at delete (D3) |

Sharing `work/` and the target is safe only because pinned tasks serialize under `track_idle`
(D1, F7). #1917 must revisit both before it admits pinned tasks concurrently. This sharing caps
disk use at one target per track.

1. **Prepare.**
   - It reads the repository root from the track row, as `prepare_worker_lease_tx` does:
     `track_worktree_target(..).repo_root` for an attached track, `workspace_path` for a managed
     one.
   - It probes `git cat-file -e <head>^{commit}` (and `base`) through the bounded `run_git`.
   - It skips the clean-tree check and supersede.
   - It plans `checkout: Track{branch} | Pinned{head, base}`.
   - The lease row gets: `path` = `checkouts/<card>`;
     `canonical_path` = `canonicalize(data_dir)` joined with the validated segments (nothing
     exists yet); `git_common_dir` = `lease_git_common_dir(repo_root)`;
     `base_source='commit'`; `base_sha = head`; policy NULL.
   - TxOutput `cwd` is `work/`.
   - Nothing touches the filesystem before the commit.
2. **Ensure, at the single launch point (fixes F10).**
   - The checkout check moves from `app_server_interact` into `spawn_side_effect`. It runs after
     the "already exited" no-op and before `spawn_codex_worker_via_shared_daemon`. Every fresh
     launch and every `SpawnStarted` recovery passes through it, for track tasks too.
   - Track tasks keep today's `verify_worktree_base`.
   - For pinned, an existing checkout is kept only if it passes every check: it is registered
     at the path; HEAD == `head` and detached; its realpath equals `canonical_path`; and
     `git status --porcelain` is empty.
   - Otherwise (a partial add, or a retry): `remove_workspace_worktree` (guards kept; the target
     gains `head: Branch | Detached`), then `worktree add --detach` through
     `isolated_git_command`, then the check again.
   - A blind recreate would pull the checkout from under a worker already running in the daemon
     (F11, `:1164`), so the check comes first.
   - Ensure also creates `work/artifacts/<card>` and `work/target`.
3. **Read-only on start and on resume, with no sandbox code.**
   - Codex `workspace-write` with cwd `work/` (F11) leaves `checkouts/` and the common `.git`
     readable but not writable, and keeps network access.
   - Resume keeps the thread's cwd, as track workers already require.
   - A forge action from a pinned worker is refused before `resolve_forge_cwd` (F19) with
     `refused: pinned-checkout-read-only`.
4. **Release and gate.**
   - The existing release points flip the row, and NULL policy means no delivery (F14).
   - The gate cwd is the lease path, which is the checkout (F5).
   - The gate target is `Unbound{LegacyLease}`, unchanged. Its wire meaning is already
     "`delivery_policy IS NULL`" (F14), and only its Rust doc comment is reworded to name
     pinned. A new variant would touch the frozen `fe/core/api/schemas.ts` (F15) and need a
     WEB_COMPAT bump. Reusing it changes neither WEB_COMPAT nor SYNC_EVENT_VERSION, and the
     frontend is unchanged.
5. **Reclaim at close (the terminal sweeper's closed-track arm, F16).** After the arm ends
   sessions, a new step visits each closed track that has a `pinned/<track>/` directory. Today
   no point guarantees quiescence (F16, F17), so the step requires all three of:
   - (i) no current task of the track is `dispatched`, `running` or `verifying`;
   - (ii) no session of the track is `running` with a live terminal (the arm's own set);
   - (iii) for each pinned attempt, `stop_group` (F14) re-run on its last gate-op artifacts
     returns `Ok`.

   It then removes `checkouts/*` (with the guards, then `worktree prune`) and `work/target`.
   - On `Err`, it keeps the trees, logs, and retries next tick.
   - A reopen re-arms admission. The step re-reads `closed_at` in an IMMEDIATE transaction
     right before removal.
6. **Reclaim at delete.** The post-commit sweep (F17) also removes `pinned/<track>/` whole
   (artifacts included), using the `repo_root` it already derives for the track worktree. That
   is the same contract as the track worktree.
7. **Admission is unchanged (D1).**

### 3.4 Worker header and template

`render_task_worker_prompt` (F13) gains a `Workspace:` section, for pinned tasks only:
- `checkout`: read-only and never committed. This overrides the system prompt's "the platform
  commits after you report".
- `head`, and `base` (or "not declared").
- `remote`: the publish resolver, extracted into one `track_repo_remote(track)` that both
  callers use (F20), or `none (no upstream remote for <repo>)`.
- "Run git and gh inside the checkout (`cd <checkout>`); gh resolves the repository from its
  `origin`. Pass the PR number explicitly."
- `work` (your cwd), `artifacts/<card>` (write reports here and name them in
  `calm.task.complete`), and `target` (point build output here).
- "The gate runs in the checkout."

`templates/builtin/issue-development.md` "Working method" (`:73`) gains one paragraph. Each
review channel is a `kind: codex` task with
`workspace: {kind: pinned, head: <PR head>, base: <PR base>}`. Its goal names only the channel
role and the PR number, with no clone, `--repo`, directory or target commands.

## 4. Failure and diagnostic matrix

| Case | Detected at | Outcome | Diagnostic |
|---|---|---|---|
| Bad shape: abbreviated head, unknown kind, claude, terminal, child route | `validate_task` | block invalid, never scheduled | `invalid_declaration` listing the valid choices |
| Root changed between claim and prepare | `frozen_root_block_tx` (prepare) | `spawn-failed`, no row | `refused: declaration-changed-in-flight: task block <id> changed after claim; declare again under a new key` |
| Unknown head or base | prepare | `spawn-failed`, no row, no directory | `refused: pinned-head-unknown: commit <sha> is not in <repo>; push or fetch it, then declare again under a new key` (`pinned-base-unknown` likewise) |
| Checkout HEAD moved, on a branch, dirty, or partial | ensure (`spawn_side_effect`) | removed and added again at `head` | on failure: `refused: pinned-checkout-unavailable: git worktree add failed in <repo>: <stderr>` |
| Kernel restart, owner op recoverable | driver re-drive (`driver.rs:1047`), then ensure on `SpawnStarted` | checkout verified or recreated, launch continues | as above |
| Older machine boot, owner op not recoverable | boot reclaim (F18) | attempt `spawn-failed: <BOOT_RECLAIM_REASON>`, row released with no delivery; trees wait for close | existing text |
| No repository remote | header | runs | `remote: none (no upstream remote for <repo>)` |
| Cancel or timeout | existing mark, kill, release | row released; trees wait for close | — |
| Track closed with a task in flight, a live session, or a gate group not proven stopped | close step terms (i) to (iii) | trees kept; retried every 30 s | `tracing::warn`: `pinned trees kept for track <id>: <term>` |
| Track deleted | delete post-commit sweep | `pinned/<track>/` removed, registrations pruned | — |
| Forge action from a pinned worker | `transport.rs`, before `resolve_forge_cwd` | refused | `refused: pinned-checkout-read-only: a pinned worker cannot run forge actions` |

Permission, candidate, session and merge fences are unchanged, and track tasks follow F8.

## 5. Composition

- **#1917.** Pinned keeps today's serialization (D1). Once #1917 decides admission by conflict,
  it can classify pinned leases through `lease_workspace_tx` as occupying no track checkout.
  Before admitting pinned tasks concurrently, it must give each attempt its own `work/` and
  target (§3.3).
- **#1921.** `track_read` becomes a third variant of this field.
  - If #1921 keeps its `access_mode` column, pinned rows write `read_only` and
    `lease_workspace_tx` reads that column.
  - `track_read` uses the Codex `read-only` sandbox, because its cwd is the shared checkout.
  - The forge refusal is shared.

## 6. Slice plan (one PR, about 800 lines: about 430 prod and 370 test)

The PR contains:
- the `calm-types` enum, validator, schema and root-hash registration;
- `frozen_root_block_tx`;
- `workspace_lease/pinned.rs`, a new file because `mod.rs` is 791 lines. It holds prepare,
  ensure, the close step and the delete step;
- the ensure call moved into `spawn_side_effect`;
- the policy parameter on `acquire_workspace_lease_at_path_tx`;
- `track_repo_remote`, extracted from `publish.rs`;
- the header and the forge fence;
- the template paragraph;
- the `LegacyLease` doc comment;
- `calm-types/src/event.rs:602`, reworded to "a worker attempt's lease of its checkout".

There is no migration, no `Task` change, no frontend change, no Claude change and no sandbox
change.

Must-red tests. Each names the production mutation that turns it red; tests marked * get
mutation verification.
- T1 `pinned_head_must_be_full_commit_id`: remove the full-hex check.
- T2 `pinned_refused_off_codex_lists_valid_choices`: remove the kind check.
- T3* `pinned_prepare_uses_claim_frozen_root`: skip the frozen-hash comparison.
- T4* `pinned_unknown_head_refused_before_any_row`: skip the `cat-file` probe.
- T5 `pinned_checkout_on_a_branch_is_replaced`: remove the detached check from ensure.
- T6 `pinned_worker_cwd_contract`: TxOutput `cwd` = checkout. This is a cwd contract only;
  write denial is acceptance (d).
- T7* `pinned_checkout_moved_is_replaced_before_launch`: skip the HEAD check.
- T8* `pinned_recovery_from_spawn_started_ensures_checkout`: leave ensure in
  `app_server_interact`. The test starts recovery at `SpawnStarted`.
- T9 `pinned_partial_checkout_recreated`: trust an existing directory without checking it.
- T10* `pinned_release_writes_no_delivery`: pass `Kernel` for pinned.
- T11 `pinned_gate_cwd_is_checkout`: lease path = `work/`.
- T12* `closed_track_reclaims_pinned_only_when_quiet`. It drives a real pinned completion on an
  open track, closes the track, then runs `terminal_sweeper::sweep`. There are three
  mutations, one per term: (i), (ii), and (iii) using a live gate descendant that carries the
  marker.
- T13 `track_delete_removes_pinned_tree_and_registration`: remove the delete arm.
- T14* `pinned_worker_forge_action_refused`: remove the fence.
- T15 `pinned_header_renders_shared_remote_and_checkout_cwd`: resolve through the track
  worktree's upstream, which yields `none`.
- T16 `pinned_worktree_hooks_see_only_the_allowlisted_environment`, in `lease_git_env.rs`, with
  its header updated: a plain `Command` for the add.
- The existing partition tests (F2, F3) also apply. `projection_drift_fields_equal_hashed_stored_fields`
  gains `workspace` in its not-stored set.

Gates:
- `scripts/local-ratchet-gates.sh`. Use the route constants.
- Targeted `cargo nextest` for `calm-types` and `calm-server`, then the whole `-p calm-server`
  run, then `scripts/local-rust-gates.sh --quick`.
- Golden: `tests/goldens/mcp_tool_registry.json`.
- Golden: `issue_development_planner_prompt.txt` (`REGEN_PLANNER_PROMPT_GOLDEN=1`).
- Not triggered: migrations, `head_schema_fixture.rs`, column snapshots, `fe/`, `gen:api`,
  `worker_prompt_*` goldens, event goldens, the `planner.md` caps.

## 7. Acceptance (issue item 4, D5)

Run a dev kernel built from the PR branch. It is not 4140, and 4140 is not restarted.
`data_dir` must be outside `/tmp` (KNOWN GAPS). A dev-template track reviews a real open PR of
this repository with two `kind: codex` pinned channels on the same head. Pass criteria:
- (a) The Planner's blocks hold no clone, `--repo`, directory or target commands.
- (b) Each worker runs `cd <checkout> && gh pr view <n>` without `--repo` successfully, and the
  header shows the remote.
- (c) Each gate log shows its own checkout as cwd.
- (d) A worker write into the checkout fails, both on start and after a daemon resume.
- (e) The track worktree's `git status` and the product diff are unchanged.
- (f) After the track is closed, `git worktree list` lacks both checkouts, `work/target` is
  gone, and the artifacts remain.
- (g) An unknown head ends with `refused: pinned-head-unknown`.

## 8. KNOWN GAPS

- Claude cannot use `pinned` (D2, F12). The follow-up is a Claude worker sandbox, then adding
  claude here.
- Pinned disk (checkouts and one target per track) is held until the track is closed or deleted.
- Delete removes pinned trees without a gate stop proof, which is the same contract as the
  track worktree (F17).
- A reopen that lands between the close step's re-read and its removal can lose a running
  build's target.
- Network access is on, so a pinned worker's shell `gh` can still merge or post a review.
- Codex `workspace-write` leaves `/tmp` and `$TMPDIR` writable, so a `data_dir` under `/tmp`
  would make the checkout writable.
- A head that exists only on a fork or an unfetched remote is refused; the kernel does not fetch.
- The repository's `AGENTS.md` is not auto-loaded, because the cwd is `work/`. The header names
  the file.
- Read-only access rests on Codex `workspace-write` semantics and on resume keeping the cwd.
  Acceptance (d) proves both.

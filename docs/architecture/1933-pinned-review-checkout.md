# #1933 — pinned review checkouts

**Owner rules.**
1. Cover the observed pain with the fewest mechanisms. Hypotheticals become one-line KNOWN GAPS.
2. Compatibility means the 4140 database only.
3. A task option applies to every same-kind path, and errors list the valid choices.
4. The kernel owns generic lifecycle and authorization; the dev template owns review semantics.
   Nothing infers "review" from goal text.

**Outcome.** A codex task block gains a typed `workspace` field.
- `track`, the default, behaves exactly as today.
- `pinned {head, base?}` runs against a private, detached `git worktree` of the track's
  repository at exactly `head`. The worker can read it but cannot write it.
- The kernel allocates the work, artifact and build directories, renders the repository remote
  in the worker header, and runs the gate in the checkout.
- Pinned trees stay until the track is deleted.
- There is no migration and no new column. The frontend reader and one wire enum value change.

**Owner decisions (2026-10-01/02).**

| ID | Decision |
|---|---|
| D1 | No `track_idle` bypass |
| D2 | Codex only |
| D3 | Artifacts stay until track delete |
| D5 | Acceptance runs on a dev kernel with two Codex channels |
| D6 | A migration number, if one is ever needed, is assigned at merge |
| D7 | Subtract persistence |
| D8 | Fix the stale `event.rs:602` comment |
| D9 | No close-time reclaim; resources go only at track delete |
| D10 | The frontend task reader gains the field |
| D11 | Pinned gets an accurate unbound reason |
| D12 | Track-task behavior is unchanged |
| D13 | Ensure runs after side-effect admission |

## 1. Problem and evidence

PR #1927 (track `e0646de4…`, frozen head `d448361f…`, 4140 at `a4a16f14`, 2026-10-01):
- Two Codex reviewers each ran `git clone --shared --no-checkout` and a detached checkout by
  hand.
- Their first `gh pr view` failed because the clone's `origin` was a local path, so both added
  `--repo keanji-x/neige-calm`.
- The Planner assembled base/head, checkout, report dir, target dir and gate cwd by hand in goal
  text.
- Nothing reclaimed `/tmp/neige-1545-pr1927-a-r1`: 47 MB of checkout and 408 MB of `target/`
  for one channel and one round (`du -sh`).

## 2. Facts (origin/main `bcd4dec59`)

| # | Fact | Where |
|---|---|---|
| F1 | Tasks are declared as report `task` blocks. `calm.plan.upsert` is a retired shim | `track_report_blocks/contracts.rs:258-357`; `tools/plan.rs:398-412` |
| F2 | `TASK_FIELDS` and `validate_task` reject unknown keys, and a test pins the schema to `TASK_FIELDS` | `calm-types/src/report_blocks/kinds.rs:152-173`; `contracts.rs:653-664` |
| F3 | The root hash covers only keys present in the payload. Fields that are hashed but not stored (`refs`, `no_gate_reason`) are an explicit test exclusion | `calm-types/src/task_recovery.rs:15-48`; `task_context.rs:1383-1410` |
| F4 | The claim freezes the root ref and its hash in `claim_context_json`, and re-reads the root block in its own transaction. Prepare refuses only once the monitor has marked the task `context_stale_at_ms` (`context-stale: …`) | `scheduler/mod.rs:1029-1048`; `task_context.rs:1159-1176`; `operation/mod.rs:85-102`; `codex_adapter/mod.rs:769` |
| F5 | Agent tasks may not set `gate.cwd`. Their gate cwd is the worker card's latest `workspace_leases.path` | `calm-types/src/report_blocks/tasks.rs:283-287`; `task_verify_adapter/mod.rs:441-462` |
| F6 | No CLI command declares tasks | `mcp_server/cli/commands.rs:81-247` |
| F7 | `track_idle` is false in three cases: a codex/claude task is in flight, a non-`stuck` owner holds a lease, or a delivery is unsettled | `calm-truth/src/db/sqlite/track_idle.rs:20-57` |
| F8 | Prepare runs `prepare_worker_lease_tx`, then `acquire_workspace_lease_tx`, which is the only lease INSERT. `lease_owner` is the op id, and the delivery policy is `base.map(Kernel)` | `workspace_lease/worker.rs:92-125`; `workspace_lease/mod.rs:168-205` (policy `:203`); `codex_adapter/mod.rs:814` |
| F9 | The prepare TxOutput commits in the same transaction as the lease INSERT. A keyed worker operation row (`idempotency_key` = task id) cannot be deleted; unkeyed rows still can | `operation/repo_sqlite.rs:273-320`; `calm-truth/src/db/sqlite/operations_keyed_rows_permanent_tests.rs:35-63`, `:66-71` |
| F10 | Codex verifies the checkout in `app_server_interact`. Recovery from `SpawnStarted` calls `spawn_side_effect` directly, and that path launches after `admit_task_side_effect` without verifying. Admission must run immediately before new provider or process effects | `codex_adapter/mod.rs:868-886`, `:935`, `:951`; `operation/driver.rs:540`; `operation/mod.rs:105` |
| F11 | Codex workers run `workspace-write` with cwd = TxOutput `cwd`. Resume sends `threadId` + config only. A persisted `thread_id` is reused | `shared_codex_appserver.rs:1225-1246`, `:3472-3500`; `codex_adapter/mod.rs:1164-1166` |
| F12 | Claude workers have no permission boundary | `claude_adapter/mod.rs:356-375` |
| F13 | The first prompt is `Goal/Context/Acceptance` plus the completion-ID footer. The system prompt says "the platform commits after you report" | `codex_adapter/mod.rs:1485-1525`; `prompts/worker/head-mcp.md:6` |
| F14 | Only `delivery_policy='kernel'` inserts a delivery. Two producers map a NULL-policy lease to `legacy_lease` ("predates kernel delivery"): the gate target and the `calm.plan.list` candidate view | `release.rs:126-150`; `task_verify_adapter/target.rs:95-110`, `:199-227`; `calm-types/src/verify_target.rs:39-50`; `git_candidate/view.rs:167-172`, `:266-269`; `tools/plan.rs:693` |
| F15 | Frontend strict readers: agent task blocks use `z.strictObject` and degrade an unknown key to `unsupported`. The gate-target enum lives in `fe/core/api`, which is `readonly`; a change there needs an `OWNERSHIP-CHANGE: <path> — <why> (#n)` trailer in the commit and in the PR body | `fe/core/domain/report.ts:135-162`, `:258`; `fe/core/api/schemas.ts:829-843`; `fe/module-file-inventory.yaml:44`; `fe/tools/ownership/validator.ts:37`, `:136`; `fe/core/AGENTS.md`. Precedents: `dccc140ff`, `71112fccd` |
| F16 | `WEB_COMPAT_VERSION` (35) must be equal in `routes/version.rs:33` and `fe/web/src/app/providers/public.tsx:16` (`scripts/gate-web-compat-version-lockstep.sh`). `SYNC_EVENT_VERSION` is bumped only with a migration default (`calm-types/src/event.rs:225-227`; `gate-sync-event-version-lockstep.sh`) | read |
| F17 | Track delete holds `lock_for_track_delete()` through its post-commit sweep. The sweep has every `git_common_dir` of the track's lease rows; area delete uses the same sweep | `routes/tracks.rs:3102`, `:126`, `:2976`; `workspace_lease/mod.rs:331-352`; `routes/areas.rs:568` |
| F18 | Boot reclaim only flips the row of an older-boot lease whose owner is not recoverable; recoverable owners are re-driven | `release.rs:233-262`; `operation/driver.rs:1047` |
| F19 | Worker forge actions run in the worker's lease path | `mcp_server/transport.rs:1029-1090` |
| F20 | Publish resolves the remote as `track_worktree_target(..).repo_root`, then `head_upstream(main root)`. Track branches have no upstream | `builtin_plugins/dev/publish.rs:163-200`; `track_worktree.rs:92`; `upstream.rs:84-96` |
| F21 | `git worktree add` runs repository hooks under an allowlisted environment, and a test pins this | `tests/cases/lease_git_env.rs:1-40` |
| F22 | isolated-codex-v1 is deleted, so there is no second lease kind. The doc of `workspace.leased` is stale | `71112fccd`; `calm-types/src/event.rs:602` |
| F23 | Review semantics belong to the dev template. The data root is `Config.data_dir` | `templates/builtin/issue-development.md:73`; `config.rs:26` |

**4140** (`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`, 2026-10-01):
- 118 tasks (codex 66, claude 48, terminal 4); none is non-terminal.
- 110 leases, all `released`. One already has a base with a NULL policy, so a NULL policy does
  not mark a pinned lease.
- Every lease has its owner op: `left join operations … where o.id is null` returns 0.
- 429 task blocks across all `track_vcs_objects` blobs; 0 carry `workspace` or
  `context.neige_workspace`.

Nothing changes meaning.

## 3. Design

### 3.1 Contract (calm-types)

```json
"workspace": { "kind": "track" }
"workspace": { "kind": "pinned", "head": "<full sha>", "base": "<full sha>" }
```

- **Type.** `TaskWorkspace` is tagged by `kind` (`deny_unknown_fields`). Omitted means `track`.
- **Registration.** The field goes on `TASK_FIELDS`, the `contracts.rs` properties and tombstone
  list, and `TASK_ROOT_HASH_FIELDS`. It is not a drift field (F3).
- **Values.** `head` is required and `base` is optional. Both must be full commit ids. `pinned`
  is allowed only on `kind: codex` tasks on the track's own route.
- **Errors** (`invalid_declaration`) list the valid choices:
  - `workspace.kind must be one of: track, pinned`
  - `workspace pinned requires kind: codex (got claude)`
- **Other surfaces.** #1921 adds `track_read` here. There is no CLI mirror (F6).

### 3.2 Persistence: none (D7, D12)

- **Task workspace at prepare.** Prepare reads the current root block through one
  `frozen_root_block_tx`, which is the claim fence's inline read (F4) moved so both callers
  share it.
  - If the block declares `pinned`, its root hash must equal the claim-frozen hash. Otherwise
    prepare refuses with the existing `context-stale: frozen closure no longer matches the
    document`.
  - Anything else takes today's track path unchanged: no new refusal for track tasks, and the
    existing context-stale handling (F4) stays as it is.
  - A pinned→track edit after the claim runs as track. That is safe because both kinds are
    admitted identically (D1).
- **Lease kind.** TxOutput carries `workspace`, `head`, `base` and `checkout`, committed with the
  lease row in the keyed worker op (F9).
  - The single reader is `lease_workspace_tx`:
    `json_extract(o.tx_output_json,'$.data.workspace')` via `lease_owner`.
  - It is used by the forge fence and by the two unbound-reason producers (§3.3 step 4).
- **Delivery policy.** `acquire_workspace_lease_at_path_tx` takes the policy from the plan
  instead of `base.map(Kernel)` (`mod.rs:203`): `Kernel` for track, NULL for pinned.

### 3.3 Layout and lifecycle (reuses `workspace_leases`)

`<data_dir>/pinned/<track_id>/` contains:

| Path | What it holds |
|---|---|
| `checkouts/<card_id>/` | The attempt's detached worktree. The worker cannot write it. Gate builds land in its own `target/` |
| `work/` | The worker cwd, writable, shared by the track's pinned attempts |
| `work/target/` | The worker's build directory |
| `work/artifacts/<card_id>/` | Reports |

All of it is removed at track delete (D3, D9). Sharing `work/` relies on the serialization of
pinned tasks under `track_idle` (D1, F7); #1917 must revisit it.

1. **Prepare.**
   - Repository root comes from the track row: `track_worktree_target(..).repo_root` for an
     attached track, `workspace_path` for a managed one.
   - Probe `git cat-file -e <head>^{commit}` (and `base`) through the bounded `run_git`.
   - Skip the clean-tree check and supersede.
   - Plan `checkout: Track{branch} | Pinned{head, base}`.
   - Lease row: `path` = `checkouts/<card>`; `canonical_path` = `canonicalize(data_dir)` joined
     with the validated segments; `git_common_dir` = `lease_git_common_dir(repo_root)`;
     `base_source='commit'`; `base_sha = head`; delivery policy NULL.
   - TxOutput `cwd` is `work/`.
   - Nothing touches the filesystem before the commit.
2. **Ensure (pinned only; D12, D13).**
   - It runs in `spawn_side_effect` after the exited no-op and after `admit_task_side_effect`
     (`codex_adapter/mod.rs:935`), and before `spawn_codex_worker_via_shared_daemon`. Every
     fresh launch and every `SpawnStarted` recovery passes through it. The worktree add runs
     hooks, so it is a process effect and must come after admission (F10).
   - Track tasks keep `verify_worker_checkout` in `app_server_interact` (`:877`), unchanged;
     `verify_codex_worker_workspace` skips it for pinned.
   - An existing checkout is kept only if all of these hold: it is registered at the path, HEAD
     is detached at `head`, its realpath equals `canonical_path`, and `git status --porcelain` is
     empty. Otherwise ensure runs `remove_workspace_worktree` (its guards; the target gains
     `head: Branch | Detached`), then `worktree add --detach` via `isolated_git_command`, then
     the check again.
   - Ensure never removes a checkout that passes the check, because a reused thread may
     already be running in it (F11).
3. **Read-only on start and on resume.**
   - Codex `workspace-write` with cwd `work/` leaves `checkouts/` and the common `.git`
     unwritable, and keeps network access. There is no sandbox code change.
   - A forge action from a pinned worker is refused before `resolve_forge_cwd` (F19) with
     `refused: pinned-checkout-read-only`.
4. **Release, gate and reason.**
   - Release: a NULL policy means no delivery (F14).
   - Gate: its cwd is the checkout (F5).
   - Reason: both producers (F14) ask `lease_workspace_tx` and emit the new `pinned_checkout`
     for a pinned lease. Existing NULL-policy rows keep `legacy_lease`.
   - Enum changes: `UnboundReason::PinnedCheckout` in `calm-types/src/verify_target.rs`;
     `FrozenUnbound::PinnedCheckout` plus its wire arm (`target.rs:95-110`);
     `git_candidate/view.rs` `UnboundReason::PinnedCheckout` (`:167-172`, `:266-269`).
5. **Reclaim at track delete only (D9).**
   - In the post-commit sweep, still under `lock_for_track_delete()` (F17), run
     `rm -rf <data_dir>/pinned/<track>/`.
   - Then run `git --git-dir=<common> worktree prune` for every `git_common_dir` the sweep
     already captured from the track's lease rows. That covers attached and managed tracks, and
     area delete.
6. **Admission is unchanged (D1).**

### 3.4 Worker header and template

`render_task_worker_prompt` (F13) gains a `Workspace:` section for pinned tasks only:
- `checkout`: read-only and never committed. This overrides "the platform commits after you
  report".
- `head`, and `base` or "not declared".
- `remote`: from one `track_repo_remote(track)`, extracted from publish (F20), or
  `none (no upstream remote for <repo>)`.
- "Run git and gh inside the checkout (`cd <checkout>`); pass the PR number explicitly."
- `work` (cwd), `artifacts/<card>` (name the report file in `calm.task.complete`), and
  `target`.
- The gate runs in the checkout.

In `templates/builtin/issue-development.md`, the "Working method" section (`:73`) gains one
paragraph:
- Each review channel is a `kind: codex` task with
  `workspace: {kind: pinned, head: <PR head>, base: <PR base>}`.
- Its goal names the channel role and the PR number.
- It contains no clone, `--repo`, directory or target commands.

### 3.5 Frontend and versions (D10, D11)

- **`fe/core/domain/report.ts`** (not `readonly`). `agentTaskBlockPayloadSchema` gains
  `workspace`: a strict discriminated union on `kind`, either `track` or
  `pinned {head, base?}`, nullish. Terminal blocks keep refusing the field. Contract coverage
  goes in `report.test.ts`.
- **`fe/core/api/schemas.ts:829-831`** gains `'pinned_checkout'`, with a case in
  `schemas.contract.test.ts`, and `gen:api` regenerates `generated/wire.ts`.
- **Ownership trailers.** Each `fe/core/api/*` path carries
  `OWNERSHIP-CHANGE: <path> — <why> (#1933)` in the commit and the PR body (F15). This is the
  owner-layer change request that `fe/AGENTS.md` and `fe/core/AGENTS.md` require; the
  orchestrator approved it as D11.
- **Versions.**
  - `WEB_COMPAT_VERSION` goes 35 → 36 in both files (F16). A stale bundle would otherwise
    degrade pinned blocks to `unsupported` and reject `pinned_checkout` gate events; with the
    bump it shows the server-update notice instead.
  - `SYNC_EVENT_VERSION` stays: there is no migration default and no stamp (F16).
  - `REST_API_VERSION` stays: `neige-app` reads none of these types
    (`grep -rln UnboundReason crates/neige-app` finds 0).
- **Cost verdict (D11): not prohibitive.**
  - About 6 production files: 3 Rust enums and maps, 2 FE schemas, 2 version constants.
  - Their tests.
  - The trailer lines.

## 4. Failure and diagnostic matrix

| Case | Detected at | Outcome | Diagnostic |
|---|---|---|---|
| Bad shape: abbreviated head, unknown kind, claude, terminal or child route | `validate_task` | Block invalid, never scheduled | `invalid_declaration` listing valid choices |
| Pinned block changed between claim and prepare | `frozen_root_block_tx` | `spawn-failed`, no row | `context-stale: frozen closure no longer matches the document` (existing text) |
| Unknown head or base | Prepare | `spawn-failed`, no row, no directory | `refused: pinned-head-unknown: commit <sha> is not in <repo>; push or fetch it, then declare again under a new key` (`pinned-base-unknown` likewise) |
| Checkout HEAD moved, on a branch, dirty or partial | Ensure (`spawn_side_effect`) | Removed and added again at `head` | On failure: `refused: pinned-checkout-unavailable: git worktree add failed in <repo>: <stderr>` |
| Attempt not startable at launch | `admit_task_side_effect` before ensure | No worktree add, no hook run | Existing admission text |
| Kernel restart, owner recoverable | Driver re-drive, then ensure on `SpawnStarted` | Checkout verified or recreated | As above |
| Older machine boot, owner not recoverable | Boot reclaim (F18) | Attempt `spawn-failed: <BOOT_RECLAIM_REASON>`; row released, no delivery | Existing text |
| No repository remote | Header | Runs | `remote: none (no upstream remote for <repo>)` |
| Cancel, timeout, track close | Existing paths | Row released; trees stay until delete | — |
| Track or area deleted | Post-commit sweep under the delete lock | `pinned/<track>/` removed, registrations pruned | — |
| Forge action from a pinned worker | `transport.rs` | Refused | `refused: pinned-checkout-read-only: a pinned worker cannot run forge actions` |

The permission, candidate, session and merge fences are unchanged, and so is every track-task
path (D12).

## 5. Composition

- **#1917.** Pinned keeps serialization (D1). When #1917 makes admission conflict-based, it can
  classify pinned leases through `lease_workspace_tx` as using no track checkout. Before pinned
  tasks run concurrently, it must give each attempt its own `work/` and `target`.
- **#1921.** `track_read` becomes the third variant of this field.
  - If #1921 keeps its `access_mode` column, pinned rows write `read_only` there, and
    `lease_workspace_tx` reads that column.
  - `track_read` uses the Codex `read-only` sandbox, because its cwd is the shared checkout.

## 6. Slice plan (one PR, ~860 lines: ~440 prod, ~420 test, including ~60 FE)

Contents:
- **calm-types:** the enum, validator, schema and root-hash registration.
- **Prepare side:** `frozen_root_block_tx` and the delivery-policy parameter.
- **`workspace_lease/pinned.rs`:** prepare, ensure and the delete step. It is a new file because
  `mod.rs` is 791 lines.
- **Adapter:** the pinned-only ensure in `spawn_side_effect`, the verify skip for pinned, and the
  forge fence.
- **Shared code:** `track_repo_remote` extracted from `publish.rs`.
- **Wire:** the 3 Rust reason enums and maps, the header, the template paragraph, and the
  `event.rs:602` doc.
- **Frontend:** `report.ts`, `schemas.ts` and its generated files, `WEB_COMPAT_VERSION` 36 ×2,
  and the `OWNERSHIP-CHANGE` trailers.
- **Not included:** no migration, no `Task` change, no Claude change, no sandbox change, and no
  change to track-task behavior.

Must-red tests. Each names the production mutation that turns it red; * marks the ones that get
mutation verification.
- T1 `pinned_head_must_be_full_commit_id`: remove the full-hex check.
- T2 `pinned_refused_off_codex_lists_valid_choices`: remove the kind check.
- T3* `pinned_prepare_refuses_root_changed_since_claim`: skip the hash comparison.
- T4 `track_task_root_edit_keeps_todays_prepare_path`: apply the comparison to every task.
- T5* `pinned_unknown_head_refused_before_any_row`: skip the `cat-file` probe.
- T6 `pinned_checkout_on_a_branch_is_replaced`: remove the detached check.
- T7 `pinned_worker_cwd_contract`: TxOutput `cwd` = checkout. Write denial is covered by
  acceptance (d).
- T8* `pinned_checkout_moved_is_replaced_before_launch`: skip the HEAD check.
- T9* `pinned_recovery_from_spawn_started_ensures_checkout`: leave ensure in
  `app_server_interact`.
- T10 `pinned_partial_checkout_recreated`: trust an existing directory.
- T11 `pinned_ensure_runs_after_admission`: call ensure before `admit_task_side_effect`. A
  non-startable attempt must leave no hook probe.
- T12 `track_spawn_started_recovery_unchanged`: apply ensure to track tasks.
- T13* `pinned_release_writes_no_delivery`: pass `Kernel` for pinned.
- T14 `pinned_gate_cwd_is_checkout`: lease path = `work/`.
- T15* `pinned_reason_in_gate_target_and_plan_list`: map NULL policy to `legacy_lease`
  unconditionally, in the gate producer on one run and in the plan.list producer on the other.
  A legacy NULL-policy row stays `legacy_lease`.
- T16 `track_delete_removes_pinned_tree_and_prunes`: remove the delete step. Covers an attached
  and a managed track.
- T17* `pinned_worker_forge_action_refused`: remove the fence.
- T18 `pinned_header_renders_shared_remote_and_checkout_cwd`: resolve through the track
  worktree's upstream.
- T19 `pinned_worktree_hooks_see_only_the_allowlisted_environment` (`lease_git_env.rs`, header
  updated): use a plain `Command`.
- T20 (FE, `report.test.ts`) a pinned agent block parses as a task: drop `workspace` from the
  shape, and it degrades to `unsupported`.
- T21 (FE, `schemas.contract.test.ts`) a `pinned_checkout` gate target parses: remove the enum
  value.
- The existing partition tests (F2, F3) also apply. The drift test gains `workspace` in its
  not-stored set.

Gates:
- `scripts/local-ratchet-gates.sh`.
- `scripts/gate-web-compat-version-lockstep.sh`.
- Targeted `cargo nextest` for `calm-types` and `calm-server`, then the whole `-p calm-server`
  run, then `scripts/local-rust-gates.sh --quick`.
- Goldens: `mcp_tool_registry.json` and `issue_development_planner_prompt.txt`.
- `(cd fe && npm ci && npm run gen:api && npm run lint && npm run build && npm test)`. The
  `lint:js` step runs `check-readonly-change-requests.mjs`, which requires the trailers.
- Check `fe/tools/mutation/manifest.json` anchors in `schemas.ts`; `71112fccd` re-based one.
- Not triggered: migrations, `head_schema_fixture.rs`, `SYNC_EVENT_VERSION`, `worker_prompt_*`,
  `planner.md` caps.

## 7. Acceptance (issue item 4, D5)

Run a dev kernel built from the PR branch, with `data_dir` outside `/tmp`. Do not use 4140 and do
not restart it. A dev-template track reviews a real open PR of this repository with two
`kind: codex` pinned channels on the same head. All of these must hold:
- (a) The Planner's blocks hold no clone, `--repo`, directory or target commands.
- (b) Each worker runs `cd <checkout> && gh pr view <n>` without `--repo`, and the header shows
  the remote.
- (c) Each gate log shows its own checkout as cwd.
- (d) A worker write into the checkout fails on start and after a daemon resume.
- (e) The track worktree's `git status` and the product diff are unchanged.
- (f) The artifacts are readable while the track exists. After the track is deleted,
  `pinned/<track>/` is gone and `git worktree list` lacks both checkouts.
- (g) An unknown head ends with `refused: pinned-head-unknown`.
- (h) The browser shows the pinned task blocks as tasks.

## 8. KNOWN GAPS

- Claude cannot use `pinned` (D2, F12). Follow-up: a Claude worker sandbox, then claude here.
- Track delete has no worker or gate stop proof, the same contract as the track worktree today.
- Disk is held until track delete: one checkout plus one cold gate `target/` per attempt (about
  455 MB per channel and round, per the §1 evidence). Follow-up option: a generic gate
  build-dir variable.
- Network is on, so a pinned worker's shell `gh` can still merge or review.
- `/tmp` and `$TMPDIR` stay writable under `workspace-write`, so `data_dir` must not be under
  `/tmp`.
- A head that exists only on a fork or an unfetched remote is refused; there is no kernel fetch.
- The repository `AGENTS.md` is not auto-loaded (the cwd is `work/`); the header names it.
- Read-only rests on the Codex `workspace-write` semantics and on resume keeping the cwd. Both
  are proven by acceptance (d).

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
- There is no migration and no new column. The frontend reader, one wire enum value and one
  optional wire field change.

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
| D15 | The workspace kind is frozen at claim, in `claim_context_json` |
| D16 | Supported layout: source repo, track worktree and `data_dir` outside `/tmp` and `$TMPDIR` |
| D17 | Delete removal lives in the shared sweep; prune only repositories still present |

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
| F4 | The claim writes `claim_context_json` as a JSON array of `TaskContextRef` (the root ref and its hash first) and re-reads the root block in its own transaction. Every reader parses that array; the refs are compared field by field, not as whole structs. Prepare refuses only once the monitor has marked `context_stale_at_ms` | `calm-truth/src/db/sqlite/task.rs:157-174`; `calm-types/src/event.rs:15-25`; `scheduler/mod.rs:1029-1048`; `task_context.rs:467-530`, `:1159-1176`; `operation/mod.rs:85-102`; `codex_adapter/mod.rs:769` |
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
| F17 | Track delete holds `lock_for_track_delete()` through its post-commit sweep. It quiesces each card's active Codex turn first and propagates any error. Managed recycling renames the directory into trash before commit. The shared `sweep_workspace_worktrees_for_tracks` has every `git_common_dir` of the lease rows; area delete calls the same sweep | `routes/tracks.rs:3102`, `:126`, `:2672`, `:2845`, `:2860`, `:2976`; `workspace_recycle.rs:325`; `workspace_lease/mod.rs:331-366`; `routes/areas.rs:648` |
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
  `context.neige_workspace`. Every `claim_context_json` is a ref array with no `workspace` key.
- Layout (D16): 31 tracks, 0 with `workspace_path` or `workspace_worktree_path` under `/tmp`
  (`select count(*), sum(workspace_path like '/tmp/%'), sum(coalesce(workspace_worktree_path,'')
  like '/tmp/%') from tracks` → `31|0|0`). `CALM_DATA_DIR=/home/kenji/.local/share/neige-next/data`
  (`~/.local/share/neige-next/env:9`). The Codex daemon has no `TMPDIR`
  (`tr '\0' '\n' < /proc/<codex app-server pid>/environ | grep ^TMPDIR=` is empty).

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
  - on a terminal task: `workspace applies to: codex, claude`
- **Other surfaces.** #1921 adds `track_read` here. There is no CLI mirror (F6).

### 3.2 Persistence: no new column (D7, D12, D15)

- **Frozen at claim (D15).** The root `TaskContextRef` gains
  `#[serde(default, skip_serializing_if = "TaskWorkspace::is_track")] workspace: TaskWorkspace`.
  `context_ref` fills it from the root block's payload, and only for root refs; non-root refs
  always omit it. So the claim's existing write (F4) freezes the choice into the existing
  `claim_context_json`, while recomputed root refs still match frozen ones.
  - Old rows have no key, so they read as `track` (4140: all of them).
  - `TaskContextRef` is a ts-exported wire type, so `gen:api` regenerates `wire.ts`. ts-rs
    emits `workspace?: TaskWorkspace`, and `schemas.contract.test.ts:201` pins
    `z.infer<wireEventSchema>` to the generated `Event`. So `TaskWorkspace` gets `#[derive(TS)]`
    plus an export, and the refs object of `taskContextFrozenSchema`
    (`fe/core/api/schemas.ts:505-518`) gains an optional `workspace` (§3.5).
  - The compiler flags the `TaskContextRef` literals in 6 test files: `event_serde_goldens`,
    `mcp_track_state`, `task_projection_acceptance`, `task_terminal`,
    `terminal_lifecycle_task_delete` and `task_attempt_tests`.
- **Prepare branches on the frozen kind,** read from the root ref in the task row's
  `claim_context_json`.
  - **Frozen `pinned`.** `frozen_root_block_tx` (the claim fence's inline read, F4, moved so
    both callers share it) requires the current root to exist with a hash equal to the frozen
    one. `head` and `base` then come from that hash-verified block. Otherwise prepare refuses
    before any row is written, with its own text:
    `refused: pinned-declaration-changed: task block <id> changed or was removed after claim;
    declare again under a new key`.
  - **Frozen `track`, or no key.** Today's path, byte for byte. There is no root re-read and no
    new refusal (D12).
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

Supported layout (D16): the canonical source repository, the track worktree and `data_dir`
must all lie outside `/tmp` and `$TMPDIR`. Codex `workspace-write` keeps both writable, so a
location under them is writable by every worker. 4140 satisfies this (§2).

1. **Prepare.**
   - Prepare branches on the frozen kind (§3.2). The pinned steps below run only for a frozen
     `pinned`.
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
   - Ensure also creates `work/`, `work/target` and `work/artifacts/<card>`.
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
5. **Reclaim at track delete only (D9, D17).**
   - The step lives inside the shared `sweep_workspace_worktrees_for_tracks`
     (`workspace_lease/mod.rs:355-366`), so track delete (`tracks.rs:2976`, still under
     `lock_for_track_delete()`) and area delete (`areas.rs:648`) both run it. The sweep gains a
     `data_dir` argument.
   - It runs `rm -rf <data_dir>/pinned/<track>/`.
   - It then runs `git --git-dir=<common> worktree prune` only for a captured `git_common_dir`
     that is still a directory, which means an attached repository.
   - A managed track's repository was renamed into trash before commit (F17), so its stale
     registrations go with the existing trash retention.
6. **Admission is unchanged (D1).**

### 3.4 Worker header and template

`render_task_worker_prompt` (F13) gains a `Workspace:` section for pinned tasks only:
- `checkout`: read-only and never committed. This overrides "the platform commits after you
  report".
- `head`, and `base` or "not declared".
- `remote`: from one `track_repo_remote(track)` in kernel `workspace_lease/upstream.rs`, next to
  `head_upstream`; dev publish calls it too (F20). Otherwise it reads
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
- **`fe/core/api/schemas.ts:505-518`**: the `taskContextFrozenSchema` refs object gains an
  optional `workspace`. It reuses the `report.ts` workspace schema and is covered by the same
  `schemas.ts` OWNERSHIP-CHANGE trailer.
- **Ownership trailers.** Each `fe/core/api/*` path carries
  `OWNERSHIP-CHANGE: <path> — <why> (#1933)` in the commit and the PR body (F15). This is the
  owner-layer change request that `fe/AGENTS.md` and `fe/core/AGENTS.md` require; the
  orchestrator approved it as D11.
- **Versions.**
  - `WEB_COMPAT_VERSION` goes 35 → 36 in both files (F16), and
    `crates/calm-server/tests/cases/version.rs:145`, `:150` pin the literal 36. A stale bundle
    would otherwise degrade pinned blocks to `unsupported` and reject `pinned_checkout`. With
    the bump, the server's `minWebCompatVersion` 36 is above the bundle's 35: a bundled app
    shows the `app-update` notice and a browser shows the refresh overlay
    (`fe/web/src/app/providers/public.tsx:88-89`).
  - `SYNC_EVENT_VERSION` stays: there is no migration default and no stamp (F16).
  - `REST_API_VERSION` stays: `neige-app` reads none of these types
    (`grep -rln UnboundReason crates/neige-app` finds 0).
- **Cost verdict (D11): not prohibitive.**
  - 8 hand-written production files: 3 Rust reason producers (`verify_target.rs`, `target.rs`,
    `view.rs`), `TaskContextRef` (`event.rs`, plus `TaskWorkspace` `#[derive(TS)]`),
    `routes/version.rs`, `schemas.ts` (the reason enum and the context-frozen ref),
    `report.ts` and `public.tsx`.
  - The generated `wire.ts`, the version test, the contract tests, the 6 `TaskContextRef` test
    literals, and the trailer lines.

## 4. Failure and diagnostic matrix

| Case | Detected at | Outcome | Diagnostic |
|---|---|---|---|
| Bad shape: abbreviated head, unknown kind, claude, terminal or child route | `validate_task` | Block invalid, never scheduled | `invalid_declaration` listing valid choices |
| Frozen-pinned block changed, removed or edited to track after claim | `frozen_root_block_tx` (prepare) | `spawn-failed`, no row | `refused: pinned-declaration-changed: task block <id> changed or was removed after claim; declare again under a new key`, or `context-stale` if the monitor already marked it. Either way it is a refusal |
| Frozen-track block edited after claim | — | Today's path, unchanged (D12) | — |
| Unknown head or base | Prepare | `spawn-failed`, no row, no directory | `refused: pinned-head-unknown: commit <sha> is not in <repo>; push or fetch it, then declare again under a new key` (`pinned-base-unknown` likewise) |
| Checkout HEAD moved, on a branch, dirty or partial | Ensure (`spawn_side_effect`) | Removed and added again at `head` | On failure: `refused: pinned-checkout-unavailable: <git worktree remove --force \| git worktree add> failed in <repo>: <stderr>` |
| Crash inside `worktree add` leaves a `locked` registration | Ensure | One `remove --force` cannot clear a lock, so spawn fails | The remove arm of the row above (KNOWN GAP) |
| Attempt not startable at launch | `admit_task_side_effect` before ensure | No worktree add, no hook run | Existing admission text |
| Kernel restart, owner recoverable | Driver re-drive, then ensure on `SpawnStarted` | Checkout verified or recreated | As above |
| Older machine boot, owner not recoverable | Boot reclaim (F18) | Attempt `spawn-failed: <BOOT_RECLAIM_REASON>`; row released, no delivery | Existing text |
| No repository remote | Header | Runs | `remote: none (no upstream remote for <repo>)` |
| Cancel, timeout, track close | Existing paths | Row released; trees stay until delete | — |
| Track or area deleted while a pinned worker's turn cannot be quiesced | `teardown_track_deletion` (`tracks.rs:2672`, `:2845`) | Delete refused; nothing removed | The existing quiescence error |
| Track or area deleted while a gate or a stuck owner's process still runs | — | Trees removed under it (KNOWN GAP) | — |
| Track or area deleted | Shared sweep | `pinned/<track>/` removed; attached registrations pruned; managed registrations follow trash retention | — |
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

## 6. Slice plan (one PR, ~900 lines: ~450 prod, ~450 test, including ~60 FE)

Contents:
- **calm-types:** the enum, validator, schema and root-hash registration.
- **Prepare side:** `frozen_root_block_tx` and the delivery-policy parameter.
- **`workspace_lease/pinned.rs`:** prepare, ensure and the delete step. It is a new file because
  `mod.rs` is 791 lines.
- **Adapter:** the pinned-only ensure in `spawn_side_effect`, the verify skip for pinned, and the
  forge fence.
- **Shared code:** `track_repo_remote` in `workspace_lease/upstream.rs`, called by publish and
  the header.
- **Claim freeze:** the root `TaskContextRef.workspace` field and its `context_ref` fill.
- **Wire:** the 3 Rust reason enums and maps, the header, the template paragraph, and the
  `event.rs:602` doc.
- **Frontend and versions:** `report.ts`, `schemas.ts` and its generated files,
  `WEB_COMPAT_VERSION` 36 in `version.rs` and `public.tsx`, the literal in
  `tests/cases/version.rs:145`, `:150`, and the `OWNERSHIP-CHANGE` trailers.
- **Not included:** no migration, no `Task` change, no Claude change, no sandbox change, and no
  change to track-task behavior.

Must-red tests. Each names the production mutation that turns it red; * marks the ones that get
mutation verification.
- T1 `pinned_head_must_be_full_commit_id`: remove the full-hex check.
- T2 `pinned_refused_off_codex_lists_valid_choices`: remove the kind check.
- T3* `pinned_prepare_refuses_root_changed_since_claim`: skip the hash comparison. The test
  asserts `context_stale_at_ms IS NULL` right before prepare, so the old fence cannot pass it.
- T3b `pinned_to_track_edit_after_claim_refused`: branch on the current block's kind instead of
  the frozen one. The test asserts `context_stale_at_ms IS NULL` right before prepare and the
  `pinned-declaration-changed` diagnostic.
- T3c `pinned_root_deleted_after_claim_refused`: treat a missing root as track. The test asserts
  `context_stale_at_ms IS NULL` right before prepare and the `pinned-declaration-changed`
  diagnostic.
- T3d `claim_context_without_workspace_reads_as_track`: make the field required, so 4140-shaped
  JSON fails.
- T4 `track_task_root_edit_keeps_todays_prepare_path`: apply the comparison to every task. It
  also asserts `context_stale_at_ms IS NULL` before prepare.
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
- T12 `track_spawn_started_recovery_unchanged`: run `verify_worker_checkout` in
  `spawn_side_effect` for every task.
- T13* `pinned_release_writes_no_delivery`: pass `Kernel` for pinned.
- T14 `pinned_gate_cwd_is_checkout`: lease path = `work/`.
- T15* `pinned_reason_in_gate_target_and_plan_list`: map NULL policy to `legacy_lease`
  unconditionally, in the gate producer on one run and in the plan.list producer on the other.
  A legacy NULL-policy row stays `legacy_lease`.
- T16 `delete_removes_pinned_tree_and_prunes_present_repos`: remove the step from the shared
  sweep. Covers an attached track delete, a real managed track delete (with recycling), and an
  area delete.
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
- The implementation brief records each mutation's complete predicted red set before it runs,
  per the `AGENTS.md` mutation rules.

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

Run a dev kernel built from the PR branch, with the source repository, the track worktree and
`data_dir` outside `/tmp` and `$TMPDIR` (D16). Do not use 4140 and do not restart it. A dev-template track reviews a real open PR of this repository with two
`kind: codex` pinned channels on the same head. All of these must hold:
- (a) The Planner's blocks hold no clone, `--repo`, directory or target commands.
- (b) Each worker runs `cd <checkout> && gh pr view <n>` without `--repo`, and the header shows
  the remote.
- (c) Each gate log shows its own checkout as cwd.
- (d) On start and again after a daemon resume, the worker attempts a write into each of the
  pinned checkout, the source repository and the track worktree. Every attempt is denied. A
  final clean-tree check alone does not count.
- (e) The track worktree's `git status` and the product diff are unchanged.
- (f) The artifacts are readable while the track exists. After the track is deleted,
  `pinned/<track>/` is gone and `git worktree list` lacks both checkouts.
- (g) An unknown head ends with `refused: pinned-head-unknown`.
- (h) The browser shows the pinned task blocks as tasks.

## 8. KNOWN GAPS

- Claude cannot use `pinned` (D2, F12). Follow-up: a Claude worker sandbox, then claude here.
- Track delete has no gate stop proof, the same contract as the track worktree today. A
  cancel or timeout whose interrupt failed leaves a `stuck` owner, which `track_idle` ignores
  (`track_idle.rs:35-47`). The next pinned attempt can then share `work/` with it.
- A crash inside `worktree add` leaves a `locked` registration that one `remove --force` cannot
  clear. The track worktree shares this today.
- Disk is held until track delete: one checkout plus one cold gate `target/` per attempt (about
  455 MB per channel and round, per the §1 evidence). Follow-up option: a generic gate
  build-dir variable.
- Network is on, so a pinned worker's shell `gh` can still merge or review.
- A layout with the source repository, track worktree or `data_dir` under `/tmp` or `$TMPDIR`
  is unsupported: `workspace-write` leaves those writable. This applies to every Codex worker
  today, not only to pinned.
- A head that exists only on a fork or an unfetched remote is refused; there is no kernel fetch.
- The repository `AGENTS.md` is not auto-loaded (the cwd is `work/`); the header names it.
- Read-only rests on the Codex `workspace-write` semantics and on resume keeping the cwd. Both
  are proven by acceptance (d).

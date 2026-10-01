# #1933 — pinned review checkouts

**Owner rules.** (1) Cover the observed pain with the fewest mechanisms; hypotheticals are
one-line KNOWN GAPS. (2) Compatibility is the 4140 database only. (3) A task option applies to
every same-kind path; errors list the valid choices. (4) The kernel owns generic lifecycle and
authorization; the dev template owns review semantics. Nothing infers "review" from goal text.

**Outcome.** A codex task block gains a typed `workspace` field. `track` (the default, today's
behavior) runs in the track checkout. `pinned {head, base?}` runs against a private, detached
`git worktree` of the track's repository at exactly `head`, which the worker can read but not
write, with kernel-allocated work, artifact and build directories, the repository remote as a
header fact, and its gate run in that checkout. The checkout is removed once the attempt is over
and its worker has stopped. There is no migration and no new column.

**Owner decisions (2026-10-01, round 1).**
- D1: no `track_idle` bypass.
- D2: Codex only in this issue.
- D3: artifacts live until track delete.
- D5: acceptance runs on a dev kernel with both channels as Codex.
- D6: a migration number would be assigned at merge.
- D7: persistence is subtracted (§3.2).
- D8: the stale `event.rs:602` comment is fixed.

## 1. Problem and evidence

PR #1927 (track `e0646de4…`, frozen head `d448361f…`, 4140 at `a4a16f14`, 2026-10-01):
- Two Codex reviewers each hand-ran `git clone --shared --no-checkout` + a detached checkout.
- Their first `gh pr view` failed because the clone's `origin` was a local path; both added
  `--repo keanji-x/neige-calm`.
- The Planner hand-assembled base/head, checkout, report dir, target dir and gate cwd in goal
  text.
- Nothing reclaimed `/tmp/neige-1545-pr1927-{a,b}-r1`.

## 2. Facts (origin/main `bcd4dec59`)

| # | Fact | Where |
|---|---|---|
| F1 | Tasks are declared as report `task` blocks. `calm.plan.upsert` is a retired shim that writes nothing | `track_report_blocks/contracts.rs:258-357`; `tools/plan.rs:398-412` |
| F2 | Field vocabulary and validation: `TASK_FIELDS`, `validate_task` (unknown keys rejected); a test pins schema == `TASK_FIELDS` | `calm-types/src/report_blocks/kinds.rs:152-173`; `contracts.rs:653-664` |
| F3 | The root hash covers only keys present in the payload, so a key absent from history changes no released hash. Hashed fields that are not stored (`refs`, `no_gate_reason`) are an explicit test exclusion | `calm-types/src/task_recovery.rs:15-48`; `task_context.rs:1383-1410` |
| F4 | The claim freezes the task's root ref (`block_id` + root hash) in `tasks.claim_context_json`. Inside the claim tx it re-reads the root block from the report card and races-lost on any hash change | `scheduler/mod.rs:941-1050` (fence `:1029-1048`); `task_context.rs:292-330`, `:1159-1176`; `calm-truth/src/db/sqlite/task.rs:166` |
| F5 | Agent tasks may not set `gate.cwd`; their gate runs in the worker card's latest lease `path`, in any lease state (the worker releases before the gate) | `report_blocks/tasks.rs:283-287`, `:579-595`; `task_verify_adapter/mod.rs:441-496` |
| F6 | No CLI command declares tasks | `mcp_server/cli/commands.rs:81-247` |
| F7 | Admission: `compute_ready` and the claim re-check `track_idle`. It is false while any codex/claude task is in flight or any lease of the track is held | `scheduler/mod.rs:136-151`, `:878`, `:1079`; `calm-truth/src/db/sqlite/track_idle.rs:15-58` |
| F8 | Worker prepare is `prepare_worker_lease_tx` (track checkout, clean-tree check, supersede, HEAD base), then `acquire_workspace_lease_tx`. That is the only lease INSERT: `lease_owner = op.id`, `delivery_policy='kernel'`, track freeze, `workspace.leased` | `codex_adapter/mod.rs:762-866`, `:814`; `workspace_lease/worker.rs:92-125`; `workspace_lease/mod.rs:137-245` |
| F9 | The prepare TxOutput is written to `operations.tx_output_json` in the same transaction as the lease INSERT. Operations rows are never deleted. The gate already reads `$.data.cwd` from it | `operation/repo_sqlite.rs:273-320`; `grep -rn "DELETE FROM operations" crates` = 0 hits; `task_verify_adapter/mod.rs:463-477` |
| F10 | `verify_worktree_base` requires HEAD == base, the realpath **and a branch** (a detached HEAD fails). It re-runs whenever the driver re-enters spawn after a restart | `worker.rs:129-140`; `base.rs:447-487`; `codex_adapter/mod.rs:877`, `:1555`; `operation/driver.rs:350-540` |
| F11 | Codex workers: `workspace-write`, approval `never`, cwd = TxOutput `cwd`. Resume sends `threadId` + shell-env config only. Track workers already rely on the daemon keeping a resumed thread's cwd and sandbox | `shared_codex_appserver.rs:1225-1246`, `:3472-3500`; `codex_adapter/mod.rs:898`, `:1146-1181` |
| F12 | Claude workers have no permission boundary (`--allow-dangerously-skip-permissions`; settings hold only hooks and attribution) | `claude_adapter/mod.rs:356-375`; `routes/claude_cards.rs:313-341` |
| F13 | The first prompt is `Goal/Context/Acceptance` only | `codex_adapter/mod.rs:1485-1525` |
| F14 | Release inserts a git delivery only for `delivery_policy='kernel'`. A non-kernel lease is gate-`Unbound{LegacyLease}` | `workspace_lease/release.rs:126-150`; `task_verify_adapter/target.rs:199-227` |
| F15 | Boot reclaim is row-only. #1830 S2 deleted the #1815 disk reclaim | `release.rs:193-262`; `operation/driver.rs:351`; `1830-s2-worker-in-track-worktree.md:16`, `:236` |
| F16 | Track delete cascades lease rows; its sweep removes only the track worktree and candidate refs | `workspace_lease/mod.rs:247-363`; `0115_workspace_lease_upstream_base.sql:34` |
| F17 | Worker forge actions run in the worker's lease path | `mcp_server/transport.rs:1029-1090` |
| F18 | There is no typed owner/name repository field. Dev publish uses `head_upstream(repo_root).url` as `gh --repo` | `workspace_lease/upstream.rs:40-100`; `builtin_plugins/dev/publish.rs:157-200` |
| F19 | isolated-codex-v1 was deleted in #1893 S4; there is no second lease kind to reuse. The `workspace.leased` doc is stale | `71112fccd`; `0128_drop_planner_dispatch_receipts.sql`; `calm-types/src/event.rs:602` |
| F20 | Review semantics live in the dev template, which says nothing about checkouts | `templates/builtin/issue-development.md:73`, `:82-106`; `docs/architecture/1897-builtin-dev.md:7-9` |
| F21 | Typed data root: `Config.data_dir` | `config.rs:26`, `:174-182` |

**4140** (`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`, 2026-10-01):
- Tasks: 118 (`select kind,count(*) from tasks group by kind`: claude 48, codex 66, terminal 4),
  none non-terminal.
- Leases: 110, all `released`. One row already has a base with `delivery_policy` NULL
  (`select count(*) from workspace_leases where delivery_policy is null and base_sha is not null`
  = 1), so NULL policy is not a pinned marker.
- Every lease has its owner operation (`… left join operations o on o.id=wl.lease_owner where
  o.id is null` = 0).
- Report blocks: JSON-parsing every `track_vcs_objects` blob gives 429 task blocks, 0 with
  `workspace` and 0 with `context.neige_workspace`.
- Migrations: the latest is 0128.

Nothing changes meaning.

## 3. Design

### 3.1 Contract (calm-types owns the vocabulary)

```json
"workspace": { "kind": "track" }
"workspace": { "kind": "pinned", "head": "<full sha>", "base": "<full sha>" }
```

- `TaskWorkspace` is a `kind`-tagged enum (`deny_unknown_fields`); omitted means `track`.
- Registered on `TASK_FIELDS`, the `contracts.rs` properties and tombstone forbid list, and
  `TASK_ROOT_HASH_FIELDS`. It is not in `PROJECTION_DRIFT_TASK_FIELDS` (not stored, F3).
- `head` is required and `base` is optional. Both are full commit ids (40/64 lowercase hex).
- `pinned` is allowed only on `kind: codex` tasks on the track's own route. Errors
  (`invalid_declaration`) name the valid choices:
  - `workspace.kind must be one of: track, pinned`
  - `workspace pinned requires kind: codex (got claude)`
  - `workspace applies only to codex/claude tasks on the track's own route`
- #1921 adds `track_read` as a third variant here instead of `context.neige_workspace`.
- No CLI mirror is needed (F6).

### 3.2 Persistence: none (D7)

- **The task's workspace needs no `tasks` column.** Its one reader is the worker op's
  `prepare_tx`, which runs once per attempt. It reads the claim-frozen root block: the root ref
  in `claim_context_json`, then the report card in-tx, then the hash compared with the frozen
  hash, then `workspace` from the payload. Because `workspace` is a root-hash field, an equal
  hash proves this is the value the claim admitted. A changed hash refuses
  (`declaration_changed_in_flight`). The claim fence's inline read (F4,
  `scheduler/mod.rs:1029-1048`) moves into one `frozen_root_block_tx` that both callers use.
  `build_worker_payload` stays unchanged and hash-stable.
- **The lease needs no `workspace_leases` column.** Prepare writes `workspace`, `head`, `base`,
  `checkout`, `work_dir` into TxOutput, which is committed with the lease row (F9). One
  `lease_workspace_tx(lease)` reads it, the way the gate already reads `$.data.cwd`:
  `json_extract(o.tx_output_json,'$.data.workspace')` via `lease_owner`. Its only readers are
  the forge fence, the target reason and the reclaim sweep.
  - NULL `delivery_policy` alone would be ambiguous: 1 row in 4140 already has it.
  - A path-prefix test would infer meaning from a configurable root.
- **Result: zero migrations.** If review forces one, its number is assigned last, at merge;
  draft #1921 also claims 0129.

### 3.3 Kernel lifecycle (reuses `workspace_leases`)

Layout: `<data_dir>/pinned/<track_id>/<card_id>/` (F21) holds `checkout/` (detached worktree),
`work/` (the worker cwd), `work/artifacts/` and `work/build/`. The card is per attempt, so the
path is unique and fixed in the prepare tx.

1. **Prepare.** `prepare_worker_lease_tx` branches on the workspace from §3.2. Track takes F8,
   unchanged. Pinned:
   - resolves the repository from `agent_cwd()` (`git_repo_root_for_track_cwd`);
   - probes `git cat-file -e <head>^{commit}` (and `base`) through the bounded `run_git`;
   - skips the clean-tree check and supersede (track-checkout concerns);
   - plans path `…/checkout` with `checkout: Track{branch} | Pinned{head, base, work_dir}`
     replacing `WorkerLeasePlan.branch`.

   The lease INSERT writes `base_sha=head`, `base_source='commit'` and `delivery_policy` NULL.
   TxOutput `cwd` = `work/`. The row and TxOutput are durable before any filesystem effect.
2. **Create and verify.** `verify_worker_checkout` becomes `ensure_worker_checkout`, called at
   the existing spawn point (`codex_adapter/mod.rs:1555`), so every re-drive after a restart
   repeats it. Pinned:
   - registered and present: verify HEAD == head, the realpath, and a detached HEAD;
   - missing: `worktree prune`, then (again) `git -C <repo> worktree add --detach <path> <head>`
     via `isolated_git_command`, then verify;
   - creates `work/artifacts` and `work/build`.

   Objects and the real `origin` are shared, so `gh` needs no `--repo`.
3. **Read-only on start and on resume, with no sandbox change.** The worker runs Codex
   `workspace-write` with cwd = `work/`. The checkout and the common `.git` are readable but not
   writable, and today's mode keeps the network for `gh`. Resume keeps the thread's cwd and
   sandbox as it does for track workers today (F11).
   - Forge actions from a pinned worker are refused before `resolve_forge_cwd` (F17):
     `refused: pinned-checkout-read-only`.
4. **Release (row).** The existing release points flip the row unchanged and insert no delivery
   (F14). `verify_target_identity` maps a pinned lease to a new `UnboundReason::PinnedCheckout`
   (wire enum, regenerated). A pinned lease is not `LegacyLease`.
5. **Gate.** Gate cwd is the lease path, which is the checkout (F5). There is no declaration or
   gate code change. Gate build output inside the checkout is removed with it.
6. **Reclaim (disk).** One sweep in `workspace_lease/pinned.rs`, run at boot after
   `reclaim_dead_workspace_leases_on_boot` and on each scheduler pass. It walks
   `<data_dir>/pinned/`, so finished entries leave the set.
   - It removes `checkout/` and `work/` except `work/artifacts/` when all three hold:
     - the card's lease is `released`;
     - the attempt is terminal (`done`/`failed`/`canceled`);
     - the card has no active worker session (`session_projection_active_for_card_tx`).
   - The removal reuses `remove_workspace_worktree`'s guards; its target gains
     `head: Branch(_) | Detached`.
   - `<track_id>/` is removed whole once the track row is gone (F16; this covers track delete
     and a crash).
7. **Admission is unchanged (D1).** A pinned task is a codex task in flight, and its lease is a
   held lease of the track (F7). Pinned tasks therefore serialize with the writer and with each
   other.

### 3.4 Worker header

`render_task_worker_prompt` (F13) gains a `Workspace:` section, rendered for pinned only.
Track workers keep today's prompt and goldens. It lists:
- checkout (read-only; read its `AGENTS.md`);
- `head`, and `base` (or `not declared`);
- `remote`: `<name> <url>` from `head_upstream(repo_root)` (F18), or
  `none (no upstream remote for <repo>)`;
- work dir (cwd), `artifacts`, `build` (point build outputs there);
- "the gate runs in the checkout; name your report file in calm.task.complete".

### 3.5 Dev template

`templates/builtin/issue-development.md` "Working method" (`:73`) gains one paragraph:
- Each review channel is a `kind: codex` task with
  `workspace: {kind: pinned, head: <PR head>, base: <PR base>}`.
- Its goal names the channel role only: no clone, `--repo`, directory or target-dir commands.
- Verdicts come back in the completion message, which names the artifact file.

The kernel `planner.md` is untouched.

## 4. Failure and diagnostic matrix

| Case | Detected at | Outcome | Diagnostic |
|---|---|---|---|
| Bad shape: abbreviated head, unknown kind, claude, terminal, child route | `validate_task` (declare) | block invalid, never scheduled | `invalid_declaration` with the valid choices (§3.1) |
| Root block changed between claim and prepare | `frozen_root_block_tx` in prepare | `spawn-failed`, no row | `refused: declaration-changed-in-flight: task block <id> changed after claim; declare again under a new key` |
| Unknown head or base | pinned prepare | `spawn-failed`, no row, no directory | `refused: pinned-head-unknown: commit <sha> is not in <repo>; push or fetch it, then declare again under a new key` (`pinned-base-unknown` likewise) |
| Wrong head: HEAD moved, or attached to a branch | `ensure_worker_checkout` (every spawn re-drive) | spawn fails; checkout kept until the sweep | `pinned-checkout-moved: <path> HEAD is <found> (<branch or detached>), expected detached <head>` |
| No repository remote | header render | runs | `remote: none (no upstream remote for <repo>)` |
| Checkout create fails | `ensure_worker_checkout` | spawn fails; compensation releases the row | `refused: pinned-checkout-unavailable: git worktree add failed in <repo>: <stderr>` |
| Re-drive with checkout missing | `ensure_worker_checkout` | recreated at `head`, then verified | the create row's text on failure |
| Cancel or timeout | existing mark, kill, release | row released; disk kept until the stop proof | — |
| Execution not stopped (active session, `stuck` op) | sweep predicate | checkout kept, rechecked each pass | `tracing::warn` once per lease: `pinned checkout kept: worker session still active` |
| Reboot mid-run | boot reclaim, then sweep | attempt `spawn-failed: <BOOT_RECLAIM_REASON>`, no delivery, checkout removed | existing text |
| Track deleted | sweep | `pinned/<track_id>/` removed, registrations pruned | — |
| Forge action from a pinned worker | `transport.rs` before `resolve_forge_cwd` | refused | `refused: pinned-checkout-read-only: a pinned worker cannot run forge actions` |

Permission, candidate, session and merge fences are unchanged. Track tasks take F8 unchanged.
A pinned lease never binds a candidate.

## 5. Composition

- **#1917** (concurrent read-only tasks). Pinned keeps today's serialization (D1). When #1917
  makes admission conflict-based (readers vs writers in the track checkout), a pinned lease is
  classified through the same `lease_workspace_tx` as occupying no track checkout. That lets
  pinned reviewers run beside each other and the writer. The change lives in #1917's predicate
  only.
- **#1921** (`track_read`, draft) moves from `context.neige_workspace.access` to the third
  variant of this field.
  - If it keeps its `access_mode` lease column, pinned rows write `read_only` there.
    `lease_workspace_tx` then reads that column, leaving one source.
  - It maps `track_read` to the Codex `read-only` sandbox, because its cwd is the shared
    checkout.
  - The forge refusal is shared.

## 6. Slice plan (one PR, about 800 lines: ~450 prod + ~350 test)

Contents:
- `calm-types` enum, validator, schema and root-hash registration;
- `frozen_root_block_tx` (moved out of the claim fence);
- `workspace_lease/pinned.rs` (prepare branch, ensure, reclaim; new file, because `mod.rs` is
  at 791 lines);
- the adapter TxOutput and the header;
- the forge fence and target reason;
- the template paragraph;
- the `calm-types/src/event.rs:602` doc, rewritten to "a worker attempt's lease of its checkout".

There is no migration, no `Task` struct change, no Claude change, no sandbox or resume change.

Must-red tests. Each names the production mutation that turns it red; * means mutation-verify:
- T1 `pinned_head_must_be_full_commit_id`: drop the full-hex check.
- T2 `pinned_refused_off_codex_lists_valid_choices`: drop the kind check (claude, terminal and
  child route are all asserted).
- T3 `pinned_prepare_uses_claim_frozen_root`*: skip the frozen-hash compare.
- T4 `pinned_unknown_head_refused_before_any_row`*: skip the `cat-file` probe.
- T5 `pinned_checkout_is_detached_at_head_with_shared_origin`: drop `--detach`.
- T6 `pinned_worker_cwd_is_work_dir_not_checkout`*: write TxOutput `cwd` = checkout. This is
  the read-only invariant.
- T7 `pinned_checkout_moved_fails_spawn_redrive`*: skip the HEAD check on re-drive.
- T8 `pinned_missing_checkout_recreated_on_redrive`: drop the recreate arm.
- T9 `pinned_release_writes_no_delivery_and_gate_unbound_pinned`*: write `delivery_policy='kernel'`.
- T10 `pinned_gate_cwd_is_checkout`: write the lease path = `work/`.
- T11 `pinned_reclaim_waits_for_terminal_attempt_and_stopped_session`*: drop either term (two
  runs).
- T12 `track_delete_removes_pinned_tree_and_registration`: drop the track-gone arm.
- T13 `pinned_worker_forge_action_refused`*: remove the fence.
- T14 `pinned_lease_keeps_track_serialized`: exclude pinned leases from `track_idle`.
- T15 `pinned_header_renders_workspace_facts`: drop the remote line.
- Plus the existing partition tests (F2, F3). `projection_drift_fields_equal_hashed_stored_fields`
  gains `workspace` in its not-stored set.

Gates:
- `scripts/local-ratchet-gates.sh`. Use the route constants, not literal route strings.
- Targeted `cargo nextest` for `calm-types` and `calm-server`. Then the whole `-p calm-server`
  run (new SQL reads meet the source-scan suites), then `scripts/local-rust-gates.sh --quick`.
- Snapshots: `tests/goldens/mcp_tool_registry.json`, `issue_development_planner_prompt.txt`
  (`REGEN_PLANNER_PROMPT_GOLDEN=1`).
- `npm run gen:api` (`UnboundReason`), `fe/core/domain/report.ts` (zod), then
  `(cd fe && npm ci && npm run lint && npm run build && npm test)`.
- Not triggered: migrations, `head_schema_fixture.rs`, column snapshots, `worker_prompt_*`
  goldens (track prompt unchanged), event goldens, `planner.md` caps.

## 7. Acceptance (issue item 4, D5)

On a dev kernel built from the PR branch (not 4140, no 4140 restart), a dev-template track
reviews a real open PR of this repository. Both channels are `kind: codex` pinned tasks on the
same head. The checks:
- (a) The Planner's blocks hold no clone, `--repo`, directory or target commands.
- (b) Each worker's first `gh pr view` without `--repo` succeeds, and its header shows the
  remote.
- (c) Each gate log shows its own checkout as cwd.
- (d) A worker write into the checkout fails, and HEAD equals `head` at the gate.
- (e) The track worktree `git status` and the product diff are unchanged.
- (f) After both finish, `git worktree list` lacks both checkouts and the artifacts remain.
- (g) An unknown head ends `refused: pinned-head-unknown`.

## 8. KNOWN GAPS

- Claude reviewers cannot use `pinned` (D2). Claude workers have no permission boundary (F12).
  Follow-up: a Claude worker sandbox issue, then add claude to `pinned`.
- A head only on a fork or an unfetched remote is refused; there is no kernel fetch.
- The repository `AGENTS.md` is not auto-loaded (cwd is `work/`); the header names it.
- Pinned reviewers stay serialized until #1917.
- A `stuck` worker in the same boot keeps its checkout until reboot (S2's stuck-owner class).
- Artifacts persist until track delete (D3), with no size cap.
- Read-only rests on Codex `workspace-write` semantics (cwd writable, other paths not, network
  on) and on resume keeping cwd. Both are upstream behavior, proven by acceptance (d).

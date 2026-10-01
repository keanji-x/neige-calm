# #1933 — pinned review checkouts

**Owner rules.** (1) Cover the observed pain with the fewest mechanisms; hypotheticals are
one-line KNOWN GAPS. (2) Compatibility is the 4140 database only. (3) A task option applies to
every same-kind path; errors list the valid choices. (4) The kernel owns generic lifecycle and
authorization; the dev template owns review semantics. Nothing infers "review" from goal text.

**Outcome.** A codex or claude task block gains a typed `workspace` field. `track` (the default,
today's behavior) runs in the track checkout. `pinned {head, base?}` runs against a private,
read-only, detached `git worktree` of the track's repository at exactly `head`, with
kernel-allocated work, artifact and build directories, the repository remote rendered as a fact,
and its gate run in that checkout. The checkout is removed once the attempt is over and its
worker has stopped. #1921 later adds `track_read` to the same field.

## 1. Problem and evidence

PR #1927 (track `e0646de4…`, frozen head `d448361f…`, 4140 at `a4a16f14`, 2026-10-01): both
reviewers hand-ran `git clone --shared --no-checkout` + a detached checkout. Their first
`gh pr view` failed because the clone's `origin` was a local path, so both added
`--repo keanji-x/neige-calm`. The Planner hand-assembled base/head, checkout, report dir, target
dir and gate cwd in goal text. Nothing reclaimed `/tmp/neige-1545-pr1927-{a,b}-r1`.

## 2. Facts (origin/main `bcd4dec59`)

| # | Fact | Where |
|---|---|---|
| F1 | Tasks are declared as report `task` blocks. `calm.plan.upsert` is a retired shim that writes nothing; its schema fixture stays frozen | `track_report_blocks/contracts.rs:258-357`; `tools/plan.rs:398-412`; `tests/fixtures/plan_upsert_input_schema.json` |
| F2 | Field vocabulary and validation: `TASK_FIELDS`, `validate_task` (unknown keys rejected); a test pins schema properties == `TASK_FIELDS` | `calm-types/src/report_blocks/kinds.rs:152-171`, `:173`; `contracts.rs:653-664` |
| F3 | Block → `TaskDeclaration` → `tasks` row: `project_task_declarations`, then `project_tasks_tx` (UPSERT) | `report_blocks/tasks.rs:124-147`, `:723`; `track_report/write.rs:309-404`; `calm-truth/src/db/sqlite/task_projection.rs:1412`, `:1611` |
| F4 | Root-hash fields hash only keys present in the payload, so a new key absent from history changes no released hash. Hashed fields must also be drift fields | `calm-types/src/task_recovery.rs:15-48`; `task_context.rs:44`, `:1383-1412`; `task_projection.rs:25` |
| F5 | Agent tasks may not set `gate.cwd`; their gate runs in the worker's lease path (#1727 S4 D3) | `report_blocks/tasks.rs:283-287`, `:579-595` |
| F6 | No CLI command declares tasks. `neige` forwards ls/cat/find/state/…/vacuum only | `mcp_server/cli/commands.rs:81-247` |
| F7 | Admission: `compute_ready` and the claim re-check `track_idle`. It is false while any codex/claude task is in flight or any lease of the track is held | `scheduler/mod.rs:136-151`, `:878`, `:1079`; `calm-truth/src/db/sqlite/track_idle.rs:15-58`; `calm-types/src/task_execution.rs:5` |
| F8 | Worker prepare (both adapters): `prepare_worker_lease_tx` (track checkout, clean-tree check, stuck-lease supersede, HEAD base), then `acquire_workspace_lease_tx` (the only lease INSERT, `delivery_policy='kernel'`, track freeze, `workspace.leased`) | `codex_adapter/mod.rs:762-866`; `claude_adapter/mod.rs:753-876`; `workspace_lease/worker.rs:92-125`; `workspace_lease/mod.rs:137-245` |
| F9 | Spawn verification `verify_worker_checkout` → `verify_worktree_base` requires HEAD == base, the realpath, **and a branch** (a detached HEAD fails). It re-runs when the driver re-enters spawn after a kernel restart | `worker.rs:129-140`; `base.rs:447-487`; `codex_adapter/mod.rs:877`, `:1555`; `claude_adapter/mod.rs:1011`; `operation/driver.rs:350-540` |
| F10 | The worker cwd is TxOutput `cwd` (= lease path); the payload `cwd` is ignored | `codex_adapter/mod.rs:898`, `:1146-1181`; `claude_adapter/mod.rs:904`, `:1048` |
| F11 | The first prompt is a hard-coded `Goal/Context/Acceptance` render with no workspace or repository facts | `codex_adapter/mod.rs:1485-1525`; `claude_adapter/mod.rs:777` |
| F12 | Codex workers: `sandbox_mode "workspace-write"`, `approval "never"`, cwd = lease path. Resume sends only `threadId` + shell-env config (no cwd, no sandbox) | `shared_codex_appserver.rs:1225-1246`, `:3370`, `:3472-3500`; `codex_appserver.rs:754-763` |
| F13 | Claude workers have no permission boundary: `--allow-dangerously-skip-permissions`, and a settings file holding only hooks and attribution. Restart reuses that settings file; with no terminal row its cwd falls back to the track workspace. Only the Claude Planner is sandboxed | `claude_adapter/mod.rs:356-375`; `routes/claude_cards.rs:313-341`; `claude_restart_adapter.rs:150-190`; `claude_planner/spawn.rs:20-75` |
| F14 | Gate cwd: the latest lease `path` of the worker card, in any state. The worker releases before the gate runs. Env is `env_clear` + PATH/HOME/LANG/LC_ALL/TERM + proxy; no sandbox | `task_verify_adapter/mod.rs:441-496`; `gate_process.rs:151-215` |
| F15 | Release: only a `kernel` delivery policy inserts a git delivery. A non-kernel lease is gate-`Unbound{LegacyLease}` | `workspace_lease/release.rs:126-150`; `task_verify_adapter/target.rs:199-227` |
| F16 | The report path releases the lease inside the report tx while the worker is still live; the reaper, timeout and cancel paths release after a stop proof | `decision_sink.rs:186-197`; `reaper/mod.rs:431-469`; `scheduler/mod.rs:91-127`, `:1667-1730` |
| F17 | Boot reclaim is row-only: an older-boot lease whose owner op is not recoverable fails its attempt and is released `interrupted`. The #1815 disk reclaim was deleted in #1830 S2 | `release.rs:193-262`; `operation/driver.rs:351`; `1830-s2-worker-in-track-worktree.md:16`, `:236` |
| F18 | Track delete cascades lease rows; its post-commit sweep removes only the track worktree and candidate refs | `workspace_lease/mod.rs:247-363`; `0115_workspace_lease_upstream_base.sql:34` (`track_id … ON DELETE CASCADE`) |
| F19 | Worker forge actions run in the worker's lease path | `mcp_server/transport.rs:1029-1090` |
| F20 | No typed owner/name repository field exists. Dev publish uses `head_upstream(repo_root).url` as both push URL and `gh --repo` | `workspace_lease/upstream.rs:40-100`; `builtin_plugins/dev/publish.rs:157-200` |
| F21 | The isolated-codex-v1 path (`calm.task.dispatch`, `context.neige_execution`) was deleted in #1893 S4. The `workspace.leased` doc "isolated workspace lease" is stale, and there is no second lease kind to reuse | `71112fccd`; `0128_drop_planner_dispatch_receipts.sql`; `calm-types/src/event.rs:602`; `report_blocks/tasks.rs:752-768` |
| F22 | Review semantics live in the dev template ("two independent channels", convergence); it says nothing about checkouts | `templates/builtin/issue-development.md:73`, `:82-106`; `docs/architecture/1897-builtin-dev.md:7-9` |
| F23 | Typed data root: `Config.data_dir` | `config.rs:26`, `:174-182` |

**4140** (`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`, 2026-10-01): 118 tasks
(`select kind,count(*) from tasks group by kind`: claude 48, codex 66, terminal 4; statuses
done 93, failed 21, canceled 4, so 0 non-terminal); 110 leases, all `released`, 0 held
(`select state,count(*) from workspace_leases group by state`); 1 `stuck` operation; latest
migration 0128. A Python scan of 5443 `track_vcs_objects` found 429 task blocks, 0 with a
top-level `workspace` key and 0 with `context.neige_workspace` (JSON-parse every `blob`, count
`kind == "task"` blocks and their payload keys). The new field defaults to `track`, so no row or report changes
meaning and no data migration is needed.

## 3. Design

### 3.1 Contract (calm-types owns the vocabulary)

```json
"workspace": { "kind": "track" }
"workspace": { "kind": "pinned", "head": "<full sha>", "base": "<full sha>" }
```

- `TaskWorkspace` is a serde-tagged enum in `calm-types` (`kind`; `deny_unknown_fields`). Omitted
  means `{"kind":"track"}`. It is on `TASK_FIELDS`, in the `contracts.rs` property list, in the
  tombstone forbid list, and in `TASK_ROOT_HASH_FIELDS` + `PROJECTION_DRIFT_TASK_FIELDS` (F4).
- `head` is required and `base` is optional. Both are full commit ids (40 or 64 lowercase hex);
  an abbreviation is refused, so the identity is never ambiguous.
- Allowed on codex/claude tasks on the track's own route. It is forbidden on terminal and
  child-track tasks (neither takes a worker lease).
- Errors (`invalid_declaration`, path `payload`) name the valid choices. Example:
  `task review-a: workspace.kind must be one of: track, pinned (got "clone")`.
- #1921 adds `{"kind":"track_read"}` as a third variant here; its
  `context.neige_workspace.access` key is not introduced.
- No CLI mirror is needed: no CLI command declares tasks (F6).
- Persisted as `tasks.workspace_json TEXT NOT NULL DEFAULT '{"kind":"track"}'` (migration 0129),
  read in `prepare_tx` like the other frozen columns. `build_worker_payload` is unchanged and
  stays hash-stable.

### 3.2 Kernel lifecycle (reuses `workspace_leases`, no second lease table)

The same migration adds `workspace_leases.workspace TEXT NOT NULL DEFAULT 'track'
CHECK (workspace IN ('track','pinned'))`. A trigger requires a pinned row to have
`delivery_policy IS NULL` and a non-null base. `WORKSPACE_LEASE_COLUMNS`, `WorkspaceLease` and
calm-truth's `workspace_lease_for_card` gain the column. One value, one meaning: target and
forge code branch on `workspace`, never on a NULL `delivery_policy`.

Layout under `Config.data_dir` (F23): `pinned/<track_id>/<card_id>/` holds `checkout/` (the
detached worktree, read-only to the worker), `work/` (the worker cwd, writable),
`work/artifacts/` and `work/build/`. The card is per attempt, so the path is unique and is
known in the prepare transaction.

1. **Prepare (in the worker op's prepare tx, both adapters).** `prepare_worker_lease_tx`
   branches on the task's `workspace`. Track keeps F8 unchanged. Pinned:
   - resolves the source repository from the track's `agent_cwd()` (`git_repo_root_for_track_cwd`);
   - checks `git cat-file -e <head>^{commit}` (and `base`) through the bounded `run_git`;
   - skips the clean-tree check and stuck-lease supersede, which are about the track checkout;
   - returns a `WorkerLeasePlan` whose path is `…/checkout`, with
     `checkout: Track{branch} | Pinned{head, base, work_dir}` replacing the bare `branch`.

   `acquire_workspace_lease_at_path_tx` writes `workspace='pinned'`, `base_sha=head`,
   `base_source='commit'` and `delivery_policy` NULL. The durable row exists before any
   filesystem effect.
2. **Create and verify (spawn side, idempotent).** `verify_worker_checkout` becomes
   `ensure_worker_checkout`. Codex calls it at `codex_adapter/mod.rs:1555` and Claude at
   `claude_adapter/mod.rs:1011` (F9), so a kernel restart re-runs it. Track: today's
   verification. Pinned:
   - if the checkout is registered and present: verify HEAD == head, the realpath, and that
     HEAD is detached;
   - if it is missing: `worktree prune`, then (again) `git -C <repo> worktree add --detach <path> <head>`
     through `isolated_git_command`, then verify;
   - create `work/artifacts` and `work/build`.

   Because the worktree is created from the repository itself, objects and the real `origin`
   remote are shared, so `gh` resolves the repository with no `--repo`.
3. **Worker permissions, on start and on resume.** The checkout is outside every writable root.
   - **Codex:** `workspace-write` with cwd = `work/` (F12). Today's mode keeps network access for
     `gh`; the checkout and the common `.git` are readable and not writable. Resume
     (`resume_thread_typed`) now sends the thread's frozen `cwd` and `sandbox` for every worker
     thread, track ones included (one path), instead of relying on daemon defaults.
   - **Claude:** the per-card settings file gains the Planner's sandbox block (`enabled`,
     `failIfUnavailable`, `allowUnsandboxedCommands: false`, from `claude_planner/spawn.rs`,
     shared rather than copied) plus `permissions.deny` for `Edit`/`Write` under the checkout.
     Restart reuses the same settings file (F13). The card payload carries `cwd = work/`, so the
     restart fallback no longer picks the track workspace.
   - **Forge actions** from a pinned worker are refused before `resolve_forge_cwd` (F19) with
     `refused: pinned-checkout-read-only`. #1921's `track_read` reuses the same
     `TaskWorkspace::writable()` predicate.
4. **Release (row).** The existing release points (F16, F17) flip the row unchanged. Pinned
   inserts no delivery because its policy is not `kernel` (F15). `verify_target_identity` maps
   `workspace='pinned'` to a new `UnboundReason::PinnedCheckout` (wire enum; regenerate
   `fe/core/api/generated`).
5. **Gate.** Gate cwd is already the latest lease path, which is the checkout (F14). This
   requires no declaration change: agent tasks may not set `gate.cwd` (F5). Before spawning a
   pinned gate, `prepare_tx` verifies HEAD == head. The gate env gains two variables,
   `NEIGE_TASK_BUILD_DIR` and `NEIGE_TASK_ARTIFACTS_DIR`. They are generic paths; the dev
   template, not the kernel, maps them to `CARGO_TARGET_DIR`.
6. **Reclaim (disk).** The checkout must outlive the row release (the gate runs after it, F14),
   so the disk is reclaimed by one sweep, `workspace_lease::pinned::reclaim`. It runs at boot
   after `reclaim_dead_workspace_leases_on_boot` (F17) and on each scheduler pass. It walks the
   directories that exist under `data_dir/pinned/`, so finished entries leave the set:
   - **`checkout/` and `work/build/`** are removed when all three hold:
     - the lease row is `released`;
     - the attempt is terminal (`done`/`failed`/`canceled`, so not `verifying`);
     - the card has no active worker session (`session_projection_active_for_card_tx`, the
       predicate Claude restart uses).

     Removal is `git worktree remove --force` through the shared `remove_workspace_worktree`
     guards (symlink leaf, foreign registration). Its target gains `head: Branch(_) | Detached`.
   - **`<track_id>/`** is removed whole (artifacts included) when the track row is gone. This
     covers track delete (F18: cascaded rows leave no lease to read) and a crash between commit
     and sweep.
   - **`work/artifacts/`** stays until then, because the Planner reads the reports.
7. **Admission is unchanged.** A pinned task is still a codex/claude task in flight, and its
   lease is a held lease of the track (F7). Pinned tasks therefore stay serialized with the
   writer and with each other. Concurrency belongs to #1917 (§5).

### 3.3 Worker header facts

`render_task_worker_prompt` (F11) takes a typed `WorkspaceFacts` built from the plan in both
adapters and renders a `Workspace:` section for every agent task (uniform). For track it shows
the checkout, branch and base. For pinned it shows:
- `checkout` (read-only; read `<checkout>/AGENTS.md` first);
- `head`, and `base` (or `not declared`);
- `remote`: `<name> <url>` from `head_upstream(repo_root)`, the resolution dev publish already
  uses (F20), or `none (<reason>)`;
- `work dir` (cwd), `artifacts dir`, `build dir`;
- `gate`: runs in the checkout with `NEIGE_TASK_BUILD_DIR`.

`calm.plan.list`'s `worktree` facts gain a required `workspace` field and, for pinned, the
`artifacts` path, so the Planner reads reports without guessing.

### 3.4 Dev template guidance

`templates/builtin/issue-development.md` "Working method" (`:73`) gains one paragraph. Each
review channel's task declares `workspace: {kind: pinned, head: <PR head>, base: <PR base>}`.
Its goal names the channel role only, with no clone, `--repo`, directory or target commands.
Gate steps use `CARGO_TARGET_DIR="$NEIGE_TASK_BUILD_DIR"`, and verdicts are read from the
`artifacts` path. The kernel `planner.md` (byte-capped) is not touched.

## 4. Failure and diagnostic matrix

| Case | Detected at | Outcome | Diagnostic |
|---|---|---|---|
| Bad shape: abbreviated or non-hex head, unknown `kind`, terminal or child-route task | `kinds.rs` `validate_task` (declare) | block invalid, never scheduled | `invalid_declaration`: `workspace.head must be a full commit id (40 or 64 lowercase hex)` / `workspace.kind must be one of: track, pinned` / `workspace applies only to codex/claude tasks on the track's own route` |
| Unknown head or base SHA | pinned prepare (`worker.rs`) | `spawn-failed`, no row, no directory | `refused: pinned-head-unknown: commit <sha> is not in <repo>; push or fetch it, then declare again under a new key` (`pinned-base-unknown` likewise) |
| Wrong head: checkout HEAD moved or attached to a branch | `ensure_worker_checkout` (spawn and every re-drive); pinned gate prepare | spawn or gate fails; the checkout is kept for inspection until the sweep | `pinned-checkout-moved: <path> HEAD is <found> (<branch or detached>), expected detached <head>` |
| Missing repository remote | header render | runs; nothing refused | `remote: none (no upstream remote for <repo_root>)` |
| Checkout create fails (disk, git, foreign registration) | `ensure_worker_checkout` | spawn fails; the compensation releases the row | `refused: pinned-checkout-unavailable: git worktree add failed in <repo>: <stderr>` |
| Resume with checkout missing | `ensure_worker_checkout` on re-drive | recreated at `head`, then verified | none if it succeeds, else the create row above |
| Codex daemon resume | `resume_thread_typed` | same cwd (`work/`) and sandbox as start | — |
| Cancel or timeout | existing mark, kill, release (F16) | row released; disk kept until the stop proof | — |
| Execution not stopped (session still active, or op `stuck`) | reclaim predicate | checkout kept; rechecked every pass | `tracing::warn` once per lease: `pinned checkout kept: worker session still active` |
| Reboot mid-run | boot reclaim (F17), then sweep | attempt `spawn-failed: <BOOT_RECLAIM_REASON>`, row released with no delivery, checkout removed | existing reason text |
| Track deleted | sweep (track row gone) | `pinned/<track_id>/` removed, registrations pruned | — |
| Forge action from a pinned worker | `transport.rs` before `resolve_forge_cwd` | refused | `refused: pinned-checkout-read-only: a pinned worker cannot run forge actions` |

Permission, candidate, session and merge fences are unchanged. A track task takes the
unchanged F8 path. Pinned rows never bind a candidate, and the merge path reads only kernel
candidates.

## 5. Composition

- **#1921** (`track_read`, draft). The variant goes on this field, which removes the context key
  (`calm-types/src/workspace_access.rs` on that branch). Its `access_mode` lease column is
  derived from `workspace_leases.workspace` (`track` = write, `track_read`/`pinned` = read)
  instead of being a second column. Forge refusal and resume-sandbox plumbing are shared. It
  maps `track_read` to the Codex `read-only` sandbox because its cwd is the shared checkout;
  pinned keeps `workspace-write` on a private `work/` cwd so artifacts and builds stay writable.
- **#1917** (concurrent read-only tasks). It changes admission (F7) so readers conflict only with
  writers. A pinned lease conflicts with nothing in the track checkout, so #1917 can admit
  pinned tasks with the reader rule. This issue does not change admission (§3.2 step 7).

## 6. Slice plan (one PR, about 1.1k lines with tests)

Contents:
- `calm-types` enum and validator, schema;
- migration 0129;
- `workspace_lease/pinned.rs` (prepare, ensure, reclaim; new file, because `mod.rs` is at 791 lines);
- the adapter branches, the header, Codex resume and Claude settings;
- the forge fence, the target reason, the gate env;
- the template paragraph and the stale `event.rs:602` doc.

A split, only if review forces it: S1 = contract + lifecycle + Codex, S2 = Claude + template.

Must-red tests. Each names the production mutation that turns it red; * means mutation-verify:
- T1 `pinned_head_must_be_full_commit_id`: drop the full-hex check in `validate_task`.
- T2 `workspace_refused_on_terminal_and_child_route`: drop the kind and route check.
- T3 `pinned_unknown_head_refused_before_any_row`*: skip the `cat-file` probe (the row gets
  written; the failure text changes).
- T4 `pinned_checkout_is_detached_at_head_with_shared_origin`: drop `--detach`.
- T5 `pinned_checkout_moved_fails_spawn_redrive`*: skip the HEAD check on re-drive.
- T6 `pinned_missing_checkout_recreated_on_resume`: drop the recreate arm.
- T7 `pinned_release_writes_no_delivery_and_gate_unbound_pinned`*: write `delivery_policy='kernel'`
  for pinned.
- T8 `pinned_gate_runs_in_checkout_with_build_dir_env`: drop the env insert.
- T9 `pinned_reclaim_waits_for_terminal_attempt_and_stopped_session`*: drop either the
  terminal-attempt or the session term (two mutations, two runs).
- T10 `track_delete_removes_pinned_tree_and_registration`: omit the track-gone arm.
- T11 `pinned_worker_forge_action_refused`*: remove the fence.
- T12 `codex_resume_sends_frozen_cwd_and_sandbox`: drop `cwd` from the resume params.
- T13 `claude_pinned_settings_sandboxed_and_restart_uses_work_dir`: restart falls back to the
  track workspace.
- T14 `pinned_lease_keeps_track_serialized`: exclude pinned leases from `track_idle`.
- T15 `worker_header_renders_workspace_facts`: drop the remote line.
- Plus the existing partition tests (F2, F4), which go red if any registry is missed.

Gates:
- `scripts/local-ratchet-gates.sh`. New Rust must use the route constants, not the literal
  route strings (terminology counts).
- Targeted `cargo nextest` for `calm-types`, `calm-truth` (including
  `task_context_migration_tests` `HEAD_TASK_COLUMNS`, `workspace_lease_upstream_migration`,
  `track_write_point_registry`) and `calm-server`. Then the whole `-p calm-server` run (new SQL
  reads meet the source-scan suites), then `scripts/local-rust-gates.sh --quick`.
- Snapshots:
  - `head_schema_fixture.rs` lists 0129;
  - `track_projection_policy_patch.rs` `TASK_PERSISTENT_COLUMNS`;
  - the `Task {..}` literals in about 15 test files;
  - `tests/goldens/mcp_tool_registry.json`;
  - `worker_prompt_{cli,mcp}.txt`;
  - `issue_development_planner_prompt.txt` (`REGEN_PLANNER_PROMPT_GOLDEN=1`).
- `npm run gen:api` (`UnboundReason`) and `fe/core/domain/report.ts` (zod), then
  `(cd fe && npm ci && npm run lint && npm run build && npm test)`.
- Not triggered: event goldens (no new event), the `calm.plan.upsert` fixture (frozen),
  `planner.md` byte caps.

## 7. Acceptance (issue item 4)

Run on a dedicated, non-4140 kernel built from the PR (no 4140 restart). Use a real open PR of
this repository and a dev-template track. Two reviewer channels are declared as pinned tasks on
the same head; channel a is Codex and channel b is Claude, which covers both permission paths.
The checks:
- (a) The Planner's blocks hold no clone, `--repo`, directory or target commands.
- (b) Both workers' first `gh pr view` without `--repo` succeeds, and the header shows the remote.
- (c) Each gate log shows its own checkout as cwd and its own build dir.
- (d) A write attempt into the checkout fails in both providers, and the checkouts' HEADs
  equal `head` at the gate.
- (e) The track worktree `git status` is unchanged, and the product diff is empty.
- (f) After both finish, `git worktree list` lacks the checkouts and the artifacts remain.
- (g) A deliberately unknown head ends `refused: pinned-head-unknown`.

## 8. KNOWN GAPS

- A head that exists only on a fork or unfetched remote is refused; there is no kernel fetch.
- The repository `AGENTS.md` is not auto-loaded (cwd is `work/`); the header names it.
- Pinned reviewers stay serialized (one worker per track) until #1917.
- A `stuck` worker in the same boot keeps its checkout until reboot (S2's stuck-owner class).
- Artifacts persist until track delete; there is no size cap.
- Pinned Claude workers need the Claude sandbox on the host (`failIfUnavailable`), as the
  Planner already does.
- Codex `workspace-write` semantics (cwd writable, other paths read-only, network on) are
  upstream behavior, proven by acceptance (d), not by a unit test.

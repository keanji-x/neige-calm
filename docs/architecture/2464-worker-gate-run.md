# #2464 — gate runs requested by the worker, executed by the kernel, reused by content

**Owner rules.** (1) Simple first: the fewest mechanisms that cover the observed symptoms;
hypotheticals are one-line KNOWN GAPS. (2) Compatibility means the 4140 database only. (3) One
consistent agent-facing surface: `neige.<object>.<action>` ↔ `neige <object> <action>`, options
are schema keys, errors list the valid choices. (4) A safety mechanism only for a hazard this
change introduces; a pre-existing hazard is named and left alone.

**Review tier: L2, both slices.** A new worker tool that makes the kernel commit and run commands
outside the sandbox (authority), persisted run records that later stand in for a gate verdict
(persistence), and gate processes that run while the worker is alive (isolation).

**Outcome.** A gated worker calls `neige_task_gate_run`. The kernel commits the worker's checkout
as the attempt's one commit above the lease base, with the delivery's own script, and runs the
task's declared gate on that commit, in the lease checkout, outside the sandbox. The worker gets the result in the same session, fixes, and calls
again. When the worker reports done and the delivery makes no further commit, the first gate
verdict of the attempt reuses the attempt's last run if that run passed on the same commit and the
remote-tracking refs did not move. Otherwise the gate runs as today. `render_gate_precheck` stops
telling the worker to run the steps in its sandbox.

## 1. Problem (#2459, verified)

The same steps run twice, in two environments:

- **Sandbox false reds.** #2377's worker ran `scripts/local-rust-gates.sh` twice in its sandbox
  (26 and 25 failures: `unshare`/`bwrap` cannot mount `/proc`, read-only Go cache), about 18.7
  minutes, only to conclude "environment failure, the kernel decides". The kernel gate then ran
  the same step green in about 21.4 minutes.
- **Read-only gitdir false greens.** A Codex worker cannot write `.git` (#2058 D7), so `git add
  -N` fails and every check that reads tracked or staged files (`git grep`, `git ls-files`,
  pre-commit, lint-staged) misses new files. #2310's worker built a private index under `/tmp`
  to get around it.

## 2. Facts

Verified at `11c313881` by reading the code, or by the command shown. Probes ran in
`/mnt/data2/kenji/wt/gate-run-2464-tmp/probe/` (a private `codex app-server`, a fake Responses
server and a fake MCP server; no model, no production data).

### 2.1 Q7 first: liveness while a worker waits

| # | Claim | Where | Verified |
|---|---|---|---|
| L1 | A running worker fails as `worker-timeout` when its transcript capture does not advance for the idle window, or when it runs past the cap stamped at `running`. Progress is `MAX(worker_flow_cursors.updated_at_ms)` of the worker card: a message, a tool call or a tool result | `scheduler/worker_liveness.rs:1-4`, `:35-51`; `calm-truth/.../task_liveness.rs:47-66` | read |
| L2 | Idle defaults to 3600 s and may be configured down to 60 s (`CALM_WORKER_IDLE_TIMEOUT_SECS`); the cap defaults to 8 h | `worker_liveness.rs:21-27`; `config.rs:184-194` | read |
| L3 | One tool call records the call when the model emits it and the result when it returns; nothing in between. A call longer than the idle window gets the worker reaped | L1 doc comment; `config.rs:184-187` ("one command records nothing until it returns") | read |
| L4 | The Codex "turn ended without a report" sweep needs the thread at rest with no active turn for 300 s; a turn blocked in a tool call is active | `scheduler/running_worker.rs:24`, `:104-130` | read |
| L5 | Codex 0.159.2 (the 4140 binary) times an MCP `tools/call` out after **300 s** by default and returns `timed out awaiting tools/call after 300s` to the model as the tool output. `[mcp_servers.<k>] tool_timeout_sec` overrides it (5 s probe: failed at 5.0 s) | probe `drive.py long` (1300 s call failed at 300.0 s) and `drive.py tts5b` | command |
| L6 | neige writes `[mcp_servers.neige]` with `command`, `args`, `env` only: no `tool_timeout_sec`, so Codex workers get the 300 s default | `provider/src/codex/shared/home.rs:360-375`; `mcp_server/wiring.rs:57` (only a Planner thread gets per-tool config) | read |
| L7 | A Claude worker has no neige MCP server; it calls the `neige` CLI from its Bash tool. Claude 2.1.280's Bash tool defaults to 120 000 ms and caps at 600 000 ms unless `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS` are set | `claude_adapter/tests.rs:22-38`; `planner_card.rs:10-24`; binary text `var p=120000,d=600000;function X_e…BASH_DEFAULT_TIMEOUT_MS…` | command |
| L8 | Codex starts one MCP server process per thread; the kernel serves one connection's requests one at a time. A blocking call therefore blocks only its own thread's later neige calls | probe `drive2.py` (two threads → two `start` records); `mcp_server/transport.rs:300-340` | command, read |
| L9 | The `neige` CLI client has no request timeout | `crates/neige-cli/src/main.rs:101-152` | read |
| L10 | The shim's comment "codex's default 120 s `tools/call` timeout" is false for 0.159.2 (L5) | `neige-mcp-stdio-shim/src/budget.rs:10` | command |

**Conclusion.** A blocking wait is safe from reaping only while each call returns within the idle
window, and it must return within the provider's tool timeout (300 s Codex, 120 s Claude by
default) or the model sees an error. Today's precheck has the same exposure: one long shell
command records nothing until it returns (L3).

### 2.2 The other six questions

| # | Claim | Where | Verified |
|---|---|---|---|
| K1 | The gate runs only for a `verifying` row. For a candidate-bound agent task it runs in the worker card's lease worktree and `gate.cwd` is refused; a terminal task uses its spawn cwd and an unbound task its declared or track cwd | `task_verify_adapter/mod.rs:461-466`, `:481-526` (`:495`, `:517`); `target.rs:565-574` | read |
| K2 | The target check samples provenance, `HEAD` and `git status --porcelain=v1 --untracked-files=all` (fsmonitor off) before the gate in the prepare tx and after the group is stopped; any difference discards the verdict as `gate-target-mismatch`. It does not see an edit reverted before the after-sample, ignored files, or anything outside the checkout | `target.rs:61-71`, `:353-456`, `:458-471`, `:688-790` | read |
| K3 | The sample must stay under 4 s because it runs inside `BEGIN IMMEDIATE` and auto-commit writers have a 5 s busy timeout | `target.rs:75-85` | read |
| K4 | The delivery script checks provenance, no operation in progress, `HEAD` on `refs/heads/<branch>` and the base object; then `add -A`, commits only when the index differs, and pins a ref. It commits on whatever `HEAD` is, and records `base_is_ancestor` | `calm-types/src/forge_git.rs:54-92` | read |
| K5 | A Codex worker runs `workspace-write`; its gitdir is read-only. A Claude worker is not prevented from committing, so a candidate with more than one commit above the lease base is already a legal shape | `shared_codex_appserver.rs:953`; `codex_adapter/mod.rs:446`; #2058 S1 K1 | read |
| K6 | `delivery.state` is `no_change` only when the candidate equals the lease base; otherwise `committed` | `git_candidate/view.rs:116-128` | read |
| K7 | `tasks.gate_json` is written only while the row is `pending`; a running attempt's gate is frozen | `calm-truth/.../task.rs:37-60` | read |
| K8 | Every delivery is submitted through `submit_delivery` (report path and scheduler path) | `git_candidate/delivery.rs:379-431`; `scheduler/git_delivery.rs:210-230` | read |
| K9 | The checkout is `Busy` while any delivery of another attempt is unsettled | `calm-truth/.../track_occupancy.rs:42-95` | read |
| K10 | Regate (#2405) puts a failed row back to `verifying` and refuses unless every task-verify op of the task is terminal and its marked group is proven stopped | `task_verify_adapter/regate.rs:136-189` | read |
| K11 | The live verdict comes from the wait status; the exit file is read only for dead or reattached work, because a same-user process can forge it | `gate_process.rs:168`; `task_verify_adapter/mod.rs:844-922` | read |
| K12 | The worker prompt is rendered once, in the worker's prepare tx, from the `tasks` row; both adapters share it | `operation/task_prompt.rs:10-28` | read |
| K13 | Worker heads say "Do not `git commit` … the platform commits after you report"; `neige_task_done` takes an optional `commit_message`, validated by `CommitMessage::parse` | `prompts/worker/head-mcp.md:6`, `head-cli.md:6`; `mcp_server/tools/emit.rs:63-104`; `git_candidate/commit_message.rs:19-43` | read |
| K14 | Operation kinds bound to a task row are listed in `TASK_BOUND_ADAPTER_KINDS`; `tests/scheduler.rs:2669-2700` checks every registered kind is classified. `operations.kind` has no CHECK; keyed operation rows are permanent | `operation/mod.rs:74-93`; `0029_operations.sql`; `operations_keyed_rows_permanent_tests.rs` | read |
| K15 | The scorecard counts `gate_red` from `task.gate_result` events with `passed = false` | `scripts/track-scorecard.py:109-111` | read |
| K16 | No gate or regate table exists: regate state lives on the `tasks` row (`gate_attempt`, `gate_result_json`) and in task-verify op rows | `calm-truth/.../task_regate.rs:10-46` | read |
| K17 | The dev template tells the Planner the worker runs the gate steps as its precheck | `templates/builtin/dev.md:113-122` | read |
| K18 | `OperationRuntime::submit` inserts the row and then awaits `drive()`, which holds one global drive mutex while it awaits each adapter's effects; a slow effect delays every caller of `submit` | `operation/driver.rs:130-153`, `:384-395` | read |
| K19 | task-verify releases its held wrapper inside `spawn_side_effect`, before the runtime persists `Parked`; forge-action releases in the observer, after the park commits | `task_verify_adapter/mod.rs:684-757`; `driver.rs:651-665`; `forge_action_adapter/mod.rs:1-2`, `:1339` | read |
| K20 | Boot re-drives `Pending`/`TxCommitted`/`SpawnStarted` ops through `drive_one`, and verifies `Parked` ones. In steady state a parked op whose leader is gone gets `recover_parked(alive = false, PreDeadlineProbe)`; a `Fail` there is claimed and failed unless the observer's completion committed first ("leaving it parked would later be misclassified as a deadline failure") | `driver.rs:1140-1160`; `:928-975` | read |
| K21 | The ownership audit checks every commit in `merge-base..HEAD`: a commit that changes a frozen path (`wire.ts`, `openapi.json`) without its own `OWNERSHIP-CHANGE` trailer is a violation. It runs in `fe` lint (so in this repository's gates) and in CI | `fe/package.json:21`; `fe/tools/ownership/validator.ts:131-146`, `:184-202`, `:306`; `.github/workflows/pr-ownership.yml:63-70` | read |
| K22 | `git for-each-ref --format='%(objectname) %(refname)'` prints the same lines after `refs/remotes/origin/HEAD` is retargeted between two refs at one commit; adding `%(symref)` changes the output | experiment in `gate-run-2464-tmp/symref` (git 2.39.5) | command |

### 2.3 Source-invariant gates that constrain the code

- `TASK_BOUND_ADAPTER_KINDS` and its classification test (K14): the new kind goes in the
  task-bound list, so the stale-context fence applies.
- `tests/cases/deferred_write_tx_invariant.rs`: every new transaction is `begin_immediate_tx`.
- `tests/goldens/mcp_tool_registry.json`, `worker_prompt_mcp.txt`, `worker_prompt_cli.txt`
  (`REGEN_PROMPT_GOLDENS=1`), and the CLI table tests in `mcp_server/cli/commands/tests.rs`
  (spelling is derived from the tool name).
- `fe/core/api/generated/wire.ts` and `openapi.json` (slice 2 adds an evidence variant): frozen
  generated files, so each commit carries one `OWNERSHIP-CHANGE` line per file, preserved in
  the PR body.
- `scripts/local-ratchet-gates.sh` (prose and terminology ratchets) on prompts and docs.
- Not triggered: `gate-sync-event-version-lockstep.sh` (no event kind, no migration),
  `scripts/ci/ratchets/append_seam_boundary.sh` (events are appended only through
  `apply_gate_result_in_tx`, an existing site).
- Size: `task_verify_adapter/target.rs` (1605) and `mod.rs` (1152) are already over 800 lines;
  new code goes into new files, and slice 1's extraction makes `mod.rs` shorter.
- `GIT_DELIVERY_SCRIPT` is part of every persisted delivery payload's hash: slice 1 splits it
  into a shared prefix and tail, and a test pins that their concatenation is byte-identical to
  today's text.

## 3. Decisions

- **D1 Where it runs: in the lease checkout.** The run is the gate's own environment: the same
  directory, the same warm build cache, the same wrapper, and the same K2 after-sample. The
  checkpoint's own checks (D2) stand in for the before-sample.
  - Concurrent edits (Q1): the worker is blocked in the call (L8) and told not to edit during a
    run. An edit that persists until the after-sample discards the result as
    `gate-target-mismatch` (K2). An edit reverted in between is a KNOWN GAP; it needs a worker
    that sabotages its own run.
  - *Rejected: a separate detached checkout.* Its cache is cold (#2377's warm gate took 21
    minutes). Its result would also not describe the checkout where the gate at done runs, so
    reuse would be unsound.
- **D2 The checkpoint: one attempt commit above the lease base (Q2).** The run wrapper's first
  step, numbered 1 and named `neige-checkpoint`, runs the delivery script (K4) with one line
  added: after the provenance, in-progress, branch and base-object checks, and before `add -A`,
  it runs `neige_git reset -q --soft "$4"`, where `$4` is the lease base.
  - Then it commits as the delivery does: when the index differs from the lease base, with the
    call's `commit_message` (else `neige: attempt <id> gate run <n>`). It pins
    `refs/neige/gate-runs/<track>/<card>/<attempt>-r<n>` last.
  - `HEAD` stays on that commit. The steps see exactly what the gate at done sees: `HEAD` is a
    commit with the worker's tree, and the index and worktree are clean.
  - **Why the reset (F1).** The ownership audit judges every commit in the range (K21). Without
    the reset, a run commit whose message lacks a trailer stays in the PR range for good: a
    Codex worker cannot rewrite history. With it, the branch carries at most two commits above
    the lease base: the latest run's commit and, if the worker edited after that run, the
    delivery's. Earlier run commits leave the branch and stay reachable only through their
    `gate-runs` refs.
    - Each remaining message is one the worker wrote. A missing trailer is the same mistake a
      `neige_task_done` message can make today, and it is cheaper to fix: this repository's
      gate runs the audit (K21), so the run is red and the next run replaces the commit.
  - **The parent is the lease base in every case.**
    - (a) Catch-up (#2058): the lease base is `U`, the `HEAD` prepare reset to (#2058 D6
      step 5), so the run commit sits on `U`, as the catch-up delivery's does.
    - (b) A Claude worker that committed on its own (K5): its commits are folded into the run
      commit, under the run's message. The worker heads already say "do not commit".
    - (c) The delivery's checks run before the reset. Exit 10/11/12/15 (wrong checkout, branch
      or in-progress operation) leave `HEAD` where it was. A failure after the reset (`add`, a
      hook, `commit`) leaves `HEAD` at the lease base with the changes staged: no file is lost,
      and the next run or the delivery commits them.
  - **The delivery is unchanged** (its script bytes are pinned, §2.3). It commits on `HEAD`,
    the run commit, only when the worker edited after the last run. With no edit, the
    candidate *is* the run commit (`committed`, K6), and done's `commit_message` is not used:
    the run's message is the commit's message. The prompt says so, and says to pass the full
    message on every run.
  - Candidate, catch-up and regate do not change.
  - The checkpoint runs inside the held wrapper, after the park (D9). No checkpoint git runs in
    the operation drive (K18).
  - *Rejected: temp index + `commit-tree` + private ref, HEAD unmoved.* The steps would still
    read the dirty checkout at the old `HEAD`, so the #2459 false greens stay, and `base...HEAD`
    steps would see no change.
  - *Rejected: move `HEAD` for the run and move it back.* It needs a restore step and its own
    crash recovery; the reset gives one commit per attempt in the common case without either.
- **D3 Waiting: one tool that starts or joins, then waits at most `W` (Q3).**
  `W = min(90 s, idle / 2)`. That fits under Claude's 120 s Bash default (L7), Codex's 300 s
  (L5), and the idle window whatever it is configured to (L2). The prompt renders the actual
  `W`.
  - **The deadline is end to end (F2).** It starts when the handler starts. One `BEGIN
    IMMEDIATE` transaction does the D10 admission and reserves the run by inserting its op row,
    keyed `<attempt>#r<n>`. Two concurrent callers that pick the same `n` hit the same key and
    payload, so the second joins.
  - The handler then starts `runtime.drive()` in a detached task and waits on the completion
    bus, re-reading the op row, until the deadline. It never awaits the drive (K18). Dropping
    the request cancels nothing.
  - The drive itself only kills a recorded group, spawns the held wrapper, records it and
    parks; the slow parts (checkpoint, steps) run after the park (D9).
  - A call returns `running` at `W`; the next call joins the same run.
  - *Rejected: one unbounded blocking call* (provider timeouts L5/L7, reaping L3).
  - *Rejected: separate start and wait tools* (two names for one capability).
  - *Rejected: raising Codex `tool_timeout_sec` and Claude's Bash env* (provider configuration
    for a poll interval).
  - With `W = 90 s`, the typical 4140 gate (143 s) needs two calls and the longest (985 s)
    eleven (§11).
- **D4 Reuse key (Q4)** — §4.
- **D5 Limits (Q5).**
  - At most one unfinished run per attempt: a call joins it.
  - At most `GATE_RUNS_PER_ATTEMPT = 5` admitted runs (O2); the sixth call is refused, and a
    refusal writes no row.
  - A run's timeout is the gate's `timeout_secs`.
  - Per track: one running read-write attempt holds the checkout (K9), and its delivery waits
    for its run (D8), so at most one gate process per checkout, as today.
  - No global limit: there is none today, and the expected load is lower than precheck plus
    gate.
- **D6 Planner view (Q6).**
  - No wake and no event per run: a mid-task red is the worker's iteration.
  - `neige_task_ls` (`candidate.verification`) adds `gate_runs: {used, max, last}`, where
    `last = {run, commit, passed, status_detail, failing_step}`.
  - The verdict of the first gate is `task.gate_result` as today. When it is reused, its
    `target` evidence is `reused` (§4) and its `log_path` / `log_tail` are the run's.
  - Scorecard: `gate_red` keeps its meaning (final verdicts). Two new columns: `runs` (run ops
    of the window) and `reused` (verdicts with `reused` evidence).
  - The dev template's verification plan reads: "The kernel runs the gate when the worker asks;
    a passing run on the unchanged commit is the gate's verdict."
- **D7 4140 and the neighbours (Q7).**
  - Liveness: §2.1 and D3.
  - 4140: no migration, no table, no column. The run record is the op row (`kind =
    'task-gate-run'`, key `<attempt>#r<n>`, result = §5 type), K14. Existing `gate_result_json`
    rows parse unchanged: slice 2 adds an evidence variant, it changes no existing one. Workers
    already running at upgrade keep their frozen prompt (K12).
  - Regate: always runs. Reuse applies only to gate attempt 1 (§4), so a regate never copies a
    run back.
  - **One helper** reads the gate ops of an attempt, both kinds (task-verify `#g…` and
    task-gate-run `#r…`): `attempt_gate_ops(tx, attempt) → Clear | Unfinished(key) |
    Unproven(key)`. `Unproven` means terminal, but the marked group is not proven stopped
    (K10).
    - D8 waits on `Unfinished`.
    - A new run, reuse (§4 (5)) and regate require `Clear`; a call that finds `Unfinished`
      joins that run.
- **D8 Delivery waits for the run.** `submit_delivery` (K8) declines to submit while the
  helper says `Unfinished` for the producer attempt.
  - The report path then leaves the row to the scheduler. The scheduler waits for the run op
    (`runtime.wait`) and submits, inside the spawned per-delivery task. That task holds only the
    `git-delivery:<id>` single-flight key, never the track scheduling lock
    (`scheduler/git_delivery.rs:185-206`).
  - Without this, a `neige_task_done` (or a cancel or liveness end) during a run commits files
    the steps are still writing.
  - The unsettled delivery keeps the checkout `Busy` meanwhile (K9).
  - `Unproven` does not hold the delivery: that is the #2437 class, see §7.
- **D9 Run recovery (F3).** The run releases its wrapper in the observer, after the park
  commits: the forge-action order (K19). Before the park, therefore, no checkpoint and no step
  has run. task-verify keeps its own order: its worker is gone.
  - **Re-drive of `Pending` / `TxCommitted` / `SpawnStarted`** (K20). `spawn_side_effect` first
    kills the op's recorded group, if any, and proves it stopped by marker; otherwise it
    returns `Err` and the op goes `Stuck`. Then it spawns a fresh held wrapper. The old wrapper
    was never released (it exits 75 at stdin EOF), so the checkout is untouched.
  - **`Parked`, any mode, leader gone (`alive = false`).** `Fail` with `gate-infra: the run
    ended without a kernel-observed verdict`. The driver's claim-then-fail path loses to an
    observer that already committed (K20).
  - **`Parked`, `Boot`, leader alive.** `stop_group`, then `Fail` with `gate-infra: the kernel
    restarted during the run`. The checkpoint may have moved `HEAD`; the next run resets it
    (D2).
  - **`Parked`, `PastDeadline`.** `Fail` with `gate timeout (parked deadline exceeded)`. The
    reader maps it to `gate-timeout`, like a live timeout (the deadline is the gate timeout
    plus slack).
  - No run path reads the exit file (K11): the worker is alive and same-user.
- **D10 Who may run.** The caller must be the worker card of `attempt_id`. The attempt must be
  `running` and the current execution of its key, must declare a gate, and must hold a
  kernel-delivery lease. Read-only and legacy-lease tasks are refused: nothing to commit, their
  gate runs after the report (§11: none is gated on 4140). The context must not be stale
  (task-bound kind, K14). The command set is the frozen `gate_json` (K7): the worker chooses
  *when*, never *what*. `prepare_tx` re-checks the row guard; a row that moved between the
  handler and the prepare fails the op (§5 P13).

## 4. Reuse key

At the first task-verify prepare of an attempt, after `prepare_target_tx` froze an unrefused
`Candidate`, the verdict is the reused run's when **all** hold:

1. The gate attempt is 1: no earlier verdict, no regate.
2. Let `R` be the attempt's highest-numbered run op. `R` is `succeeded` with `passed`, and its
   evidence is `verified` with no reasons.
3. `R.commit == candidate.commit_sha`. One commit id fixes the tree, the parent (the lease
   base, D2) and the message. The gate declaration is fixed by the attempt (K7), and the
   checkout by the lease (one per attempt).
4. `R.refs` equals the digest of `git for-each-ref --format='%(objectname) %(refname)
   %(symref)' refs/remotes refs/tags`. `R.refs` is taken by the kernel in `spawn_side_effect`,
   bounded by `SAMPLE_TIMEOUT`; the second digest is taken in this prepare, within the same
   bound (K3). `%(symref)` makes a retargeted `origin/HEAD` visible (K22). If either digest
   cannot be taken, there is no reuse; it is not an error.
5. The D7 helper says `Clear`.

**Why it is sound.** The steps are a function of the commit's content and history (3), of refs
outside the commit (4), and of the environment. Remote-tracking refs and tags are the refs a
gate names as a base (`--base origin/main`). The delivery's own candidate ref, the kernel's
upstream refs and other tracks' branches move without changing what the steps read, so they are
left out. A step that reads a moved ref through a fast-forward-stable form (`merge-base`,
`A...HEAD`) would get the same answer anyway; the digest covers the rest.

**When it must not apply.** In each of these cases the gate runs exactly as today:

- the delivery made a commit (the worker edited after the run);
- the last run is not a pass (a flaky step re-ran red);
- a ref or symref under `refs/remotes` or `refs/tags` moved;
- a regate;
- a run whose processes are not proven stopped;
- a refused or unsampled prepare.

**Typed record.** `VerifyTargetEvidence::Reused { run, cwd, before }`: `run` is the op key
`<attempt>#r<n>`, and `before` is this prepare's sample. `FrozenTarget::Reused` makes the spawn
a no-op, like a refusal. Slice 2 regenerates `wire.ts` and `openapi.json`.

## 5. Producer × state matrix of the run result

`GateRunResult { run, commit: Option<sha>, refs: Option<digest>, verdict: GateVerdict, evidence:
Option<VerifyTargetEvidence> }`. `verdict.status_detail` uses the gate's own vocabulary: `None`
(passed), `gate-red`, `gate-timeout`, `gate-infra`, `gate-target-mismatch`.

**Finalization precedence (F5).** One function, `finalize_run`, in the run module, takes:

- the shared observation from `gate_process::observe_verdict`: exit code, timeout, wait
  failure or signal, plus the started step;
- the `stop_group` outcome;
- an after-sample against the pinned run ref.

The rows of the first table are in precedence order: the first row that matches produces the
result, so every outcome has exactly one producer. P5–P8 apply only after P1–P4 did not match.

| # | Producer (live observer) | passed | gate-red | gate-timeout | gate-infra | gate-target-mismatch |
|---|---|---|---|---|---|---|
| P1 | The group is not proven stopped, whatever the observation | | | | ✓ | |
| P2 | Step 1 (the checkpoint) exited 10/11/12/15 | | | | | ✓ (delivery sentence; no declared step ran; `HEAD` unmoved) |
| P3 | Step 1 exited with any other non-zero code (13, 14, git, hook) | | | | ✓ (no declared step ran; `HEAD` may be at the lease base with the changes staged, or at a new commit) |
| P4 | The run ref is missing or unreadable, or the after-sample fails. This covers a timeout or signal inside the checkpoint, and exit 75 (handshake EOF) | | | | ✓ |
| P5 | The after-sample differs from the run ref or is dirty, whatever the steps did | | | | | ✓ (`verified` with reasons) |
| P6 | Exit 0 | ✓ | | | | |
| P7 | A declared step exited non-zero | | ✓ | | | |
| P8 | Gate timeout | | | ✓ | | |
| P9 | Non-zero with no started step, signal, or wait failure (reachable only if P4 did not match) | | | | ✓ | |

| # | Producer (no live observer) | Result |
|---|---|---|
| P10 | Re-drive before the park (D9) | none: a fresh wrapper replaces the never-released one |
| P11 | `Parked`, leader gone, any mode (D9) | `gate-infra` |
| P12 | `Parked`, `Boot`, leader alive (D9) | `gate-infra` ("the kernel restarted during the run") |
| P13 | `Parked`, `PastDeadline` | `gate-timeout` |
| P14 | `prepare_tx` guard fails, record/release fails, compensation, `Stuck` | `gate-infra` (op `failed` or `stuck`; the reader maps) |

| Reader | May conclude |
|---|---|
| Worker | passed → report done, and change nothing if it wants the verdict reused. red / timeout → fix and run again. infra → run again or report done (the gate decides). target-mismatch → do not edit or switch branches during a run, then run again |
| Planner | Nothing about acceptance: runs are advisory. Only `task.gate_result` judges the attempt |
| Delivery | Only that a run is unfinished (D8) |
| Verify (§4) | Only P6 on the highest run may be reused |

## 6. Tool surface

**`neige_task_gate_run`** (`neige.task.gate_run`; CLI `neige task gate-run`). Worker role,
listed for workers.

- Parameters: `attempt_id` (required), `commit_message` (optional, `CommitMessage::parse`, the
  same rule as `neige_task_done`).
- Result:
  ```json
  {"run": 2, "commit": "<sha>|null", "log_path": "…/<attempt>-r2.log", "runs_used": 2, "runs_max": 5,
   "state": "running", "step": "clippy"}
  {"run": 2, …, "state": "finished", "passed": true}
  {"run": 2, …, "state": "finished", "passed": false, "status_detail": "gate-red",
   "failing_step": "clippy", "exit_code": 101, "log_tail": "…"}
  ```
- `step` is read from the wrapper's step file and is only informational.
- Errors (`invalid_params` / `conflict`, each naming the valid choice):
  - not this caller's running attempt: "a gate run needs the running attempt you were handed";
  - no gate: "task `<key>` declares no gate";
  - read-only or legacy lease: "task `<key>` has no kernel commit; its gate runs after you report
    done";
  - cap: "attempt used 5 of 5 gate runs; report done and the kernel's gate decides";
  - an earlier gate op not proven stopped (`Unproven`): "run `r<k>`'s processes could not be
    proven stopped; report done or fail";
  - stale context and invalid `commit_message` keep their current texts.
- A refusal writes no op row (D3): it neither consumes a run number nor becomes the highest
  run.
- `neige_task_done` / `neige_task_fail` are not refused during a run (D8).
  - Both descriptions gain: "A gate run in progress finishes before the kernel commits."
  - `neige_task_done`'s `commit_message` text gains: "When you changed nothing since your last
    gate run, the kernel makes no commit and that run's message stands."

**Replacement for `render_gate_precheck`** (kernel-delivery gated tasks; `<tool>` is
`neige_task_gate_run` for Codex, `neige task gate-run --attempt-id <id>` for Claude; `<W>` is
the rendered bound, e.g. `90 seconds`):

> This task has a gate: the steps below run in order from the checkout root under /bin/sh,
> outside your sandbox. To run them, call `<tool>` with `commit_message`: the full message of
> the commit (what the repository requires: subject, body, trailers). The kernel makes your
> current changes the one commit of this attempt, with that message, and runs the gate on it
> here. It answers within <W>; while it says `running`, call it again. Do not edit files while a
> run is in progress. Fix what a failing step reports and run again. If the last run passed and
> you change nothing after it, that run is the gate's verdict when you report done, and its
> commit is what the kernel delivers. Otherwise the kernel runs the gate after you report. You
> need not run these steps yourself.

A gated task without a kernel-delivery lease gets only: "This task has a gate: after you report
done, the kernel runs these steps in order from the checkout root under /bin/sh, outside your
sandbox." The step list stays as it is. The heads' "the platform commits after you report"
becomes "the platform commits when you run the gate and after you report".

## 7. Hazards

| Hazard | Introduced? | Mechanism |
|---|---|---|
| Gate steps run while the worker is alive and can edit the checkout | yes | K2 after-sample (P5); prompt rule; edit-and-revert is a KNOWN GAP |
| A delivery commits files a run's steps are still writing | yes | D8 |
| A reused verdict vouches for state the gate did not see at done: a moved ref or symref | yes | §4 (4) |
| …environment drift (toolchain, caches, network, clock) between run and done | no (the same as gate vs CI) | none |
| A regate copies a run back | yes | §4 (1) |
| A worker blocked in a call is reaped as idle, or a provider ends the call | yes | D3 end-to-end `W ≤ min(90 s, idle / 2)`, join |
| A slow checkpoint (hook, filter) holds the operation drive or the call | yes | D2/D9 checkpoint inside the wrapper after the park; D3 detached drive |
| A crash between release and park re-runs the checkpoint over a live group | yes | D9 release after park; re-drive stops the recorded group first |
| A worker forges the exit file that recovery reads | yes (the worker is alive) | D9: no run path reads it |
| A kernel commit with a trailer-less message stays in the PR range (K21) | yes | D2 reset to one attempt commit; the worker's message; the repository's gate shows it |
| A run's processes outlive it and touch the checkout later | no (#2437 class, new producer) | the D7 helper gates the next run, reuse and regate; delivery and occupancy stay as #2437 |
| Gate commands run with no sandbox on the worker's code | no (the gate already does, after done) | the command set stays the frozen `gate_json` (D10) |
| The checkpoint runs repository hooks and filters | no (the delivery already does) | same script, `neige_git` prelude |

## 8. Slices

Both are L2. Slice 1 is useful alone (one environment, no false greens); slice 2 removes the
second run.

### Slice 1: the gate run (about 950 production lines)

- **New op kind** `task-gate-run` (`operation/task_gate_run/{mod,finalize,checkpoint}.rs`),
  listed in `TASK_BOUND_ADAPTER_KINDS`. Phases as task-verify.
- **The tool handler** (D3): the end-to-end deadline, the admission transaction that inserts
  the op row, the detached drive, the bounded wait. Also the CLI row and
  `prompts/tools/neige_task_gate_run.md`.
- **The adapter:**
  - `prepare_tx` re-checks the row guard and freezes the lease identity, the lease base and
    `gate_json`.
  - `spawn_side_effect`: kill and prove-stopped any recorded group, take the refs digest, then
    spawn the held wrapper with `[neige-checkpoint] ++ declared steps`, and park.
  - The observer releases the wrapper, observes, then `stop_group` and `finalize_run` (§5).
  - `recover_parked`: D9. Compensation: kill the group and fail.
- **Shared code:** `render_gate_wrapper`, `spawn_held`, `observe_verdict` (the classification
  of wait status, timeout and started step), `stop_group`, `sample` / `reasons`. task-verify's
  order, sinks and `finalize` stay as they are.
- `forge_git.rs`: `GIT_DELIVERY_SCRIPT` split into a prefix and a tail; a new
  `GIT_RUN_CHECKPOINT_SCRIPT` is prefix + reset line + tail.
- D8 in `submit_delivery`; the D7 helper (regate moves onto it); the `gate-runs` ref prefix in
  the `git_candidate/refs.rs` cleanup.
- The prompt (§6), heads, `neige_task_done.md` / `neige_task_fail.md`, `dev.md` (D6 sentence),
  and L10's comment fixed.

Tests go in `tests/cases/task_gate_run.rs`, with `task_regate.rs`'s fixtures: real git, the
real wrapper, a test-played worker calling the tool through `ToolCallIdentity`, and a
fixtures-only wait bound. R2–R5 and R7–R9 call on a clean tree (an empty change set). Their
assertions do not depend on what the checkpoint commits.

- **R1** `a_gate_run_commits_new_files_before_its_steps_run`: an untracked `new.txt`; the step
  `git ls-files --error-unmatch new.txt && test -z "$(git status --porcelain)"` passes. The
  branch is at the result commit, its parent is the lease base, and it carries the given message.
- **R2** `a_red_step_returns_its_name_and_tail`: `failing_step`, `exit_code`, the tail; the task
  is still `running`; no `task.gate_result` event.
- **R3** `an_edit_during_the_run_discards_its_result`: the step blocks on a fifo, the test writes
  a file, then releases it. Result: `gate-target-mismatch`, reason `dirty`.
- **R4** `a_call_past_the_wait_bound_returns_running_and_the_next_call_joins`: `running` with the
  step name, then the same run `passed`, and one op row.
- **R4b** `a_blocked_checkpoint_still_answers_within_the_wait_bound`: a `.gitattributes` clean
  filter blocks on a fifo, so `add -A` hangs. The call answers `running` within the bound; after
  the release, the run passes.
- **R4c** `a_slow_drive_does_not_hold_the_call`: a fixtures-only delay in `spawn_side_effect`
  longer than the bound. The call answers `running` within the bound, and the run still
  completes after the request is dropped.
- **R5** `the_sixth_run_is_refused_with_the_cap`.
- **R6** `done_during_a_run_delivers_after_the_run`: no delivery op exists while the step blocks.
  After release, the candidate equals the run commit and `#g1` runs.
- **R7** `a_cancel_during_a_run_keeps_the_checkout_busy_until_it_ends`: the next pending task is
  not claimed until the run ends.
- **R8a** `a_restart_before_the_park_reruns_nothing`: seam `crash_point("task-gate-run-pre-park")`
  (`CALM_TEST_CRASH_AT` aborts the kernel process), after the artifacts are recorded and before
  `Parked` is returned. The only step appends a line to a file outside the checkout. After the
  reboot the run completes, and the file holds exactly one line.
- **R8b** `a_restart_after_the_park_ends_the_run_as_infra`: seam
  `crash_point("task-gate-run-post-park")`, at the start of the observer, before the release.
  The held wrapper exits 75 at stdin EOF. After the reboot: `gate-infra` (P11), no step record in
  the log, `HEAD` unmoved, and the next call is run 2.
- **R8c** `a_restart_during_a_step_ends_it_as_infra`: the step blocks on a fifo, and the kernel
  process is killed and rebooted by the harness. The step keeps running across the restart (no
  `kill_on_drop`). At boot the group is stopped and the result is `gate-infra` "kernel
  restarted" (P12), well within the test's bound; the fifo is never released.
- **R9** `a_gate_run_is_refused_for_other_callers_and_states`: Planner identity, another worker,
  read-only, ungated, `verifying`. Each error names the valid choice; no op row is written, and
  the next admitted call is `r1`.
- **R10** `a_checkout_on_another_branch_is_refused_without_a_commit`: `gate-target-mismatch` with
  the exit-11 sentence; `HEAD` unchanged.
- **R11** (adapter unit) `run_recovery_never_reads_the_exit_file`: `recover_parked` with
  `alive = false` and a forged exit file `0` gives `Fail` (infra) for `PreDeadlineProbe` and for
  `Boot`.
- **R12** `runs_keep_one_attempt_commit_above_the_lease_base`: run 1 (message M1, `a.txt`), then
  a self-made commit (Claude style), an edit to `b.txt`, and run 2 (M2). `git rev-list
  base..HEAD` is one commit, with message M2, holding `a.txt` and `b.txt`. Run 1's commit is
  reachable only from its `gate-runs` ref.
- **P1** (`task_prompt.rs` unit, plus goldens): the tool spelling per kind; the rendered `W`;
  "run every step yourself" is gone; the read-only gated variant.
- **L1** (unit) `the_wait_bound_stays_under_the_idle_window`: `W(60 s) = 30 s`,
  `W(3600 s) = 90 s`.

Predicted red sets (each mutation is one production change):

| Mutation | Red |
|---|---|
| MA1 the run wrapper runs the checkpoint after the declared steps instead of before | {R1} |
| MA2 the run's after-sample skipped | {R3} |
| MA3 admission ignores the unfinished run | {R4} |
| MA4 cap check removed | {R5} |
| MA5 D8 guard removed | {R6, R7} |
| MA6 run `recover_parked` on `Boot` with a live leader reattaches like task-verify | {R8c} |
| MA7 `W` ignores the idle window | {L1} |
| MA8 the old precheck text kept | {P1} |
| MA9 the run releases its wrapper before the park (task-verify order) | {R8a} |
| MA10 run `recover_parked` reads the exit file when the leader is gone | {R11} |
| MA11 the reset line removed from the checkpoint script | {R12} |
| MA12 the handler awaits the drive before its wait | {R4c} |
| MA13 a checkpoint (step 1) failure classified as `gate-red` | {R10} |

Gates: `local-ratchet-gates.sh`, `local-contract-gates.sh` (tool registry golden, worker prompt
goldens), `local-rust-gates.sh --quick`, focused `-p calm-server task_gate_run task_regate
scheduler` (the kind classification) and the delivery script tests, and the whole
`-p calm-server` once (new tool and new SQL reads trip source-scan suites).

### Slice 2: reuse at done (about 350 production lines)

- §4 in `task_verify_adapter` (a new `reuse.rs`).
- `FrozenTarget::Reused`; `VerifyTargetEvidence::Reused` in `calm-types`, with `wire.ts` and
  `openapi.json` regenerated.
- `gate_runs` in `git_candidate/view.rs`; scorecard columns.

Tests in the same file:

- **U1** `an_unchanged_candidate_reuses_the_passing_run`: the task is `done`; the evidence is
  `reused` naming `#r1`; no `#g1` wrapper or log was created; `log_path` is the run's.
- **U2** `a_change_after_the_run_runs_the_gate`.
- **U3** `a_red_last_run_is_not_reused`: run 1 passes, run 2 on the same commit is red (a counter
  file).
- **U4** `a_moved_remote_ref_runs_the_gate`.
- **U5** `a_regate_never_reuses`: the ref moves, so `#g1` runs and is red (the step compares
  the ref). The ref moves back; the regate's run spawns a process.
- **U6** `a_retargeted_origin_head_runs_the_gate`: `origin/HEAD` is retargeted between two refs
  at one commit after the run. `#g1` runs.
- **V1** `gate_runs` shown in `neige_task_ls` (`view` unit).

Predicted red sets:

| Mutation | Red |
|---|---|
| MR1 reuse disabled | {U1} |
| MR2 commit equality dropped | {U2} |
| MR3 any passed run instead of the highest | {U3} |
| MR4 refs digest not compared | {U4, U5, U6} |
| MR5 gate-attempt condition dropped | {U5} |
| MR6 `%(symref)` dropped from the digest format | {U6} |

Gates: as slice 1, plus OpenAPI (the shared classifier selects it), `(cd fe && npm ci && npm run
lint && npm run build && npm test)` for the generated type, and the scorecard run against a
fixture database.

## 9. KNOWN GAPS

- An edit made and reverted during a run, or a change to ignored files, is not seen (K2).
- A Claude worker's own commits in the attempt are folded into the run commit under the run's
  message (D2 (b)).
- A non-squash repository shows up to two commits per attempt (the last run's and the
  delivery's).
- A `neige_task_cancel` during a run waits for the run to end before the failed candidate lands
  (D8): up to the gate timeout.
- A local branch other than `HEAD`'s (`git diff main...HEAD` against a local `main`) is outside
  the refs digest.
- If a worker calls the tool while its own worker op is still re-driving spawn, a restart there
  fails `verify_worker_checkout` (`HEAD` moved). The window is the spawn of a worker that is
  already running.
- The step name in a `running` answer comes from a file the worker could write.
- A restart while the checkpoint holds `index.lock` leaves the lock; the next run's checkpoint
  is `gate-infra` until it is removed (the same as #2058's reset gap).
- #2437 stays open for delivery and occupancy.
## 10. Out of scope

- #2459 item 1's typed "worker does not precheck" marker: not needed, because the worker no
  longer runs the steps in its sandbox.
- #2459 item 2's ratchet counting untracked files: the worker side is solved (the run commits
  first), but humans still need `git add -N`. It stays its own change.
- Reuse across attempts, and reuse for a regate.
- A global gate concurrency limit.

## 11. 4140 facts

Queried read-only by the orchestrator on 2026-10-08 (`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`):

| Query | Result | Consequence |
|---|---|---|
| `SELECT kind, access, COUNT(*) FROM tasks WHERE gate_json IS NOT NULL GROUP BY kind, access` | codex/read_write 77, claude/read_write 19, terminal/read_write 4 | No gated read-only task exists; the read-only prompt variant is a code path, not a 4140 shape |
| `SELECT COUNT(*), CAST(AVG(updated_at_ms - created_at_ms)/1000 AS INT), MAX(updated_at_ms - created_at_ms)/1000 FROM operations WHERE kind = 'task-verify' AND phase = 'succeeded'` | 119 ops, avg 143 s, max 985 s | With `W = 90 s` (revision 1) the typical run answers in two calls, the longest in eleven |
| `SELECT gate_attempt, COUNT(*) FROM tasks WHERE gate_json IS NOT NULL GROUP BY gate_attempt` | 0: 9, 1: 91 | No regate has run on 4140; reuse's gate-attempt-1 rule covers every row |
| `SELECT COUNT(*) FROM tasks WHERE json_extract(gate_json, '$.timeout_secs') > 3600` | 34 | A run may outlast the idle window; D3's bounded wait, not the gate timeout, keeps the worker alive |

`CALM_WORKER_IDLE_TIMEOUT_SECS` is not set in the 4140 deploy (`deploy/start.sh`), so idle is the
3600 s default and `W = 90 s`.

## 12. Decided by the orchestrator

- **O1** (moot after revision 1, F1) Run commits stay on the track branch, but the checkpoint
  resets to the lease base first (D2), so an attempt carries at most the last run's commit plus
  the delivery's. Moving `HEAD` back is still rejected (D2).
- **O2** The cap is 5 spawned runs per attempt. 4140 has no attempt that needed a second gate
  verdict (§11), so 5 leaves room for the worker's fix loop without unbounded host load.

## 13. Revision 1 (both review channels: CHANGES)

Each finding was checked against the source before it was patched.

| Item | Resolution |
|---|---|
| F1 per-commit ownership audit | Accepted. K21 is verified (`fe/package.json:21`, `validator.ts:131-146`). D2 now resets `--soft` to the lease base before committing, so an attempt keeps one run commit, plus the delivery's if the worker edited after the run. Done's `commit_message` is unused when the delivery commits nothing (§6). Checked against: (a) catch-up, where the lease base is `U`; (b) Claude self-commits, which are folded in (KNOWN GAP); (c) the delivery checks, which run before the reset and leave the delivery bytes unchanged (§2.3). O1 is moot. Test R12, MA11 |
| F2 submission outside the bound | Accepted. K18 is verified (`driver.rs:130-153`, `:384-395`). D3 has an end-to-end deadline, the admission inserts the op row, and the drive is detached. The checkpoint moved into the wrapper, after the park, so no slow git runs in the drive. Tests R4b (blocked filter in the checkpoint) and R4c (slow drive); MA12 |
| F3a re-drive before the park | Accepted. K19 and K20 are verified. The run releases after the park (forge-action order). A re-drive stops the recorded group, then spawns a fresh, never-released wrapper. D9 is rewritten; P10. Tests R8a (seam `task-gate-run-pre-park`) and R8b (seam `task-gate-run-post-park`); MA9 |
| F3b probe reading the exit file | Accepted, with a different arm than suggested. A dead leader on any mode gives `Fail` infra, not `LeaveParked`: the driver says a dead op left parked is later misread as a deadline (`driver.rs:965-966`), and its claim loses to an observer that committed first. No run path reads the exit file. Test R11; MA10 |
| F4 symref blind spot | Accepted. K22 is reproduced. `%(symref)` is added to the format. Test U6; MR6 |
| F5 matrix not single-valued | Accepted. `finalize_run` precedence is in §5: stopped, then checkpoint, then sample, then observation. Rows are ordered; the signal, wait-failure and no-started-step producers are added (`gate_process.rs:196`, `:386-394`). The observation classification stays shared in `gate_process` |
| F6 MR4 red set | Accepted: MR4 → {U4, U5, U6} |
| N1 refused admissions | Accepted. Admission and the op insert are one transaction; a refusal writes no row (§6, R9) |
| N2 `W` | Accepted: `W = min(90 s, idle / 2)`. The prompt renders it; the Claude timeout instruction and its KNOWN GAP are gone |
| N3 P2 wording | Accepted. P2/P3 now say no declared step ran, and where `HEAD` may be |
| N4 K1 | Accepted. Qualified to candidate-bound agent tasks |
| N5 MA1 | Accepted. MA1 is one change (checkpoint after the steps) → {R1}. R2–R5 and R7–R9 run on a clean tree |
| N6 past-deadline | Accepted. P13 maps to `gate-timeout`, like P8 |
| N7 D8 placement | Accepted. The wait is in the spawned per-delivery task, outside the track scheduling lock. D8 and D7 share `attempt_gate_ops` |
| Cancel waits out the run | Left as a KNOWN GAP, as instructed |


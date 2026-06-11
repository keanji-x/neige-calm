# Plan-then-schedule: task plan table, kernel scheduler, enforced verification gate (issue #644)

Status: design, revision 4 (folds in review channels A+B, rounds 1-3; see
§11). No code in this PR. All file:line citations refer to the tree at the
time of writing (`main`, post-#642/#643).

Splits "deciding what to do" (spec LLM) from "running it" (kernel mechanics):
the spec maintains a durable per-wave task plan via new `calm.plan.*` tools; a
policy-free kernel scheduler dispatches ready tasks through the existing worker
operation saga; a kernel-owned verification gate must pass before a task is
`done`. `calm.task.dispatch` is removed from the spec tool surface (via a
deprecation shim, §4.4).

---

## 1. Current state (what the code actually does)

### 1.1 Dispatch path

- `calm.task.dispatch` is an MCP tool registered in
  `crates/calm-server/src/mcp_server/tools/emit.rs:50,78-185`, visible only to
  `CardRole::Spec` (`emit.rs:105`) and hard-gated by `require_role(...,
  CardRole::Spec)` (`emit.rs:114`). It lowers to
  `Event::CodexWorkerRequested` / `Event::TerminalWorkerRequested`
  (`emit.rs:131-173`) in one eventized write that can also carry a wave
  `lifecycle` transition (`emit.rs:175-184,303-405`).
- The dispatcher background task subscribes to `codex.worker_requested` /
  `terminal.worker_requested` (plus push kinds)
  (`crates/calm-server/src/dispatcher.rs:561-575`), promotes
  `Dispatching → Working` pre-spawn (`dispatcher.rs:788-812,1045-1056`), then
  starts an operation of kind `codex-worker` / `terminal-worker` with
  `OperationKey { operation_key: new_id(), idempotency_key: Some(<spec's key>),
  payload_hash }` and waits on it (`dispatcher.rs:1190-1280`). Spawn failure
  emits `Event::TaskFailed` from `ActorId::KernelDispatcher` and auto-promotes
  `Working → Reviewing` (`dispatcher.rs:849-873,1145-1188`).
- The subscription loop drops missed events on `RecvError::Lagged` with only a
  warning (`dispatcher.rs:599-609`) — the bus is lossy under load. See §5.1
  backstop.
- Global spawn concurrency is one semaphore, `NEIGE_DISPATCHER_PERMITS`,
  default 8 (`dispatcher.rs:68,295-303`). There is **no per-wave budget**
  anywhere today.
- Defense-in-depth: the in-tx role gate refuses dispatch-request events from
  any AI worker actor (`crates/calm-server/src/role_gate.rs:159-189`, issue
  #583).

### 1.2 Worker spawn saga

- Operations table: `crates/calm-server/migrations/0029_operations.sql`.
  Idempotency is the partial unique index `(kind, idempotency_key)`
  (`0029:36-38`). Phases `pending → tx_committed → [app_server_interact] →
  spawn_started → spawn_succeeded → succeeded`, with
  `compensating/failed/stuck` branches
  (`crates/calm-server/src/operation/mod.rs:152-162`).
- `OperationRuntime::submit` dedupes by `(kind, idempotency_key)` and conflicts
  on payload-hash mismatch (`operation/mod.rs:605-630,1134-1198`). Drive claims
  leases (`OPERATION_LEASE_MS = 60_000`, `operation/mod.rs:32,1222-1282`); boot
  recovery re-drives every **non-terminal** operation only
  (`operation/mod.rs:692-724,1284-1300`), invoked from
  `recover_operations_on_boot` (`crates/calm-server/src/lib.rs:140-145`); boot
  order is asserted in `lib.rs` `boot_order_tests`.
- Saga write points that matter for the gate design (§6.2): adapters can
  persist state only in `prepare_tx` / `app_server_interact` (both pre-spawn);
  `spawn_side_effect` receives `&TxOutput` immutably and the runtime's only
  post-spawn writes are `set_phase(SpawnSucceeded)` then `set_phase(Succeeded)`
  (`operation/mod.rs:866-881,895-903`). **There is no post-spawn
  adapter-persistence hook**, and adding one to the saga is not justified for
  one adapter — anything learned at spawn time (a pid) must be persisted
  outside the operation row, by the spawner itself.
- `CodexWorkerAdapter` (kind `codex-worker`,
  `crates/calm-server/src/operation/codex_adapter.rs:664`): `prepare_tx`
  creates the worker card + terminal rows and stamps `idempotency_key`, `goal`,
  `context`, `acceptance_criteria`, rendered prompt into the card payload
  (`codex_adapter.rs:691-775`); spawn opens a thread on the shared codex daemon
  and runs `codex resume <thread> --remote ...` in a PTY
  (`codex_adapter.rs:996-1140`). Compensation cleans worker rows and interrupts
  the shared turn (`codex_adapter.rs:879-979`).
- **Workers do not run in worktrees.** The codex worker cwd is hardcoded
  `default_cwd()` — `$HOME` or the server cwd (`codex_adapter.rs:701`,
  `crates/calm-server/src/routes/codex_cards.rs:196-207`). Terminal workers
  default the same way unless the spec passed `cwd`
  (`crates/calm-server/src/operation/terminal_adapter.rs:182-188`). The only
  per-wave directory is `waves.cwd`, used for the **spec** thread
  (`crates/calm-server/src/model.rs:347-360`, migration 0018,
  `crates/calm-server/src/operation/spec_harness_start_adapter.rs:157,380`).
  See §10 mismatch M1.

### 1.3 Completion path

- Codex workers report via the `neige` CLI (`neige task-completed
  --idempotency-key K` / `task-failed`), which connects to the kernel MCP
  socket using `NEIGE_MCP_SOCKET`/`NEIGE_MCP_TOKEN` and calls
  `calm.task.complete` / `calm.task.fail`
  (`crates/neige-cli/src/main.rs:18-27,160-186`). Those handlers are
  worker-role-gated and emit `Event::TaskCompleted` / `Event::TaskFailed`
  (`emit.rs:191-296`), auto-promoting `Working → Reviewing` in the same tx
  (`emit.rs:450-483`). Both env vars are injected only into **codex** worker
  spawn env (`codex_adapter.rs:1085-1100`).
- **Terminal workers have no completion path.** Their env carries only proxy
  vars (`terminal_adapter.rs:837-849` — no MCP socket/token), and terminal exit
  only persists `terminals.exit_code` + runtime status, emitting no event
  (`crates/calm-server/src/terminal_renderer/attach_reader.rs:40-99`; the exit
  branch holds only an `Option<Arc<dyn RouteRepo>>`, no EventBus —
  `attach_reader.rs:16-25`). Terminals that die while the kernel is down are
  reconciled by a *different* path: the boot supervisor reconcile marks stale
  rows exited with synthetic `-1` (`lib.rs:60-101`). A terminal task only ever
  "completes" today when the spec records `calm.task.verdict`. See §10
  mismatch M2 for the new wiring (both paths).
- `calm.task.verdict` (spec-only,
  `crates/calm-server/src/mcp_server/tools/wave_state.rs:73,162-312`) lowers
  `accepted`/`rejected` to a second `Event::TaskCompleted` /
  `Event::TaskFailed` under the **same** idempotency key, with
  `result.status` distinguishing verdicts from worker self-reports
  (`wave_state.rs:217-248`). Consumers (runs projection, wave-vcs) already
  tolerate multiple task events per key
  (`crates/calm-server/src/wave_fs_view.rs:440-600`).
- Projection actor convention: `is_spec_verdict_event` classifies **any**
  wave-scoped task event from an actor other than `ActorId::KernelDispatcher`
  as a spec verdict (`wave_fs_view.rs:643-645`). Every kernel-emitted task
  event in this design therefore uses `KernelDispatcher` (§6.5).

### 1.4 Observation push + spec turn loop

- The dispatcher's push branch maps `task.completed` / `task.failed` (from
  non-spec actors), user `wave.report_edited`, and worker stop hooks to
  `HarnessObservation`s (`dispatcher.rs:71-96,654-741,1293-1343`), delivered
  to the wave's `SpecHarness` with a per-spec-card monotonic envelope-id
  watermark and per-wave push lock (`dispatcher.rs:876-988`).
- `SpecHarness` queues observations, debounces (hard-fire for task events;
  250ms/5s otherwise, `crates/calm-server/src/harness/config.rs`), and issues
  one codex turn per drained batch
  (`crates/calm-server/src/harness/run_loop.rs:390-456,874-1082`). Turn text
  per observation: `harness/observation.rs:67-91`.
- Boot recovery: harness snapshots are restored from
  `runtimes.handle_state_json` and missed wave events past the snapshot
  watermark are replayed into the pending queue
  (`crates/calm-server/src/harness/mod.rs:30-120`); the replay filter is
  `event_warrants_spec_push_with_role` with no other consultation
  (`harness/mod.rs:100-110`). Recovered harnesses keep their prior thread id
  (`harness/mod.rs:59-67`), and the spec system prompt is injected **only when
  a new thread is minted** — thread reuse skips it
  (`spec_harness_start_adapter.rs:342-352,363-380`). This drives the cutover
  design in §4.4.
- Wave lifecycle: `draft → planning → dispatching → working → reviewing →
  done` with `blocked/canceled/failed` (`model.rs:283-308`); transition table
  and actor authority in
  `crates/calm-server/src/wave_lifecycle.rs:160-262`. Transitions emit
  `Event::WaveLifecycleChanged` (`wave_lifecycle.rs:310-335`,
  `event.rs:387-388`). There is **no "paused" lifecycle state**; `Blocked` and
  the terminal states are the only holds. See §10 mismatch M3.
- Spec prompt (the thing we rewrite):
  `crates/calm-server/src/spec_card.rs:34-213`; worker prompt
  `spec_card.rs:225-265`.

---

## 2. `tasks` table

New migration `crates/calm-server/migrations/0041_tasks.sql` (next free
number after 0040):

```sql
-- Issue #644 — wave-scoped task plan. Source of truth for plan-then-schedule.
CREATE TABLE tasks (
  id              TEXT PRIMARY KEY,           -- "{wave_id}:{key}", see below
  wave_id         TEXT NOT NULL,
  key             TEXT NOT NULL,              -- spec-chosen, stable, short
  kind            TEXT NOT NULL CHECK (kind IN ('codex', 'terminal')),
  goal            TEXT NOT NULL,              -- codex: goal text; terminal: cmd
  context_json    TEXT NOT NULL CHECK (json_valid(context_json)),
  acceptance_criteria TEXT NULL,
  cwd             TEXT NULL,                  -- terminal worker / gate cwd override
  depends_on_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(depends_on_json)),
  priority        INTEGER NOT NULL DEFAULT 0, -- higher schedules first
  gate_json       TEXT NULL CHECK (gate_json IS NULL OR json_valid(gate_json)),
  status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
    'pending', 'dispatched', 'running', 'verifying', 'done', 'failed', 'canceled'
  )),
  status_detail   TEXT NULL,                  -- machine-short reason, e.g. 'gate-red', 'worker-reported', 'spawn-failed'
  worker_card_id  TEXT NULL,                  -- stamped at dispatched→running AND in the report tx (§3)
  gate_result_json TEXT NULL CHECK (gate_result_json IS NULL OR json_valid(gate_result_json)),
  gate_attempt    INTEGER NOT NULL DEFAULT 0, -- attempts *prepared* (bumped in prepare_tx, §6.2)
  gate_pid        INTEGER NULL,               -- pgid of the live gate wrapper (§6.2)
  gate_pid_starttime INTEGER NULL,            -- /proc/<pid>/stat field 22 at spawn; same-boot pid-reuse guard
  gate_pid_boot_id TEXT NULL,                 -- /proc/sys/kernel/random/boot_id at spawn; cross-reboot guard (spec_appserver.rs:145 pattern)
  created_at_ms   INTEGER NOT NULL,
  updated_at_ms   INTEGER NOT NULL,
  finished_at_ms  INTEGER NULL,
  UNIQUE (wave_id, key)
);
CREATE INDEX tasks_wave_status_idx ON tasks(wave_id, status, priority DESC, created_at_ms);

-- Per-wave scheduler budget + gate policy (see §5.3, §6.6). NULL budget =
-- kernel default. Gate policy: DB DEFAULT 1 makes every wave created after
-- this migration default ON without touching the create path — NewWave /
-- wave_create_tx insert a fixed column list (model.rs:386, db/sqlite.rs:675)
-- and never name this column, so the DEFAULT applies; the backfill UPDATE
-- (runs in the same migration, after the ALTER stamps existing rows with 1)
-- resets pre-#644 waves to 0 so in-flight waves keep their behavior.
ALTER TABLE waves ADD COLUMN task_budget INTEGER NULL;
ALTER TABLE waves ADD COLUMN require_task_gates INTEGER NOT NULL DEFAULT 1;
UPDATE waves SET require_task_gates = 0;
```

Notes:

- No FK to `waves` — matches the events-outlive-rows convention
  (`migrations/0004_events.sql` header) and the operations table (no FKs).
- `kind = 'claude'` is **rejected at the tool layer** (out of scope: no
  claude-worker dispatch adapter exists; `ClaudeAdapter` is a card-create
  adapter, `crates/calm-server/src/operation/claude_adapter.rs`, not a worker
  path). The CHECK omits it deliberately so a later migration adds it
  together with the adapter.
- `depends_on_json` is a JSON array of sibling `key` strings (not full ids);
  wave scoping makes that unambiguous and keeps plans human-writable.
- `gate_pid` / `gate_pid_starttime` / `gate_pid_boot_id` are the durable
  gate-process bookkeeping that the operation saga cannot hold (§1.2 write
  points, §6.2). The triple matches the repo's established owned-pid identity
  — `verify_owned_pid` requires `(pid, start_time, boot_id)` to reject
  cross-reboot pid recycle before signaling a group
  (`crates/calm-server/src/spec_appserver.rs:109-171`). Written by the gate
  spawner after fork, **before the wrapper is released** (§6.2 handshake);
  cleared by the waiter tx.

### 2.1 Task id format and the operation idempotency key

- `key`: spec-chosen, `^[a-z0-9][a-z0-9._-]{0,63}$`, unique per wave.
- `id = "{wave_id}:{key}"` — kernel-composed. Wave ids are `new_id()` hex
  (`model.rs`), so `:` cannot collide.
- The scheduler starts the worker operation with
  `OperationKey { operation_key: new_id(), idempotency_key: Some(task.id),
  payload_hash: stable_payload_hash(payload) }` — exactly today's shape
  (`dispatcher.rs:1222-1242`). The global `(kind, idempotency_key)` unique
  index (`0029:36-38`) holds because `wave_id` is embedded in `task.id`.
  Today's spec-supplied keys are only unique by convention; this makes them
  unique by construction.
- The worker payload's `idempotency_key` field (stamped into the card payload,
  `codex_adapter.rs:736-738`; terminal: `terminal_adapter.rs:590-605`) is also
  set to `task.id`, so the **existing completion contract is unchanged**:
  workers run `neige task-completed --idempotency-key <task.id>`, the
  `task.completed` event correlates by that key, and `runs/<task.id>.md|json`
  views resolve by key (`wave_fs_view.rs:130-165`). The runs *projection*
  needs one addition for scheduler-dispatched tasks — see §5.6.
- The verify operation uses kind `task-verify` with
  `idempotency_key = "{task.id}#g{N}"` where `N` is the attempt number
  (`tasks.gate_attempt` semantics in §6.2) — distinct kind plus attempt
  suffix, so a re-verify after boot never collides with a terminal prior
  attempt.

### 2.2 Relation to operations table

`tasks` is the plan; `operations` is execution. A task references its
operations only implicitly through the idempotency-key convention
(`find_by_kind_idempotency`, `operation/mod.rs:1646-1660`); we deliberately do
not store `spawn_op_id` columns, because the operation row is itself looked up
idempotently and the convention survives operation re-submission. (If reviewers
prefer explicit columns, they are additive — but every reader would still need
the by-key lookup as fallback for the crash window in §5.5.)

### 2.3 Migration plan

Additive migration; the only backfill is the one-line
`UPDATE waves SET require_task_gates = 0` above (no tasks exist pre-#644).
Existing waves get `require_task_gates = 0` and `task_budget = NULL` (kernel
default); waves created post-migration get `require_task_gates = 1` from the
column DEFAULT with zero create-path changes. The column ships in PR-A but is
read by nothing until PR-C activates §4.1 rule 6 (rule 8 keeps it vacuous
before then). No
event-log replay concern: replay reconstructs projections from events, and the
new `plan.updated` / `task.dispatched` / `task.gate_result` events (§4, §5.6,
§6.5) are introduced with the feature.

---

## 3. Task status machine

```
pending ──(scheduler claims)──► dispatched ──(spawn op succeeds)──► running
   │                                │                                  │
   │ (spec cancel)                  │ (spawn op fails/stuck)           ├─(worker task-completed, gate declared)──► verifying ──gate green──► done
   ▼                                ▼                                  ├─(worker task-completed, no gate)───────► done
canceled                          failed                               └─(worker task-failed)──────────────────► failed
                                                              verifying ──gate red / timeout / infra──► failed
```

| Transition | Performed by | Where persisted |
|---|---|---|
| (insert) → `pending` | spec via `calm.plan.upsert` | tool tx: tasks row + `plan.updated` event in one eventized write (same pattern as `emit.rs:353-395`) |
| `pending → canceled` | spec via `calm.plan.cancel` | tool tx, guarded `WHERE status='pending'` |
| `pending → dispatched` | scheduler | claim tx (single-winner `UPDATE ... WHERE status='pending'`) + `task.dispatched` event + lifecycle promotion, §5.4 |
| `dispatched → running` | scheduler, on worker operation `Succeeded` (it `wait()`s like the dispatcher does today, `dispatcher.rs:1242`) | **guarded** `UPDATE tasks SET status='running', worker_card_id=<op target_id> WHERE id=? AND status='dispatched'` — a fast worker can report before `wait()` returns; the guard makes the late scheduler write a no-op so it can never regress `verifying/done/failed` back to `running` |
| `dispatched/running → failed` (`status_detail='spawn-failed'`) | scheduler, on operation `Failed/Stuck` | guarded row update (`WHERE status IN ('dispatched','running')`) + kernel `task.failed` event (mirrors `dispatcher.rs:849-873`) |
| `running/dispatched → verifying` (gate declared) | kernel, **inside the `calm.task.complete` tx** (`emit.rs:212-243` extended): same write that persists the worker's `task.completed` flips the row, guarded `WHERE status IN ('dispatched','running')` | emit tx |
| `running/dispatched → done` (no gate) | same place, same guard | emit tx |
| `running/dispatched → failed` (`status_detail='worker-reported'`) | kernel, inside the `calm.task.fail` tx | emit tx |
| `verifying → done` | gate waiter, on wrapper exit 0 | waiter tx: guarded row update + `task.gate_result` event + lifecycle promotion (§6.2, §6.5) |
| `verifying → failed` (`status_detail='gate-red'` / `'gate-timeout'` / `'gate-infra'`) | gate waiter (or compensation / sweep, §8) | same tx shape. **No exceptions** — gate red is failed |

Notes:

- `dispatched → verifying/done/failed` via worker report is allowed (the guard
  includes `'dispatched'`) because a fast worker can report before the
  scheduler's `wait()` returns — the same race the pre-spawn lifecycle
  promotion exists for (`dispatcher.rs:780-812`). The complementary guard on
  the scheduler's `dispatched → running` write (table above) closes the other
  direction of that race.
- **`worker_card_id` survives the fast-report race** by being stamped from
  both sides, idempotently: the scheduler stamps it when its status guard
  wins; the report tx *also* stamps it from the caller — `calm.task.complete`
  / `calm.task.fail` execute under the worker card's per-card MCP identity
  (#567), and the terminal-exit wiring (M2) knows its card. Both use
  `worker_card_id = COALESCE(worker_card_id, ?)`. Whichever tx runs first
  stamps; neither overwrites.
- Doing the worker-report transitions inside the emit tx (not in the
  scheduler's event handler) closes the crash window between "event persisted"
  and "task row updated": the events table has no idempotency-key column
  (`migrations/0004_events.sql`, `0007_events_scope.sql`), so a sweep could not
  cheaply re-derive missed reports. One tx, no window.
- **Lifecycle promotion for gated tasks** (decision; was open question): the
  existing `Working → Reviewing` auto-promotion in the report tx
  (`emit.rs:450-483`) is **suppressed when the tasks row declares a gate** —
  the self-report is a claim, not evidence. The gate waiter tx performs the
  promotion instead, on **any** gate result (green or red: either way there is
  now something to review), via the same `auto_transition_if_current_in_tx`
  (`wave_lifecycle.rs:343`). Exactly one promotion per gated task, at the
  moment evidence exists. Ungated tasks and non-task keys (legacy) keep
  today's behavior. Worker `task.failed` promotes as today (no gate runs on
  failure).
- `calm.task.verdict` is **not** in the status machine. Its duplicate-key
  `task.completed`/`task.failed` emissions (`wave_state.rs:217-248`) must NOT
  flip task rows — the emit-tx hook keys off the calling role
  (verdict = spec actor; the hook only runs in the worker-role-gated
  `calm.task.complete`/`calm.task.fail` handlers). Verdicts stay the semantic
  acceptance layer on top of `done`/`failed`.

### 3.1 Cancel semantics (honest version)

- `calm.plan.cancel` succeeds only from `pending`. Already-`canceled` →
  idempotent success. Any of `dispatched/running/verifying` → error
  `409 task <key> is in-flight; interrupting running tasks is out of scope
  (#644). The worker will finish; its result will be gated/reported as usual.
  Cancel or rewire its successors instead.`
- A `canceled` (or `failed`) task never satisfies a dependency: deps require
  `done`. Successors sit `pending` forever until the spec cancels them or
  upserts replacement deps. The scheduler does not garbage-collect — that is
  plan-revision authority, which belongs to the spec.

---

## 4. `calm.plan.*` tool surface

Registered in a new `crates/calm-server/src/mcp_server/tools/plan.rs`, wired
through `tools/mod.rs:27-33`. All write tools follow the existing contract:
required `message`, optional `lifecycle` applied in the same tx
(`tools/lifecycle_args.rs`), `role_gated_write_annotations()`
(`registry.rs:229-235`), `visible_to_roles: &[CardRole::Spec]`, and
`require_role(identity, CardRole::Spec)`. Wave identity is implicit from the
calling card (`wave_state.rs:323-351`), never a parameter.

### 4.1 `calm.plan.upsert`

```jsonc
// request
{
  "tasks": [
    {
      "key": "impl-parser",                  // required
      "kind": "codex",                       // "codex" | "terminal"
      "goal": "…",                           // required; terminal: the cmd
      "context": { "...": "..." },           // optional, any JSON
      "acceptance_criteria": "…",            // optional
      "cwd": "/abs/path",                    // optional; terminal worker cwd + gate default cwd
      "depends_on": ["write-spec"],          // optional, sibling keys
      "priority": 10,                        // optional, default 0
      "gate": {                              // optional unless wave policy requires (§6.6)
        "cwd": "/abs/repo",                  // optional; default task.cwd, else waves.cwd (§6.4)
        "timeout_secs": 1800,                // optional, default 1800, max 7200
        "steps": [
          { "name": "fmt",    "cmd": "cargo fmt --check" },
          { "name": "clippy", "cmd": "cargo clippy --all-targets -- -D warnings" },
          { "name": "test",   "cmd": "cargo test" }
        ]
      },
      "no_gate_reason": "docs-only change"    // optional; rule 6's escape hatch for an ungated codex task when waves.require_task_gates=1 (PR-C); recorded in context_json
    }
  ],
  "message": "why this plan revision"
}
// response
{ "results": [ { "key": "impl-parser", "outcome": "created" } ] }
// outcome ∈ "created" | "updated" | "unchanged" | error (whole call fails atomically)
```

Validation (whole batch in one immediate tx; any failure rolls back all):

1. `key` regex + per-wave uniqueness (UNIQUE constraint backs it).
2. `kind ∈ {codex, terminal}`; `claude` → explicit "not yet supported" error.
3. Unknown deps: every `depends_on` entry must name an existing wave task or a
   task in the same batch.
4. Cycle detection: DFS over (existing wave tasks ∪ batch), counting edges from
   the post-upsert view. Reject with the cycle path in the error.
5. Mutability: tasks are editable only while `pending` (goal/context/deps/
   priority/gate/cwd all revisable — the plan is living). Upserting a
   non-`pending` key with a byte-identical normalized payload → `unchanged`
   (idempotent retry); with a different payload → error
   (`task <key> already dispatched; insert a new task instead`). This is the
   same payload-hash idempotency rule the operations table uses
   (`operation/mod.rs:612-625`).
6. Gate policy (**PR-C-only** — not implemented in PR-A/PR-B): when
   `waves.require_task_gates = 1`, a `codex` task with no `gate` is rejected
   unless it carries `no_gate_reason` (schema field above; recorded in
   `context_json` — the escape hatch is auditable). Terminal tasks are exempt
   (they are often the glue the gates themselves would run). Before PR-C this
   rule is vacuous anyway: rule 8 rejects every *declared* gate, so enforcing
   rule 6 in PR-A would reject every normal codex task. PR-C activates rule 6
   in the same change that deletes rule 8 (§9).
7. Cwd + gate shape: `task.cwd` and `gate.cwd` are both optional, validated
   absolute when present; the gate's effective cwd resolves via the default
   chain `gate.cwd → task.cwd → waves.cwd` (§6.4 — `waves.cwd` is already
   absolute-validated at `POST /api/waves`, `db/sqlite.rs:705-707` comment).
   Gate: non-empty `steps`, each `cmd` non-empty, no ASCII control chars
   (same check as `codex_adapter.rs:203-209`).
8. **Slice guard (PR-A/PR-B window)**: until the gate runner lands (PR-C),
   `calm.plan.upsert` **rejects any task that declares `gate`** with
   `"gates are not yet enforced (lands with task-verify); resubmit without
   gate or wait for the gate slice"`. This preserves the invariant *a declared
   gate is always enforced* — no task can slip to `done` past a gate the
   kernel silently ignored. PR-C deletes the guard in the same change that
   adds enforcement. (`gate_json` column and rule 7 validation code still land
   in PR-A; only acceptance is deferred.)

Idempotency of upsert: keyed on `(wave_id, key)` + normalized payload equality,
so a re-delivered observation that re-runs the same upsert is a no-op — the
plan replaces the per-dispatch `idempotency_key` discipline the prompt
currently demands (`spec_card.rs:89-93`).

The tx appends `Event::PlanUpdated { wave_id, changed_keys, agent_message }`
(wave-scoped, actor `AiSpec`) — the scheduler trigger and the audit/UI record.
Role gate: `PlanUpdated` joins the spec-only event list alongside the dispatch
events (`role_gate.rs:159-189`).

### 4.2 `calm.plan.cancel`

Request `{ "key": "impl-parser", "message": "…" }` → `{ "ok": true }` or the
409 of §3.1. Emits `PlanUpdated` with `changed_keys: [key]`.

### 4.3 `calm.plan.list`

Read-only (`read_only_annotations()`), spec-visible. Returns all wave tasks:
`{ tasks: [ { key, kind, goal, status, status_detail, depends_on, priority,
gate: {present: bool, steps: [names]}, worker_card_id, gate_result: {passed,
failing_step, log_tail} | null, created_at_ms, finished_at_ms } ] }`. Gate
**commands are not echoed** here and the listing is spec-only — workers must
not see gate bodies (§6.7). A `plan/index.json` wave-fs view for the UI/CLI is
a follow-up (out of scope: UI task list; see §10 M4).

### 4.4 Retiring `calm.task.dispatch` (cutover for persisted spec threads)

Deleting the tool outright strands live specs: recovered harnesses keep their
prior thread id (`harness/mod.rs:59-67`), the start adapter reuses an active
`runtime.thread_id` unless `force_new_thread` is set
(`spec_harness_start_adapter.rs:342-352`), and the system prompt is injected
**only when a new thread is minted** (`spec_harness_start_adapter.rs:363-380`).
A pre-#644 spec thread will keep calling `calm.task.dispatch` per its frozen
prompt (`spec_card.rs:57,89,202`). Forcing new threads on upgrade is rejected:
it discards mid-wave spec conversation state for every active wave. Decision —
**deprecation shim**:

- PR-D replaces the `calm.task.dispatch` handler (and the
  `calm.dispatch_request` alias, `emit.rs:54-61`) with a shim that performs
  **no write** and returns a structured refusal as a *successful* tool
  result. Transport reality: handlers return `Result<Value, RpcError>`
  (`crates/calm-server/src/mcp_server/registry.rs:170-172`) and the transport
  wraps every `Ok` value as a `CallToolResult` with `"isError": false`
  (`crates/calm-server/src/mcp_server/transport.rs:483-497`) — there is no
  handler-level `isError: true` path, and we do not add one: Codex feeds the
  tool result's text content back into the model turn either way, so the
  refusal payload re-educates the persisted spec regardless of the flag.
  Exact shape the shim returns (`Ok(json!(…))`):

  ```jsonc
  {
    "error": "calm.task.dispatch was retired (#644); no task was dispatched",
    "migration": {
      "use": "calm.plan.upsert",
      "shape": "{ tasks: [{ key, kind, goal, depends_on?, priority?, gate? }], message }",
      "notes": "The kernel schedules ready tasks and runs verification gates. Use calm.plan.list to see task status."
    }
  }
  ```

  An `RpcError` shim was rejected: JSON-RPC errors are protocol-level
  failures, and we should not bet a migration path on every client surfacing
  them to the model; the `Ok`-payload shim needs zero transport surgery.
  The shim's descriptor sets `visible_to_roles: &[]` — hidden from **every**
  role's `tools/list` (visibility is role-keyed via `descriptors_for_role`,
  `registry.rs:268-274`; there is no per-thread filtering) while staying
  callable, the same hidden-but-callable pattern as `calm.task.complete`
  (`emit.rs:207-208`) and `register_deprecated_alias` (`registry.rs:283`).
  Old threads with the descriptor cached in context can still call it; new
  threads never list it.
- New spec threads get the rewritten prompt (§7) and never see the tool.
- Shim removal is a later cleanup once all waves created pre-#644 are
  terminal (operational check, not code).
- Keep `Event::CodexWorkerRequested`/`TerminalWorkerRequested` variants in the
  event enum (`crates/calm-server/src/event.rs:618-657`) — old logs must
  deserialize; mark deprecated. The dispatcher's `*.worker_requested`
  subscription arms (`dispatcher.rs:561-575,742-873`) are removed in PR-D
  (nothing emits the events once the shim is in; the operation idempotency
  index made double-dispatch across paths impossible during the
  PR-B/PR-C coexistence window anyway).
- Role gates: section 2.5 of `role_gate.rs` stays (defense for replay/internal
  callers). `spec_card.rs` prompt + its tests asserting
  `p.contains("calm.task.dispatch")` (`spec_card.rs:325,365-375`) flip to
  `calm.plan.*`. `mcp_tools_list_role_filter` test updates accordingly.

---

## 5. Scheduler

New module `crates/calm-server/src/scheduler.rs`, owned by the dispatcher
construction site (it already owns the operation runtime and the event
subscription loop, `dispatcher.rs:520-622`) — same process, same `Weak<OperationRuntime>`
discipline (`dispatcher.rs:632`).

### 5.1 Triggers, subscriptions, liveness

The scheduler rides the dispatcher's existing subscription loop. Exact kinds
added to the filter at `dispatcher.rs:561-575`, per slice:

- **PR-B** adds `"plan.updated"` and `"wave.lifecycle_changed"` — new arms in
  `handle_envelope` that poke the scheduler (they do not enter the push branch
  or the worker-spawn path). The already-subscribed `"task.completed"` /
  `"task.failed"` arms additionally poke the scheduler after the push branch.
- **PR-C** adds `"task.gate_result"` (scheduler poke + push observation §6.5
  + harness replay kind §8).
- `"task.dispatched"` (§5.6) is emitted *by* the scheduler inside its claim tx
  and is not subscribed.

Triggers:

1. `plan.updated` envelope (spec wrote/canceled tasks).
2. Task terminal/report envelopes: `task.completed`, `task.failed`,
   `task.gate_result`.
3. Boot sweep: `scheduler.sweep_all()` after `recover_operations_on_boot`
   (extends the asserted boot order in `lib.rs` `boot_order_tests` — harness
   recovery → supervisor reconcile → runtime orphans → operations →
   scheduler). Sweep arms in §8.
4. Wave lifecycle changes (`wave.lifecycle_changed`): leaving `Blocked`, or a
   user reopen, re-evaluates the wave.
5. **Liveness backstop** (the bus is lossy): the dispatcher drops missed
   events on `RecvError::Lagged` today (`dispatcher.rs:599-609`); a lagged
   `plan.updated`/`task.completed` would strand `pending`/`verifying` tasks
   until restart. Two backstops: (a) on `Lagged`, schedule a `sweep_all()`;
   (b) a slow reconcile tick (`NEIGE_SCHEDULER_RECONCILE_SECS`, default 300)
   runs the same sweep as boot (§8). Honest amendment of the v1 claim: there
   is no *fast-path* periodic tick — readiness latency comes from envelopes —
   but liveness after a lost envelope comes from the periodic sweep.
   Correctness never depends on either: every sweep arm is guarded and
   idempotent (§8), so a sweep racing live handling is a no-op.

Per-wave single-flight: a `DashMap<WaveId, Arc<Mutex<()>>>` exactly like the
push locks (`dispatcher.rs:643-649,885-895`) plus a dirty flag — a trigger
arriving mid-run marks dirty and the runner loops once more.

### 5.2 Ready-set computation (per wave, under the wave lock)

```
running_cost = COUNT(tasks WHERE wave_id=? AND status IN ('dispatched','running','verifying'))
budget       = COALESCE(waves.task_budget, kernel default)
ready        = SELECT * FROM tasks WHERE wave_id=? AND status='pending'
               AND every dep key is 'done'
               ORDER BY priority DESC, created_at_ms ASC, key ASC
take         = ready[.. max(0, budget - running_cost)]
```

Lifecycle gating: schedule only when `waves.lifecycle ∈ {Planning, Dispatching,
Working, Reviewing}`. `Draft` (user hasn't kicked off), `Blocked` (needs user),
and the terminal states (`Done/Canceled/Failed`) hold scheduling. There is no
"paused" lifecycle (§10 M3) — `Blocked` is the pause; the user-driven
`Blocked → Working` edge (`wave_lifecycle.rs:253-255`) resumes, and trigger 4
picks it up. In-flight tasks are unaffected by `Blocked` (no interruption —
out of scope), only new claims stop.

`verifying` counts against the budget deliberately: gates are heavy (cargo
test) and share the checkout (§10 R2).

### 5.3 Budget configuration

`waves.task_budget INTEGER NULL`; kernel default from
`NEIGE_WAVE_TASK_BUDGET` (parsed like `NEIGE_DISPATCHER_PERMITS`,
`dispatcher.rs:295-303`), default **1**. Default 1 is deliberate: workers and
gates share one directory tree today (no worktrees, §10 R2); >1 is opt-in for
waves whose tasks declare disjoint `cwd`s. The global dispatcher semaphore
still caps total cross-wave spawn work.

### 5.4 Dispatch of one ready task

1. Claim tx (one eventized write): `UPDATE tasks SET status='dispatched',
   updated_at_ms=? WHERE id=? AND status='pending'`; `rows_affected == 0` →
   someone else won, skip. Same tx appends `Event::TaskDispatched` (§5.6) and
   the `Dispatching → Working` promotion (`auto_transition_if_current_in_tx`,
   `wave_lifecycle.rs:343`; same pre-spawn ordering rationale as
   `dispatcher.rs:780-812`).
2. Build the payload from the frozen task row —
   `CodexWorkerOperationPayload { actor: ActorId::KernelDispatcher, wave_id,
   idempotency_key: task.id, goal, context, acceptance_criteria }`
   (`codex_adapter.rs:163-172`) or `TerminalWorkerOperationPayload`
   (`terminal_adapter` equivalent, cwd from `task.cwd`). The payload is a pure
   function of the row, so `stable_payload_hash`
   (`crates/calm-server/src/routes/terminal_cards.rs` —
   `stable_payload_hash`, used at `dispatcher.rs:1230`) is deterministic and a
   post-crash resubmit always idempotency-matches.
3. `operation_runtime.start("codex-worker", key, payload)` then `wait()` —
   identical to `dispatcher.rs:1231-1242`. Success → the **guarded**
   `dispatched → running` write + `worker_card_id` stamp (§3 — no-op if a fast
   worker report already advanced the row); failure/stuck →
   `failed('spawn-failed')` (guarded) + kernel `task.failed` event so the spec
   is pushed (§1.4 path unchanged).

Policy-free guarantee: the scheduler never re-runs a `failed` task, never
reorders beyond `(priority, created_at, key)`, never edits the plan, and never
times a worker out. Retry = the spec inserting a new task. The only "judgment"
it holds is the ready-set predicate above.

### 5.5 Race / crash analysis

- **Two triggers concurrently**: per-wave mutex serializes; the claim UPDATE is
  the single-winner primitive; the operations `(kind, idempotency_key)` unique
  index is the final backstop (worst case: duplicate `submit` returns the
  existing op id, `operation/mod.rs:612-617`).
- **Task finishes while scheduler runs**: completion transitions happen in the
  emit tx with status guards (§3), so the scheduler's stale snapshot can only
  under-fill the budget for one pass; the completion envelope itself re-triggers.
- **Fast worker report vs. scheduler success handling**: both sides guarded
  (§3) — the report tx flips only from `dispatched/running`, the scheduler
  flips only from `dispatched`; `worker_card_id` is COALESCE-stamped from both
  sides. No ordering loses data.
- **Crash between claim (`dispatched`) and operation insert**: boot/periodic
  sweep arm (§8) scans `status='dispatched'` rows, looks up
  `(kind='codex-worker'|'terminal-worker', idempotency_key=task.id)`;
  missing → resubmit (deterministic payload, step 2 above); present
  non-terminal → re-drive it (boot: operation recovery,
  `operation/mod.rs:692-724`; steady state: the sweep's single-flight
  `wait()` re-drive — §8 verifying arm 2 mechanism, lease-safe — since no
  background driver exists); present `failed/stuck` → mark task
  `failed('spawn-failed')`; present `succeeded` → mark `running` (and the
  §3 emit-tx guard means a worker report that landed meanwhile already moved
  it on — guard `WHERE status='dispatched'` makes the sweep write a no-op).
- **Crash between operation success and `running` mark**: same sweep arm.
- **Duplicate `task.completed` for one key** (worker retry, or verdict):
  status guards make the flip idempotent; verdict events don't run the hook at
  all (§3).

### 5.6 Dispatch record and the runs projection

The scheduler emits no `*.worker_requested` event, but `project_runs` derives
run status from exactly that event — a key with no requested event renders
`("unknown", None)` (`wave_fs_view.rs:557-562`; same pattern in
`wave_vcs.rs:2147-2300`). Without a fix, every scheduler-dispatched task shows
`status: "unknown"`, `requested_at: null` forever. Decision: **the claim tx
appends a dispatch-record event** (projections stay purely event-sourced,
honoring the replay convention in §2.3, rather than teaching the projection to
join `tasks` rows):

- New `Event::TaskDispatched { idempotency_key: task.id, kind /* codex|terminal */,
  agent_message }`, wave-scoped, actor `ActorId::KernelDispatcher`, kind tag
  `"task.dispatched"`. Kernel-only: the role gate refuses it from any
  card-derived actor (joins the `role_gate.rs:159-189` list).
- `project_runs` (and the `wave_vcs.rs` walker) treat `task.dispatched` as the
  requested-record: `requested_at`, `kind`, and the
  `("requested"|"running")` statuses fall back to it when no
  `*.worker_requested` event exists for the key. ~10-line projection change,
  lands with the scheduler in PR-B plus projection tests.

---

## 6. Verification gate

### 6.1 Declaration

Spec-authored structured steps, stored verbatim in `tasks.gate_json` (§4.1
shape): ordered `steps[{name, cmd}]`, optional `cwd` (absolute when present;
default chain in §6.4), optional `timeout_secs`. Structured steps (not one blob) buy per-step attribution in
logs/observations without inventing a DSL. A blocking remote-CI check is just
a step (`gh run watch ...` / `gh pr checks --watch`) — no special-casing in
the kernel.

### 6.2 `task-verify` operation + gate runner (redesigned)

The v1 design ("operation succeeds at spawn, in-memory waiter, detached
`setsid` child, pid in `tx_output`") was incoherent across restart, as both
review channels found: `tx_output` is only writable pre-spawn — the runtime's
post-spawn writes are just `set_phase(SpawnSucceeded)`/`set_phase(Succeeded)`
(`operation/mod.rs:866-881,895-903`) — so the pid was unknowable anywhere
durable; boot recovery scans only non-terminal operations
(`operation/mod.rs:1284-1300`), so a `succeeded`-at-spawn op is invisible to
it; and a `setsid` orphan **survives** kernel death while its waiter does not,
so a naive re-run races the orphan in one cwd. Redesign — **durable
`(pid, starttime, boot_id)` bookkeeping on the tasks row, a release handshake
that makes the record durable before the gate body can run, kill-then-spawn,
registry-aware sweeps** (direction (a); direction (b), a supervised no-`setsid` child, fails
because `PDEATHSIG` only covers the direct child — `cargo test` grandchildren
outlive it, so a killable process *group* is needed anyway; direction (c), a
saga post-spawn persistence hook, has no existing extension point per the
write-point audit above and is not worth extending the saga for one adapter).

Constraint kept from v1: the operation still `Succeeded`s at spawn, because
operation leases are 60s (`OPERATION_LEASE_MS`, `operation/mod.rs:32`) and a
10-minute `cargo test` inside a phase would outlive its lease and invite
re-claim (`operation/mod.rs:1222-1282`). The saga guarantees at-least-once
*start*; everything after start is owned by durable rows + the waiter/sweeps.

New `ProviderAdapter` kind `task-verify`
(`crates/calm-server/src/operation/task_verify_adapter.rs`), submitted by the
scheduler when it observes the `running/dispatched → verifying` flip (the
emit-tx flip in §3 already happened; scheduler trigger 2 reacts to the
`task.completed` envelope). The submitter computes
`idempotency_key = "{task.id}#g{row.gate_attempt + 1}"` from the current row;
racing submitters compute the same key and dedupe on the operations unique
index.

**Who writes what, when:**

- `validate`: task exists, status `verifying`, gate present.
- `prepare_tx` (attempt `N` parsed from the op's idempotency key): guarded
  bump `UPDATE tasks SET gate_attempt=N WHERE id=? AND gate_attempt=N-1 AND
  status='verifying'`; `rows_affected==0` → fail the op benignly (a different
  attempt won, or the task moved on). Freeze `gate_json` + resolved cwd +
  `N` into `tx_output.data` — the gate that runs is the one recorded.
- `spawn_side_effect` (at-least-once; **idempotent by kill-then-spawn**):
  1. *Kill prior*: read `tasks.gate_pid`/`gate_pid_starttime`/
     `gate_pid_boot_id`; if set, verify with the repo's established owned-pid
     guard — reuse `verify_owned_pid(pid, starttime, boot_id)`
     (`crates/calm-server/src/spec_appserver.rs:145-171`, `pub`; rejects
     cross-reboot pid recycle via boot_id and same-boot recycle via the
     strictly-later starttime, per its own doc comment at
     `spec_appserver.rs:109-144`) — and on match `SIGKILL` the group via
     `signal_process_group` (`spec_appserver.rs:174`, `pub`). Mismatch →
     nothing to kill (pid recycled, rebooted, or never ran); proceed.
  2. *Spawn, held*: one **wrapper script** per attempt, generated from the
     frozen `gate_json` into `<data_dir>/gate-logs/{task_id}-g{N}.sh`, run
     **explicitly as `/bin/sh <path>`** — the repo's shell convention is
     `/bin/sh -c` (`routes/terminal.rs:125-126`) and nothing in `crates/`
     requires bash, so the wrapper is POSIX sh; no shebang/exec-bit reliance —
     as a `setsid` session leader (`tokio::process::Command` + `pre_exec`)
     with **stdin piped from the kernel**. The wrapper's first action is the
     release handshake: `read -r _go || exit 75` — plain POSIX `read -r`, **no
     `-t`** (`read -t` is a bash/ksh/zsh extension; dash rejects it and every
     gate would go `'gate-infra'`); the release timeout is enforced
     kernel-side (steps 3-4 below). It runs **no gate step** until the kernel
     writes the go-token. Kernel death (or abort of the spawning task) before
     release drops the kernel-held `ChildStdin` — the pipe's only write end —
     the `read` sees EOF, returns non-zero, and the held child exits 75
     having executed nothing. Then: run the steps sequentially, echoing
     `::gate-step <name>` sentinels and exiting with the first failing step's
     code. stdout+stderr → `<data_dir>/gate-logs/{task_id}-g{N}.log`. One
     process group = one kill target for wrapper + current step + descendants.
  3. *Persist record*: the spawner (kernel side — the parent knows the pid
     synchronously from spawn) reads `/proc/<pid>/stat` starttime + the
     current boot_id (`read_proc_start_time` / `read_boot_id`,
     `spec_appserver.rs:40,94`, both `pub`) and commits
     `UPDATE tasks SET gate_pid=?, gate_pid_starttime=?, gate_pid_boot_id=?
     WHERE id=? AND status='verifying' AND gate_attempt=N`. This is the
     durable record the saga cannot hold (§1.2). Guard fails (task moved on)
     → kill the held group, fail the op benignly. Steps 3-4 run under a
     **60s kernel-side timeout** (replacing v3's wrapper-side `read -t 60`):
     on expiry, SIGKILL the held group and fail the op `'gate-infra'`.
  4. *Release*: write the **newline-terminated** go-token (`"go\n"` — POSIX
     `read` returns non-zero on EOF-before-newline even when bytes arrived,
     which would trip `|| exit 75` despite a successful release) to the
     child's stdin and close it. Only
     now can a gate command run — so **every gate process that ever executes
     a step is already recorded in the row**; there is no fork-window orphan
     (this closes the v2 residual both round-2 channels flagged: the v2
     wrapper-writes-pidfile scheme left a forked-but-unrecorded duplicate
     that no sweep would ever reap).
  5. *Register waiter*: an in-process tokio task keyed in a
     `DashMap<TaskId, WaiterHandle { attempt, join: JoinHandle }>` registry;
     `insert` overwrites a superseded attempt's entry; return. The runtime
     then advances the op `spawn_succeeded → succeeded`.
     **"Live registered waiter" is defined as: entry present AND
     `entry.attempt == tasks.gate_attempt` AND `!join.is_finished()`**; the
     waiter body runs under a drop-guard that deregisters on completion,
     panic, or abort — and deregistration is **attempt-guarded**:
     `registry.remove_if(&task_id, |_, h| h.attempt == my_attempt)`
     (`DashMap::remove_if`, dashmap 6; already the repo's compare-and-remove
     idiom, `shared_codex_appserver.rs:575`). Without the guard there is an
     ABA delete: attempt N's kill-prior SIGKILLs N-1's group while N-1's
     waiter is still alive; N registers (overwriting N-1's entry); N-1's
     waiter exits later (its tx no-ops under the `gate_attempt=N-1` guard)
     and an unguarded drop-guard would remove **N's live entry** — the next
     sweep would see "no live waiter", arm 3 would kill healthy gate N, and
     N's still-registered-attempt waiter could win its attempt-guarded tx
     with the SIGKILL exit → false red. A dead or attempt-stale handle counts
     as no waiter — so a waiter that dies without its tx leaves the task
     visible to the reconcile sweep (§8 arm 3) instead of skip-forever.
- **Waiter**, on wrapper exit (or after killing the group at `timeout_secs`):
  one tx — guarded flip `UPDATE tasks SET status=<done|failed>,
  status_detail=?, gate_result_json=?, gate_pid=NULL, gate_pid_starttime=NULL,
  gate_pid_boot_id=NULL, finished_at_ms=? WHERE id=? AND status='verifying'
  AND gate_attempt=N` + append `Event::TaskGateResult` (§6.5) + the lifecycle
  promotion (§3 note). The attempt guard means a superseded waiter (its
  attempt was killed and re-run) writes nothing. A wrapper exit with no
  `::gate-step` sentinel seen (e.g. the handshake `read` hit EOF, exit 75)
  is `'gate-infra'`. (A kernel-side release-timeout kill fails the op at
  steps 3-4 before any waiter exists — compensation / arm 4 cover it.)
  The drop-guard's attempt-guarded remove (step 5) runs on every exit path.
- `plan_compensation` / `compensate_step`: kill the recorded process group
  (`verify_owned_pid`-guarded), mark task `failed('gate-infra')` if still
  `verifying` at attempt `N`.

**Single-runner invariant** (no concurrent duplicate gates in one cwd):
(i) at most one non-terminal `task-verify` op per task — submissions key off
`row.gate_attempt + 1`, `prepare_tx`'s guarded bump admits exactly one op per
attempt number, and the sweep never mints a new attempt while a non-terminal
op **or an attempt-matched live waiter** exists — it re-drives the existing
op instead (§8 arms 1-2); (ii) every gate process that can execute a
step is recorded in the row before release (handshake, step 4), and every
spawn kills the recorded predecessor before starting; (iii) the
`(pid, starttime, boot_id)` `verify_owned_pid` match prevents pid reuse —
same-boot or cross-reboot — from killing an innocent process. Gates remain
**at-least-once** (risk R1) but never concurrent per task.

The gate process is not a terminal card and not under the proc-supervisor: it
is invisible to workers and to the UI by construction; only its log file and
result events surface.

### 6.3 Execution semantics

- Env: minimal kernel env + proxy settings (like `terminal_worker_env`,
  `terminal_adapter.rs:837-849`). **No `NEIGE_MCP_TOKEN`/`NEIGE_MCP_SOCKET`** —
  the gate cannot write kernel state.
- Timeout: waiter kills the process group at `timeout_secs` (default 1800,
  cap 7200) → red, `status_detail='gate-timeout'`.
- Exit semantics: wrapper exit 0 → green; non-zero → red with `failing_step`
  (from the last `::gate-step` sentinel) and `exit_code`; spawn/IO errors →
  red `'gate-infra'` (still `failed` — "gate didn't prove green" is the
  invariant; the spec can re-plan).
- Log capture: full log on disk; last 8 KiB around the failing step (or final
  step on green) into `gate_result_json` and the result event. Logs are
  advisory, not verdict inputs — see §6.7.

### 6.4 Where the gate runs (no worktrees exist)

The issue says "in the worker's worktree"; the code has no worker worktrees —
codex workers run in `$HOME` (`codex_adapter.rs:701`) and edit whatever path
their goal text told them to (§1.2). Therefore the **spec controls the gate
cwd** — it is the only party that knows which checkout the worker was told to
touch. One rule, stated once (§4.1 rule 7 matches): `gate.cwd` is **optional**;
the effective cwd is `gate.cwd → task.cwd → waves.cwd` (`model.rs:347-360`),
each validated absolute where it is written. The resolved path must exist at
run time, else red `'gate-infra'`. When a
later issue introduces per-task worktrees, `gate.cwd` becomes derivable;
nothing else here changes.

### 6.5 Result events and spec observation

The waiter tx (§6.2) appends `Event::TaskGateResult { task_id,
idempotency_key: task.id, passed, failing_step, exit_code, log_tail, log_path,
attempt }` (wave-scoped), using the same in-tx append helper operations use
(`events_append_for_operation_tx`, `crates/calm-server/src/db/sqlite.rs:284`).

**Actor: `ActorId::KernelDispatcher`** — for this and for every
kernel-emitted task event in this design (gate results, terminal-exit
completions §10 M2, spawn failures). Rationale: `is_spec_verdict_event`
classifies any wave-scoped task event from a non-`KernelDispatcher` actor as a
spec verdict (`wave_fs_view.rs:643-645`); a terminal-exit `task.completed`
with `ActorId::Kernel` would be recorded as a verdict whose `result.status`
parse fails, and no completion would ever render. `task.gate_result` is a new
kind the classifier doesn't currently touch, but uniformity costs nothing and
survives classifier widening. The classifier itself stays unchanged.

Push rewiring (`dispatcher.rs:71-96` + `harness_observation_from_event`,
`dispatcher.rs:1293-1343`):

- `task.gate_result` → new `Observation::TaskGateResult` (hard-fire), turn
  text: `Task <key> gate {passed|FAILED at step <name> (exit <code>)}. Log
  tail:\n<tail>\nRead the full log at plan/<key>/gate.log; read the worker
  output at runs/<task.id>.md.`
- **Gated self-report suppression**: a worker `task.completed` whose
  idempotency key resolves to a tasks row **with `gate_json` set** is not
  pushed — the spec hears the gate, not the self-report. The predicate is
  *"task has a gate"*, deliberately **not** "gated and currently `verifying`":
  a fast gate can flip the row to `done` before the push branch reads it, and
  the status-based predicate would then push both the self-report and the gate
  result. Ungated tasks and non-task keys (legacy) push as today.
- The **boot-replay path gets the same consultation**: `harness/mod.rs:100-110`
  replays `task.completed` through `event_warrants_spec_push_with_role` with
  no tasks lookup today, so a crash between the emit tx and the live push
  would replay the raw self-report to the spec. PR-C threads the tasks-row
  lookup (by idempotency key, gate predicate above) into both the live push
  branch and `replay_harness_events_since`; `task.gate_result` joins the
  replayed kinds list at `harness/mod.rs:88-96`.
- Worker `task.failed` pushes as today (no gate runs on failure).
- Gate logs readable by the spec via a new wave-fs view `plan/<key>/gate.log`
  (file-backed read in `wave_fs_view.rs`, spec-role only — `neige` calls carry
  card identity, so the view can role-gate like `descriptors_for_role`,
  `registry.rs:268-274`).

`calm.task.verdict` stays untouched as the semantic layer: spec may still
reject a gate-green `done` (its `task.failed` emission feeds the runs
projection and audit as today, `wave_state.rs:217-248`), and that rejection is
plan input (insert a remediation task), not a status-machine edge.

### 6.6 Wave-level gate policy

`waves.require_task_gates = 1` (default for **new** waves via the column's DB
DEFAULT — the create path's fixed INSERT list never names the column, so no
`NewWave`/`wave_create_tx` change, §2; existing rows are backfilled to 0 in
the same migration) makes `calm.plan.upsert` reject codex tasks
without a gate or a `no_gate_reason` (§4.1 rule 6). Enforced at plan-write
time — the scheduler stays policy-free. Rule 6 is PR-C-only: rule 8 rejects
all declared gates before that, so PR-C activates rule 6 in the same change
that deletes rule 8 (§4.1, §9).

### 6.7 Trust model (honest)

Gates are spec-authored shell executed by the kernel user with the kernel's
privileges, no sandbox. This is the **same trust level the spec already has**:
`calm.task.dispatch kind=terminal` runs arbitrary `cmd` as a kernel-spawned
PTY today (`emit.rs:151-167`, `terminal_adapter.rs:563+`). What the gate adds:

- Workers cannot read or edit the gate definition (kernel-side storage; not in
  card payloads; `plan/` list omits commands for non-spec roles).
- Workers **can** still game the *tree being gated*: they edit the same
  checkout the gate runs in and could alter tests, add a `build.rs`, or wrap
  the toolchain. The gate proves "this tree passes these commands", not "the
  worker was honest". Defense against adversarial workers requires worktree /
  sandbox isolation — explicitly out of scope here and stated as such.
- **Gate logs are worker-reachable.** `data_dir` defaults under `$HOME`
  (`config.rs:152-159` — `XDG_DATA_HOME` else `~/.local/share`, joined
  `neige-calm`) and the codex worker sandbox is `workspace-write` with
  `cwd = $HOME` (`codex_adapter.rs:1024`, `codex_cards.rs:196-207`), so
  `<data_dir>/gate-logs` is generally inside the worker-writable tree. The v1
  claim that workers "cannot reach gate-logs" was wrong. Honest statement:
  **verdict integrity rests on the wrapper's exit status**, observed by the
  kernel-owned waiter over the process handle — a worker cannot forge that.
  The log file and its pushed tail are advisory and tamper-able by a
  `$HOME`-sandboxed worker (the kill bookkeeping lives in the tasks row, not
  in a worker-reachable pidfile — v2's pidfile is gone, §6.2). Deployments
  that care set `data_dir` outside
  `$HOME` (`config.rs:26`); we do not move the default. A hostile same-user
  worker could also signal the gate process — same out-of-scope isolation
  story as above.

---

## 7. Spec prompt rewrite (direction)

`SPEC_SYSTEM_PROMPT_TEMPLATE` (`spec_card.rs:34-213`) changes:

- Replace the "Dispatch sub-jobs via `calm.task.dispatch`" turn step
  (`spec_card.rs:88-96`) with: **maintain the plan** — `calm.plan.upsert` to
  add/revise pending tasks (deps, priority, gates), `calm.plan.cancel` to drop
  pending ones, then **END YOUR TURN** (the existing turn-reactive contract,
  `spec_card.rs:100-105`, is unchanged). The kernel schedules; the spec never
  waits for spawns.
- New observation vocabulary: turns now begin with the wave goal, a
  `task.gate_result` (with log tail), an ungated task completion, a task
  failure (worker-reported or spawn), or a user report edit / user message.
  Document that gate results are machine facts, not worker claims; remediation
  = insert a new task with a new key (retry policy is yours), and
  `calm.task.verdict` remains for semantic accept/reject on top.
- Gates: "every codex task should declare a gate (fmt/clippy/tests as
  appropriate); waves with `require_task_gates` will reject ungated code
  tasks. You know the checkout path — gates default to the task cwd, then the
  wave cwd; set `gate.cwd` when the worker's checkout differs (§6.4 chain).
  Gates may run more than once (kernel restarts re-run them) — declare only
  re-runnable commands."
- Drop the per-dispatch `idempotency_key` guidance; task `key`s carry that
  role. Reads gain `neige cat plan/...`.
- `spec_card.rs` tests (`spec_card.rs:303-435`) updated in the same slice.
- Persisted pre-#644 spec threads never see this prompt (§1.4); the §4.4 shim
  is their migration path.

Worker prompt: unchanged except one honest line — "your completion report is a
claim; a kernel gate may verify it before the task counts as done."

---

## 8. Crash / recovery end-to-end

Boot order: harness recovery → supervisor reconcile → runtime orphans →
operation recovery → **scheduler sweep** (extending `lib.rs:140-145` +
`boot_order_tests`). One shared sweep body runs at boot, on the periodic
reconcile tick, and after `Lagged` (§5.1); the in-process **waiter registry**
(§6.2) makes every one of them safe — the live-waiter skip is part of the
shared body, **including boot**. That is load-bearing at boot, not vacuous:
operation recovery runs *before* the sweep and is synchronous
apply-then-drive (`operation/mod.rs:702-724`), so a `task-verify` op that was
non-terminal at crash has already been fully re-driven — its
`spawn_side_effect` registered a fresh waiter and the op advanced to
`succeeded` (`operation/mod.rs:866-903`) — by the time the sweep reads it.
Without the skip, the boot sweep's arm 3 would see "op succeeded, row
`verifying`", kill that freshly re-driven healthy gate, and the killed gate's
waiter could still win its attempt-guarded tx (the guard holds until the
replacement's `prepare_tx` bumps `gate_attempt`) → false red. With the skip,
a live registered waiter (§6.2's liveness definition: entry present,
`entry.attempt == row.gate_attempt`, join handle not finished) means the gate
is owned in-process and the sweep leaves it alone; a dead handle deregisters
(attempt-guarded `remove_if`, §6.2 step 5 — a superseded waiter's late exit
cannot delete the live attempt's entry), so a waiter that panicked/aborted
leaves the task visible to the next tick rather than skip-forever.

Per task status:

- `pending`: nothing in flight; sweep recomputes ready sets and dispatches.
- `dispatched`: §5.5 sweep arm — operation missing → resubmit (idempotent);
  non-terminal → re-driven (boot: operation recovery,
  `operation/mod.rs:692-724`, including codex `SpawnStarted` replay and the
  already-exited preservation path, `codex_adapter.rs:805-830`; steady state:
  the sweep's single-flight `wait()` re-drive, same mechanism as the
  verifying arm 2 below — no background driver exists for this kind either);
  terminal outcome → task row reconciled (guarded writes, §5.5).
- `running`, kind = codex: worker survives restarts (PTY under
  proc-supervisor; supervisor reconcile marks dead ones exited,
  `lib.rs:60-101`). The worker's eventual `neige task-completed` flips the row
  in the emit tx — no scheduler state needed. A worker that died silently
  leaves the task `running`; that surfaces in `calm.plan.list`/report and is
  the spec's (or user's) call — the scheduler holds no liveness judgment
  (policy-free). Listed as risk R4.
- `running`, kind = terminal: **mechanically reconcilable, so the sweep does
  it.** The boot supervisor reconcile has already marked dead PTYs exited via
  `terminal_set_exit` (synthetic `-1`, `lib.rs:60-101`) before the sweep runs
  (boot order above). The sweep finds `running` terminal tasks whose terminal
  row has `exit_code` set (or runtime non-running) and runs the **same**
  guarded completion tx as the live exit path (§10 M2): `task.completed`
  (exit 0) / `task.failed` (non-zero/signal/`-1`), actor `KernelDispatcher`.
  Live/sweep duplication is impossible — both run the `WHERE status IN
  ('dispatched','running')` flip; first wins, second no-ops. The periodic
  sweep runs the same arm, covering a terminal exit whose live event was lost
  to a lagged bus.
- `verifying` (the redesigned arm — operation recovery and the sweep can no
  longer both start a gate):
  1. **Live registered waiter for this task** (§6.2 liveness definition:
     entry present, `entry.attempt == row.gate_attempt`, join not finished) →
     skip; the gate is owned in-process. Applies to every sweep, boot
     included (see the derivation in the intro above).
  2. Otherwise look up the op `(kind='task-verify',
     idempotency_key="{task.id}#g{row.gate_attempt}")` (for
     `gate_attempt = 0`, no attempt was ever prepared → treat as missing).
     **Op non-terminal** → **re-drive it; never defer unconditionally, never
     mint `#g{N+1}`**. There is no background operation driver in steady
     state — the driver loop is an unbuilt TODO (`operation/mod.rs:570`
     comment) and only `submit` and `wait` ever call `drive()`
     (`operation/mod.rs:605-630,641-662`; the dispatch paths call `wait()`
     explicitly, `dispatcher.rs:1231-1242`) — so v3's "defer, recovery will
     re-drive it" was boot-true but steady-state-false: a submitter that dies
     after the op insert (or after `prepare_tx` bumped `gate_attempt`) but
     before waiter registration leaves a non-terminal op that every periodic
     sweep would defer on forever. The sweep therefore spawns a
     **single-flight re-drive task** (keyed by op id in a scheduler-local
     `DashMap`, removed when it returns) that calls
     `operation_runtime.wait(&op_id)` — the one pub API that re-polls
     `drive()` until the op is terminal (`operation/mod.rs:641-662`). The
     re-driven `spawn_side_effect`'s kill-then-spawn (§6.2) reaps any
     recorded orphan from its own earlier attempt and registers a fresh
     waiter; terminal outcomes are reconciled by arms 3-4 on the next pass
     (the re-drive task discards the result). **No double-drive**: `drive()`
     claims a 60s lease per op — the claim UPDATE is guarded by
     `lease_until_ms IS NULL OR < now` (`claim_drive_batch`,
     `operation/mod.rs:1222-1282`) — so a sweep re-drive racing the original
     submitter's `wait()`, boot recovery, or another tick executes no phase
     twice; losers poll. At boot this arm is normally vacuous: operation
     recovery runs *before* the sweep and has already re-driven the op
     (`operation/mod.rs:692-724,1284-1300`), so arm 1's live waiter skips
     the task; if a recovery item errored (`apply_recovery` logs and
     continues, `operation/mod.rs:702-724`), this arm is the retry. The v1
     unconditional "submit a fresh attempt" remains the double-run bug both
     round-1 reviews flagged — re-drive advances the *existing* attempt.
  3. **Op `succeeded`, row still `verifying`, no attempt-matched live
     waiter** → the waiter died (with the kernel, or panicked/aborted while
     the kernel lived — the dead join handle deregistered it) but the
     `setsid` orphan may survive. Kill the recorded process group (row
     triple, `verify_owned_pid`-guarded), then submit `#g{N+1}`; its
     `prepare_tx`/spawn proceed per §6.2 (its kill-prior is then a no-op).
     There is no waiter left to race: attempt N's waiter is dead by this
     arm's precondition, and any still-running stale-attempt waiter writes
     nothing under its `gate_attempt` tx guard — so no attempt-guarded tx
     can land a false red.
  4. **Op `failed`/`stuck`** → compensation should have marked the task; if
     the row is still `verifying`, mark `failed('gate-infra')` (guarded).
  5. **Op missing** (crash between the emit-tx flip and the scheduler's
     submit) → submit `#g{row.gate_attempt + 1}` — first attempt or next, same
     path.
  Crash-window walk for one attempt: before `prepare_tx` commit → op
  non-terminal, arm 2, no process ever existed; after `prepare_tx`, before
  fork → arm 2, kill-prior finds nothing; after fork, before the pid-record
  commit → the child is still **held at the handshake** (§6.2 step 2) and
  kernel death closed its stdin, so it exits without running a single gate
  step — arm 2 re-drives, kill-prior finds the row triple NULL and nothing
  is running (v2's "bounded duplicate the next sweep reaps" residual is
  gone: under the v2 wrapper-writes-pidfile scheme the replacement's live
  waiter made every later sweep skip the task, so that duplicate was in
  fact never reaped — the handshake removes the duplicate instead of
  hand-waving its reaping); after the record commit, before release → same
  held-child self-exit; the re-drive's kill-prior sees `verify_owned_pid`
  false (process gone) and spawns fresh; after release, gate running →
  classic recorded orphan: arm 2 (op non-terminal, re-drive kill-prior
  reaps) or arm 3 (op succeeded, no live waiter → kill + `#g{N+1}`); after
  the waiter tx → row is terminal, sweep skips. **Gates therefore must be
  re-runnable**; the prompt says so (risk R1).
- `done/failed/canceled`: terminal, sweep skips.
- Spec harness: gate-result events that landed while the kernel was down are
  replayed into the pending queue by the existing snapshot-watermark catch-up
  (`harness/mod.rs:79-120` — `task.gate_result` joins the replayed kinds list
  at `harness/mod.rs:88-96`), with the gated-self-report filter applied to the
  replay path too (§6.5).

---

## 9. PR slicing

Each slice lands green (fmt, clippy `-D warnings`, tests, OpenAPI regen where
routes/schemas move) and is independently shippable.

1. **PR-A — tasks table + `calm.plan.*`** (no behavior change for execution).
   Migration 0041 (including the gate bookkeeping columns
   `gate_pid`/`gate_pid_starttime`/`gate_pid_boot_id`, and the waves gate
   policy as `require_task_gates INTEGER NOT NULL DEFAULT 1` **plus the
   same-migration `UPDATE waves SET require_task_gates = 0` backfill** — new
   waves get 1 from the DB DEFAULT, no `NewWave`/`wave_create_tx` change,
   §2), `tasks` repo methods, `plan.rs` tools (upsert/cancel/list),
   validation (cycles, deps, mutability, cwd/gate shape, **and the §4.1
   rule-8 guard rejecting `gate` until PR-C** — rule 6
   (`require_task_gates`/`no_gate_reason`) is **not** implemented here, it is
   PR-C-only per §4.1), `Event::PlanUpdated` +
   role-gate entry, tests. The plan is inert — nothing reads it yet.
   `calm.task.dispatch` untouched. Wave PATCH-route fields for
   `task_budget`/`require_task_gates` + OpenAPI regen (per open question 4
   proposal).
2. **PR-B — scheduler + status machine (no gates)**. `scheduler.rs` with
   triggers/locks/budget/lifecycle gating; **dispatcher subscription kinds
   `plan.updated` + `wave.lifecycle_changed` added** (§5.1) and scheduler
   pokes on the existing task-event arms; claim tx with
   `Event::TaskDispatched` + role-gate entry + **runs/wave-vcs projection
   fallback** (§5.6) + projection tests; guarded `dispatched → running` +
   two-sided `worker_card_id` stamping (§3); emit-tx status flips in
   `calm.task.complete`/`calm.task.fail` (gateless: `running → done|failed`);
   **terminal completion wiring (M2)**: `TerminalTaskHook` threaded from
   `terminal_renderer/mod.rs:379` into the attach-reader exit branch + the
   sweep's running-terminal reconcile arm (§8), actor `KernelDispatcher`;
   boot sweep + boot-order test + `Lagged → sweep_all` + periodic reconcile
   tick (§5.1); e2e: plan → auto-dispatch → worker completes → `done`;
   fast-terminal-exit e2e (id-from-payload lookup). Depends on PR-A. Old
   dispatch path still works in parallel.
3. **PR-C — `task-verify` + gate runner + push rewiring**. Adapter with
   guarded attempt bump + kill-then-spawn (reusing
   `verify_owned_pid`/`signal_process_group`, `spec_appserver.rs:145,174`) +
   record-then-release handshake + row triple persistence (§6.2), wrapper
   script + waiter registry (join-handle liveness, drop-guard deregister) +
   waiter tx, `Event::TaskGateResult`
   (actor `KernelDispatcher`) + role-gate entry, `verifying` transitions +
   sweep verifying arm (§8), **dispatcher subscription kind
   `task.gate_result`**, gated self-report suppression in **both** the live
   push branch and the boot-replay filter (§6.5), gated lifecycle-promotion
   move (suppress in emit tx, promote in waiter tx, §3), new observation
   variant + harness replay kind, `plan/<key>/gate.log` view,
   `require_task_gates` enforcement (§4.1 rule 6, incl. the `no_gate_reason`
   escape hatch) + **removal of the §4.1 rule-8 guard** in the same change.
   Depends on PR-B.
4. **PR-D — spec authority cutover**. Prompt rewrite + prompt tests,
   `calm.task.dispatch`/`calm.dispatch_request` → **deprecation shim** (§4.4:
   `Ok`-payload structured refusal, `visible_to_roles: &[]`, zero transport
   change) for persisted spec threads, removal of the dispatcher
   `*.worker_requested` arms, role-filter test updates, glossary/docs.
   Depends on PR-C (the spec must have a full replacement before losing
   dispatch).

Out of scope honored: no claude-worker adapter (kind rejected, CHECK omits
it), no in-flight cancel/interrupt, no UI task-list view (PR-A's `plan.updated`
events give the UI a hook later; §10 M4).

---

## 10. Issue-vs-code mismatches, risks, open questions

Mismatches (issue assumptions the code does not support):

- **M1 — "worker's worktree" does not exist.** Codex workers run with
  `cwd = $HOME` (`codex_adapter.rs:701`, `routes/codex_cards.rs:196-207`);
  nothing in the repo manages per-worker worktrees (no `worktree` hits in
  `crates/`). Resolved by spec-declared `gate.cwd` (§6.4); honest consequence
  in R2.
- **M2 — terminal workers have no completion contract.** No
  `NEIGE_MCP_TOKEN`/`SOCKET` in their env (`terminal_adapter.rs:837-849`) and
  terminal exit emits no task event (`attach_reader.rs:40-99`); today only a
  spec verdict closes a terminal run. The scheduler needs a terminal signal,
  so PR-B wires **both** exit paths:
  - *Live path*: the attach-reader exit branch holds only an
    `Option<Arc<dyn RouteRepo>>` today (`attach_reader.rs:16-25`), so its
    caller (`terminal_renderer/mod.rs:379`) threads a new `TerminalTaskHook`
    bundle (EventBus + repo + role caches — the same set the dispatcher's
    `Inner` owns) down to where `terminal_set_exit` +
    `runtime_complete_for_terminal` already run (`attach_reader.rs:40-99`).
    On exit it resolves terminal → card → payload `idempotency_key`
    (stamped at `terminal_adapter.rs:590-605` — robust even when the exit
    beats the scheduler's `worker_card_id` stamp), and if a tasks row exists
    under that id, runs the shared guarded completion tx: row flip +
    `task.completed` (exit 0) / `task.failed` (non-zero/signal), actor
    `ActorId::KernelDispatcher` (so `is_spec_verdict_event` does not classify
    it as a verdict, `wave_fs_view.rs:643-645`), wave scope from the card,
    `worker_card_id` stamp, lifecycle promotion per §3.
  - *Downtime path*: exits that land while the kernel is down never reach the
    live branch; the boot supervisor reconcile persists them as
    `terminals.exit_code` (`lib.rs:60-101`) and the **scheduler sweep's
    running-terminal arm** (§8) emits the same guarded completion tx.
  This is new wiring, not "reused as-is" as the issue's notes claim.
- **M3 — no "pause/resume" lifecycle.** `WaveLifecycle` has no paused state
  (`model.rs:283-308`); `Blocked` (+ user-only cancel) is the only hold. The
  scheduler gates on lifecycle as defined in §5.2; "pause the plan" =
  `working → blocked`.
- **M4 — "user-editable plan" is not delivered.** Issue #644 lists the plan
  being inspectable *and editable by the user* among its motivations.
  `calm.plan.*` is spec-only (§4) and the UI task list is out of scope, so
  users can inspect (report, follow-up `plan/index.json` view) but can only
  *edit* the plan by asking the spec in chat. Direct user editing — a
  PATCH-style HTTP route over `tasks` (user-actor validation, same §4.1
  rules) — is a stated follow-up, not part of these four PRs.

Risks:

- **R1 — gate double-run after crash.** Gate execution is at-least-once
  (§6.2/§8) and never concurrent per task (single-runner invariant; the v2
  fork-window duplicate is closed by the record-then-release handshake — a
  gate process the row doesn't record can never run a step). Gates must be
  re-runnable; a non-idempotent gate (e.g. `cargo publish`) is a spec
  authoring error we document in the prompt (§7) but cannot prevent.
- **R2 — shared checkout under parallelism.** With no worktrees, two parallel
  tasks (or a worker plus a sibling's gate) in one repo dir interleave writes
  and produce false reds/greens. Mitigated by default `task_budget = 1`; real
  fix is worktree isolation (follow-up issue).
- **R3 — gate red ≠ worker's fault.** A red can come from a dirty tree left by
  earlier tasks or from `gate-infra`. `status_detail` + per-step logs give the
  spec the evidence, but the status machine intentionally refuses a "maybe"
  state.
- **R4 — silent codex worker death leaves `running` forever.** Policy-free
  scheduler holds no liveness timeout for codex workers (terminal tasks are
  reconciled mechanically, §8). Today's equivalent gap exists (spec waits for
  a report that never comes); the plan at least makes it visible. A kernel
  watchdog emitting `task.failed('worker-dead')` on
  runtime-exited-without-report is a candidate follow-up, kept out to
  preserve "scheduler has no judgment".
- **R5 — emit-tx coupling.** Putting task flips inside
  `calm.task.complete`/`fail` widens those handlers' tx; SQLite contention is
  already retried at the callers (`dispatcher.rs:69,820-848`) but the write
  path grows. Bounded: one indexed UPDATE per report (plus the tasks-row read
  the push/promotion predicates need, §6.5/§3).

Open questions:

1. Budget default 1 vs 3 — 1 is safe (R2) but undercuts the issue's
   parallelism motivation for single-repo waves. Proposal: 1 until worktrees;
   per-wave override exists from day one.
2. Gate log retention: `<data_dir>/gate-logs` grows unbounded; sweep policy
   (e.g. delete on wave terminal + N days) — decide in PR-C review.
3. Does `calm.plan.list` suffice for the spec, or should `runs/index.json`
   gain task status columns so one `neige cat` shows plan+runs? Lean: add to
   the wave-fs view in PR-C only if prompt iteration shows the spec needs it.
4. Exposing `task_budget` / `require_task_gates` on the wave PATCH route +
   OpenAPI — included in PR-A as plain `WavePatch` fields, or deferred?
   Proposal: include (small, and the policy is useless if unreachable);
   slotted into PR-A in §9 on that basis.

(Resolved since v1, now design decisions: gate crash-coherence → §6.2/§8;
lifecycle promotion for gated tasks → §3; lost-envelope liveness → §5.1;
runs projection → §5.6; terminal completion both paths → M2/§8; dispatch
cutover for persisted threads → §4.4; gated-task window before PR-C → §4.1
rule 8.)

---

## 11. Review disposition

### Round 1 (v1 → v2)

Both channels returned REQUEST-CHANGES on v1. Every finding was verified
against the cited code; all were factually correct. Cross-validated cluster
(codex#1 + subagent#3/#4, gate crash-coherence) → §6.2/§8 redesign. codex#2
(fast-report guard + `worker_card_id`) → §3/§5.4. codex#3 (persisted spec
threads) → §4.4 shim. codex#4 (subscription kinds) → §5.1. codex#5 +
subagent#6 (terminal wiring + boot reconcile) → §10 M2 + §8. subagent#1
(runs projection) → §5.6. subagent#2 (verdict misclassification) → §6.5
actor. subagent#5 (lagged bus) → §5.1 backstops. subagent#7 (suppression
leaks) → §6.5. subagent#8 (sandbox claim) → §6.7. subagent#9 → M4.
subagent#10 → §4.1 rule 8. subagent#11 + old open question 2 → §3 promotion
decision.

### Round 2 (v2 → v3)

Both channels confirmed all 16 round-1 resolutions and returned
REQUEST-CHANGES on residuals of the §6.2/§8 redesign plus three independent
findings. All verified against the cited code; none rejected.

- **subagent#1 (MAJOR) — boot-sweep false premise.** "Boot has no registered
  waiters" was false: operation recovery runs before the sweep and is
  synchronous apply-then-drive (`operation/mod.rs:702-724`), re-registering
  waiters for re-driven non-terminal `task-verify` ops
  (`operation/mod.rs:866-903`) — the boot sweep would have killed healthy
  re-driven gates and the killed waiter could land a false red. → The
  live-waiter skip is now arm 1 of the shared sweep body, boot included; the
  safety argument is re-derived from the boot order (§8 intro).
- **subagent#2 (MINOR) + codex#1 (MAJOR) — fork-window orphan never reaped.**
  Correct: the replacement's live waiter made every later sweep skip, so v2's
  "next sweep reaps" was false. → Window **closed**, not restated: the
  record-then-release handshake (§6.2 steps 2-4) holds the wrapper on a
  stdin go-token until the row triple is committed; kernel death before
  release EOFs the pipe and the held child exits having run nothing. The v2
  wrapper-written pidfile is deleted.
- **subagent#3 (MINOR) — dead waiter = stuck `verifying`.** → "Live
  registered waiter" is now defined as join-handle-liveness
  (`!is_finished()`) with drop-guard deregistration on
  completion/panic/abort (§6.2 step 5), so the 300s reconcile arm 3 fires.
- **codex#2 (MAJOR) — `isError` shim not implementable.** Verified: handlers
  return `Result<Value, RpcError>` (`registry.rs:170-172`) and the transport
  hardcodes `"isError": false` (`transport.rs:483-497`). → §4.4 respecified
  as an `Ok`-payload structured refusal (exact JSON shape given); `RpcError`
  variant rejected, zero transport surgery.
- **codex#3 (MAJOR) — pid guard weaker than the repo pattern.** → Added
  `gate_pid_boot_id` (§2); the kill path reuses `pub fn verify_owned_pid` /
  `signal_process_group` (`spec_appserver.rs:145,174`) instead of a bespoke
  starttime check (§6.2).
- **codex#4 (MINOR) — gates-on default under-sliced.** → DB
  `DEFAULT 1` + same-migration backfill `UPDATE … = 0`; no
  `NewWave`/`wave_create_tx` change needed (fixed INSERT column list,
  `model.rs:386`, `db/sqlite.rs:675`); slotted into PR-A (§2, §6.6, §9).
- **codex#5 (NIT) — gate.cwd contradiction.** → One rule: optional, absolute
  when present, default chain `gate.cwd → task.cwd → waves.cwd`; §4.1 rule 7,
  §6.1, §6.4, §7 aligned.
- **subagent#4 (NIT) — per-thread `tools/list` wording.** → Mechanism stated
  as role-keyed `visible_to_roles: &[]` hidden-but-callable
  (`descriptors_for_role`, `registry.rs:268-274`; pattern of
  `emit.rs:207-208`), not per-thread filtering (§4.4).

### Round 3 (v3 → v4)

Both channels confirmed all round-2 resolutions and returned REQUEST-CHANGES
on five distinct findings (subagent#2 and codex#2 are one issue). All
verified against the cited code; none rejected.

- **subagent#1 (MAJOR) — registry deregistration ABA.** Attempt N's
  kill-prior kills N-1's group while N-1's waiter is still alive; N
  registers (overwriting N-1's entry); N-1's waiter exits later and an
  unguarded drop-guard deletes **N's live entry** → next sweep sees "no live
  waiter", arm 3 kills healthy gate N, and N's waiter can win its
  attempt-guarded tx with the SIGKILL exit → false red. → Deregistration is
  now attempt-guarded via `DashMap::remove_if(entry.attempt == my_attempt)`
  (dashmap 6, already the repo's compare-and-remove idiom,
  `shared_codex_appserver.rs:575`), and the liveness definition gains
  `entry.attempt == row.gate_attempt` (§6.2 step 5, §8 intro + arms 1/3).
- **codex#1 (MAJOR) — sweep arm 2 defers forever in steady state.**
  Verified: there is no background operation driver (the driver loop is an
  unbuilt TODO, `operation/mod.rs:570`; only `submit`/`wait` call `drive()`,
  `operation/mod.rs:605-630,641-662`; dispatch paths call `wait()`
  explicitly, `dispatcher.rs:1231-1242`) — so "defer, recovery will re-drive
  it" held only at boot. → Arm 2 now re-drives: single-flight (per op id)
  scheduler task calling `operation_runtime.wait(&op_id)` until terminal;
  lease-safe against concurrent drivers (`claim_drive_batch`'s guarded lease
  UPDATE, `operation/mod.rs:1222-1282`); never mints `#g{N+1}` while an op
  is non-terminal. Same fix applied to the `dispatched` arm (§5.5, §8). §8
  safety argument re-derived; §10 risk statements unchanged (R1's
  at-least-once/never-concurrent survives via lease + kill-then-spawn).
- **subagent#2 (MINOR) + codex#2 (MAJOR) — handshake not portable.**
  Verified: `read -t` is bash/ksh/zsh-only; repo convention is `/bin/sh -c`
  (`routes/terminal.rs:125-126`) and nothing in `crates/` requires bash. →
  Wrapper pinned to `/bin/sh` (POSIX), handshake is plain `read -r _go ||
  exit 75`, go-token newline-terminated (`"go\n"`), and the 60s release timeout
  moved kernel-side (spawner kills the held group on expiry) — §6.2 steps
  2-4; waiter text + crash matrix wording updated.
- **codex#3 (MINOR) — gate-policy slicing ambiguity.** → §4.1 rule 6 marked
  PR-C-only (vacuous before then: rule 8 rejects every declared gate);
  `no_gate_reason` added to the §4.1 tool schema proper; §2.3, §6.6, §9
  PR-A/PR-C bullets aligned.
- **subagent#3 (NIT) — wrong fn name.** → `register_hidden_alias` corrected
  to `register_deprecated_alias` (`registry.rs:283`).

Round 4 verdicts: codex APPROVE (no findings), subagent APPROVE-WITH-NITS
(2 nits folded: waiter gate-infra example, `read -r`).

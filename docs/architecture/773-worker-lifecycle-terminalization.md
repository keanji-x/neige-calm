# Worker-Lifecycle Task Terminalization

> **Status:** DOC-FIRST. Issue #773 Item 2. This document is a *protocol legibility*
> deliverable — it describes code that already exists and is deliberately correct
> but hard to comprehend. **No code changes accompany this doc.** A "façade collapse"
> (any consolidation of the emit sites or guards described here) is **NOT** greenlit;
> see §9.
>
> **Audience:** a senior engineer who must safely modify the terminalization paths in
> `calm-server`. Every claim is anchored to a **function or guard name**, never a line
> number (line numbers on `main` drift fast). Re-grep the names if a path has moved.
>
> **Naming baseline:** post-#679 rename is complete — `RunStatus → WorkerSessionState`,
> `RuntimeRepo → WorkerSessionProjectionRepo`, `CardRuntime → WorkerSessionProjection`.
> The eventized-write helper is named `write_with_actor_events_typed` (the issue and
> earlier design docs call it `write_with_actor_events`; that is the underlying trait
> method on `RepoEventWrite`, wrapped by the `_typed` free function).

---

## 1. Purpose & scope

**Terminalization** is the act of moving a single `tasks` row out of an active status
(`dispatched` or `running`) into a terminal status (`done` or `failed`) **exactly once**,
and emitting the matching `Event::TaskCompleted` / `Event::TaskFailed` in the *same*
database transaction that performs the row flip.

It is **liveness-critical**. A wave's lifecycle advances `Working → Reviewing` via
`auto_transition_if_current_in_tx` (`crates/calm-server/src/wave_lifecycle.rs`), which
is appended to the *same* tx as each terminalization. The scheduler treats `Working`,
`Reviewing`, `Planning`, and `Dispatching` as schedulable and oscillates the wave
`Working ↔ Reviewing`:

- a terminalization promotes `Working → Reviewing`;
- a newly-ready dependent task claims a slot and rides the legal `Reviewing → Working`
  edge (`Scheduler::schedule_wave`, the `(Reviewing, Working)` arm in the claim tx);
- the wave settles in `Reviewing` only once no further task can be claimed.

A task that gets **stuck in `running`** therefore (a) never emits its promotion, and
(b) never enters `done_keys`, so every dependent task's `depends_on` is permanently
unsatisfied (the `done_keys.contains(dep)` ready-set test in `Scheduler`), wedging the
wave's `Working → Reviewing` settle. **A stuck task stalls the whole wave.** This is why
terminalization has so many redundant drivers and such careful guards: the cost of a
*missed* terminalization is a wedged wave; the cost of a *double* terminalization is a
duplicate event. The design optimizes hard against the former while suppressing the
latter (§5).

**Scope of this document:**
- IN: the `tasks` row terminal-state machine; the `WorkerSessionState` machine and the
  writers that drive it; every production emit site that flips a task row + emits a
  terminal task event; the two distinct CAS guard families; the `race_lost_err`
  absorber; the live-vs-sweep duplication guard and the immutable-ownership (F2) check;
  the Kernel-reporter ownership bypass.
- OUT: the gate-runner verdict mechanics (`task_verify_adapter.rs`) except where they
  intersect terminalization; spec-card report-write persistence; the reaper's
  dead-root / codex-silent-death detection heuristics (only its *terminalizing* emit
  site is in scope).

---

## 2. The terminal-state machines

### 2.1 `tasks.status` lifecycle

```
                        ┌──────────────────────────────────────────────┐
                        │                                              │
   (plan insert)        │  worker success on a GATED row               │
        │               ▼  (task_start_verifying_from_worker_tx)       │
        ▼          ┌───────────┐                                       │
   ┌─────────┐     │ verifying │──(gate verdict, task_apply_gate_      │
   │ pending │     └───────────┘   result_tx: pass→done / fail→failed) │
   └────┬────┘          ▲                       │                      │
        │ (scheduler    │                       ▼                      │
        │  claim)       │              ┌──────┐   ┌────────┐           │
        ▼               │              │ done │   │ failed │           │
   ┌────────────┐       │              └──────┘   └────────┘           │
   │ dispatched │───────┘                  ▲          ▲                │
   └─────┬──────┘  worker success on       │          │                │
        │          UNGATED row ────────────┘          │                │
        ▼          (task_complete_from_worker_tx)      │                │
   ┌─────────┐                                         │                │
   │ running │──────── worker / kernel failure ────────┘                │
   └─────────┘         (task_fail_from_worker_tx)                       │
        │                                                               │
        └───────────────────────────────────────────────────────────────┘
```

Active-status set the guarded flips target: **`status IN ('dispatched','running')`**.
`dispatched` is included deliberately — a fast worker can report *before* the
scheduler's `wait()` returns and stamps `running` (see the doc-comment on
`task_complete_from_worker_tx` in `crates/calm-truth/src/db/sqlite.rs`).

Two terminalization shapes:

- **Direct** (`dispatched/running → done|failed`): ungated success, or any failure.
- **Gated, two-hop** (`dispatched/running → verifying → done|failed`): a worker's clean
  exit on a row with `gate_json IS NOT NULL` is treated as a *claim, not evidence*. The
  worker report only moves the row to `verifying`; the automated gate runner then flips
  `verifying → done|failed`. **This second hop is NOT a terminalization emit site in
  the sense of this document** — it is the gate path (§9 disposition), and it emits
  `Event::TaskGateResult`, not `TaskCompleted`/`TaskFailed`.

### 2.2 `WorkerSessionState` lifecycle (RUN-state)

`WorkerSessionState` (`crates/calm-types/src/worker.rs`) is the worker *session*
projection's state, **distinct from the `tasks.status` machine above**. A worker session
can be `Running` while its task row is still `dispatched`; the two are reconciled, not
identified.

```
  Starting ──► Running ──► TurnPending ──► (Running) ... ──► Exited
      │           │                                    └──► Failed
      └───────────┴──────────────────────────────────────► Superseded
                  └──► Idle (re-arms to Running on next activity)
```

Variants: `Starting, Running, Idle, TurnPending, Exited, Failed, Superseded`
(`WorkerSessionState` enum). `Idle` and `TurnPending` re-arm to `Running` on the next
activity beat.

### 2.3 RUN-state writer table

The RUN-state machine is driven by a **smear** of writers. This table exists for
completeness so a modifier knows what *else* touches session state; only `bind_entry`
and the adapters are authoritative transitions. **`liveness_feeder.rs` is not a state
transition** — call this out loudly.

| Writer (fn / file)                                              | Transition                                  | Authoritative? |
|----------------------------------------------------------------|---------------------------------------------|----------------|
| `bind_entry` (`pending_codex_threads.rs`)                       | `Starting → Running` (`session_set_status_tx`) | Yes |
| codex adapter (`operation/codex_adapter.rs`)                    | `→ Running` on activity, `Running → TurnPending` on turn-pending (`session_set_status_tx`) | Yes |
| claude adapter (`operation/claude_adapter.rs`, `worker_flow/claude_transcript.rs`) | `→ Running` on activity | Yes |
| terminal adapter (`operation/terminal_adapter.rs`)             | `→ Running` on activity                     | Yes |
| `run_liveness_feeder` (`liveness_feeder.rs`)                    | **NONE.** Stamps `worker_sessions.{last_activity_ms,last_thread_status}` only. Failures are logged `"durable liveness write failed (observational; ignored)"`. | **No — observational only** |

The RUN-state machine is *input* to terminalization (the reaper reads session liveness
to decide a worker is dead, §6) but is never *itself* a task-row flip.

---

## 3. Emit-site map

There are **5 production `write_with_actor_events_typed` call sites that emit a terminal
task event (`Event::TaskCompleted` / `Event::TaskFailed`)**, in **two classes** (the
issue flattened this into one): **4 worker/kernel sites that flip a `tasks` row AND
emit** — true terminalization (§3.1) — and **1 spec-verdict site that emits the terminal
event but flips no row** (§3.2). A sixth path — the gate runner — *does* flip a row
(`verifying → done|failed`) but emits `Event::TaskGateResult`, not a terminal task event,
and is documented separately in §9 (the issue's "spec-verdict class" claim actually
points at it).

> An "independent emit site" is a function that itself opens the eventized-write tx and
> appends the terminal event. The MCP tool handlers are **not** independent — they build
> the `Event` and *delegate* into a sink method. They are listed below the table and
> explicitly excluded from the count.

### 3.1 Worker / kernel class — 4 independent emit sites

All four share the `task_*_from_worker_tx` CAS family (§4.1) and route their wave
promotion through `auto_transition_if_current_in_tx(Working → Reviewing)` in the same tx.

| # | Site (fn / file)                                                  | Emits                                  | Reporter |
|---|-------------------------------------------------------------------|----------------------------------------|----------|
| 1 | `Scheduler::fail_spawn` (`scheduler.rs`)                          | `Event::TaskFailed` (spawn failure)    | `TaskReporter::Kernel` |
| 2 | `complete_terminal_task` (free fn, `scheduler.rs`)                | `TaskCompleted` (exit 0) **or** `TaskFailed` (non-zero / signal / synthetic `-1`) — two branches of ONE fn | `TaskReporter::Card { owns_key }` (F2-proven) |
| 3 | `CardDecisionSink::commit_worker_task_report` (`decision_sink.rs`)| `TaskCompleted` **or** `TaskFailed` (worker self-report) | `TaskReporter::Card { owns_key }` (F2-proven) |
| 4 | `converge_dead_worker` (`reaper.rs`)                              | `Event::TaskFailed` (reaper observed a dead worker) | `TaskReporter::Kernel` — **deliberate card-ownership bypass** (§6.3) |

Notes:
- Site 2 (`complete_terminal_task`) uses actor `ActorId::KernelDispatcher` on every
  event "so `is_spec_verdict_event` never classifies it as a spec verdict" (its
  doc-comment). It is the **ONE** guarded terminal-completion function — both its drivers
  (live hook, sweep) run *exactly* this tx (§6).
- Site 3 uses actor `ActorId::Kernel` for the auto-transition; the terminal event itself
  carries the reporting card's actor.
- Site 4 (`converge_dead_worker`) calls `task_fail_from_worker_tx` with
  `status_detail = "spawn-failed"` (a minor surprise — it is a *runtime* death, but it
  reuses the spawn-failed detail string) and `reason` carrying the provider-interpreted
  Failed reason (FIX 3 comment).

### 3.2 Spec-verdict class — 1 independent emit site

| # | Site (fn / file)                                              | Emits                            | Row flip? |
|---|---------------------------------------------------------------|----------------------------------|-----------|
| 5 | `CardDecisionSink::commit_spec_verdict` (`decision_sink.rs`)  | `TaskCompleted` (`accepted`) **or** `TaskFailed` (`rejected`) | **NO ROW FLIP** |

**CRITICAL ASYMMETRY (invariant, §8):** `commit_spec_verdict` emits the terminal event
**but does not flip any `tasks` row.** It calls neither `task_*_from_worker_tx` nor
`task_apply_gate_result_tx`. Its tx body does exactly three things: an optional
`auto_promote_draft_in_tx`, an optional `apply_requested_transition_in_tx` (a *wave*
lifecycle move the spec requested), and `events.push((actor, scope, event))` for the
verdict. The row-flip lives **only** in the worker-role-gated handlers — see the
load-bearing comment in `commit_worker_task_report`: *"This hook lives ONLY in the
worker-role-gated `calm.task.complete` / `calm.task.fail` handlers — spec verdict
emissions (`calm.task.verdict`, wave_state.rs) never run it, so verdicts can never flip
rows."* The spec verdict is an *audit/advisory* event over a row whose status was (or
will be) decided by a worker report or the gate runner.

### 3.3 Non-independent / delegating entrypoints (NOT emit sites)

These build an `Event` and delegate; they do not open their own terminalization tx.

- `mcp_server/tools/emit.rs` `task_complete` (`require_role(CardRole::Worker)`) →
  `commit_worker_task_report_for_identity` → **site 3** `commit_worker_task_report`.
- `mcp_server/tools/emit.rs` `task_fail` (`require_role(CardRole::Worker)`) → same → **site 3**.
- `mcp_server/tools/wave_state.rs` `task_verdict` (`require_role(CardRole::Spec)`) →
  **site 5** `commit_spec_verdict`. It folds the verdict into a structured
  `result.status = "accepted"|"rejected"` shape so downstream consumers can tell a
  spec verdict apart from a worker self-report.

### 3.4 FALSE emit sites the issue cited (excluded)

The issue cited two reaper locations as emit sites. **Both are test-only**, below the
`#[cfg(test)]` boundary in `reaper.rs`:

- `task_failed_events` is a **test helper** (collects `TaskFailed` events from a repo for
  assertions); its `Event::TaskFailed { .. }` is a match arm inside that helper.
- The other cite is a **test assertion** match arm (`let failed = task_failed_events(...)`
  followed by a `match &failed[0] { Event::TaskFailed { .. } => … }` assertion).

The reaper has **exactly one** production terminalization emit site: `converge_dead_worker`.
(Note `reaper.rs` also has a *dead-root* convergence path that deliberately emits **no**
`TaskFailed` because a dead root has no task row — see the `converge_dead_root` doc-comment;
that is not an emit site either.)

---

## 4. The two-class CAS contract

Every terminalization is a **guarded UPDATE**: an `UPDATE tasks SET status=... WHERE
<guard>` that returns `rows_affected`. The guard is the concurrency-control primitive —
a 0-row result means a concurrent writer already moved the row. The two classes use
*structurally different* guards; conflating them would be unsound (§9).

### 4.1 Worker / kernel guard (`task_*_from_worker_tx`)

Defined in `crates/calm-truth/src/db/sqlite.rs`. The success path is
`task_report_success_from_worker_tx`, which tries two mutually-exclusive flips and
returns a `SuccessReportFlip` (`Done` / `Verifying` / `None`):

- `task_complete_from_worker_tx` — `→ done`, guard:
  ```
  WHERE id = ? AND wave_id = ?
    AND status IN ('dispatched','running')
    AND gate_json IS NULL
    AND (?card IS NULL OR worker_card_id = ?card
         OR (worker_card_id IS NULL AND ?owns_key))
  ```
- `task_start_verifying_from_worker_tx` — `→ verifying`, identical guard but
  `gate_json IS NOT NULL` (the gated row goes to the gate runner, never straight to
  `done`).
- `task_fail_from_worker_tx` — `→ failed`, same guard **minus** the gate condition (a
  worker failure never runs a gate, so gated and ungated rows fail identically).

The guard has four load-bearing clauses:
1. `id` + **`wave_id`** — a caller can never flip another wave's row even if it echoes a
   foreign task id.
2. `status IN ('dispatched','running')` — the **first-writer-wins** primitive. A row
   already `done`/`failed`/`verifying` matches nothing → 0 rows.
3. `gate_json` polarity — splits ungated-direct from gated-two-hop (success path only).
4. The **two-sided card-ownership guard**: `worker_card_id = ?card` for stamped rows,
   **or** `(worker_card_id IS NULL AND ?owns_key)` for the unstamped window. `?owns_key`
   is the F2 ownership proof (§6.2). `worker_card_id` is also stamped via
   `COALESCE(worker_card_id, ?card)` on a winning flip.

`TaskReporter` (`sqlite.rs`) supplies `(?card, ?owns_key)` via `binds()`:
- `TaskReporter::Kernel → (None, true)` — the bypass (§6.3): `?card IS NULL` makes the
  ownership clause vacuously true and leaves `worker_card_id` untouched (COALESCE NULL arm).
- `TaskReporter::Card { card_id, owns_key } → (Some(card_id), owns_key)`.

### 4.2 Spec-verdict / gate guard (`task_apply_gate_result_tx`)

Also in `sqlite.rs`. Guard:
```
WHERE id = ? AND status = 'verifying' AND gate_attempt = ?
```
This is a **different machine**: it flips `verifying → done|failed` (not from
`dispatched/running`), and it is keyed on `gate_attempt` so a superseded attempt's late
observer writes nothing. It clears the gate-process bookkeeping triple
(`gate_pid`, `gate_pid_starttime`, `gate_pid_boot_id`).

**Important correction to the issue's framing:** this guard is **not** used by the
spec-verdict emit site (`commit_spec_verdict`, §3.2 — that does no flip at all). Its sole
caller is the automated **gate runner**, `apply_gate_result_with_guard_in_tx` in
`crates/calm-server/src/operation/task_verify_adapter.rs`, which emits
`Event::TaskGateResult` (not `TaskCompleted`/`TaskFailed`). See §9. The doc keeps it in
the "two-class CAS contract" because it *is* the second guard family that terminalizes a
task row — but the class that uses it is the **gate runner**, not spec verdicts.

### 4.3 Per-site guard summary

| Emit site | Flip fn | Guard family |
|-----------|---------|--------------|
| `fail_spawn` | `task_fail_from_worker_tx` (Kernel) | 4.1 worker/kernel |
| `complete_terminal_task` | `task_report_success_from_worker_tx` / `task_fail_from_worker_tx` (Card+F2) | 4.1 worker/kernel |
| `commit_worker_task_report` | `task_report_success_from_worker_tx` / `task_fail_from_worker_tx` (Card+F2) | 4.1 worker/kernel |
| `converge_dead_worker` | `task_fail_from_worker_tx` (Kernel) | 4.1 worker/kernel |
| `commit_spec_verdict` | **none** | — (no flip) |
| *(gate runner, §9)* `apply_gate_result_with_guard_in_tx` | `task_apply_gate_result_tx` | 4.2 gate |

---

## 5. `race_lost_err` / absorber semantics

The duplicate-suppression mechanism is a single round-trip through the eventized-write
helper. It is the **only** mechanism — there is no application-level dedup table.

**Constant & helpers (`scheduler.rs`):**
```rust
const RACE_LOST: &str = "scheduler: race lost (guarded write no-op)";
pub(crate) fn race_lost_err() -> CalmError { CalmError::Conflict(RACE_LOST.into()) }
pub(crate) fn is_race_lost(e: &CalmError) -> bool {
    matches!(e, CalmError::Conflict(m) if m == RACE_LOST)
}
```

**Mechanism (the full round-trip):**

1. Inside the tx closure, the guarded UPDATE returns `rows_affected`. If it is `0`, the
   closure returns `Err(race_lost_err())` *before* appending any event.
2. In `RepoEventWrite::write_with_actor_events` (the trait impl in `sqlite.rs`,
   reached via `write_with_actor_events_typed`), a closure `Err` triggers
   `tx.rollback()` and propagates the error — **no event row is appended, no commit, no
   broadcast.** (The same impl also rejects an *empty* event batch with a rollback — a
   separate guard against a closure that flips nothing but tries to emit.)
3. Broadcast happens **only after `tx.commit()` succeeds** (`bus.emit_envelope(...)` in
   the post-commit loop). So the row flip and the broadcast are atomic, and *only the
   winner broadcasts*.
4. The caller's terminal `match` maps the conflict back to a silent no-op:
   ```rust
   match result {
       Ok(_) => Ok(()),
       Err(e) if is_race_lost(&e) => Ok(()),   // first writer already won
       Err(e) => Err(e),
   }
   ```
   This exact arm appears in `fail_spawn`, `complete_terminal_task`, and
   `converge_dead_worker`.

**Semantics:** *first-writer-wins.* A 0-row guarded UPDATE means a concurrent writer
already moved the row out of the active set; the loser rolls back its whole tx
(suppressing its event) and returns `Ok(())`. There is **no** "compare and re-emit" — the
event is simply dropped. Consumers tolerate the *winner's* event; the loser produces
nothing.

> Note on `commit_worker_task_report`'s 0-row handling: it does **not** translate every
> 0-row to a silent success. Round-2/round-6 review hardened it to *disambiguate* a
> 0-row flip (`task_get_tx`): no-row-for-key (legacy `calm.task.dispatch`) and
> already-terminal rows still emit the event (duplicate task events per key are
> tolerated, design §1.3); an **active** row whose ownership guard rejected the reporter
> returns `CalmError::Forbidden` (`"task ... is not owned by reporting card ..."`) — the
> caller is told it does not own the task. This is distinct from `race_lost_err`: it is a
> *security* refusal, not a benign race loss.

---

## 6. Live-vs-sweep duplication guard

### 6.1 `complete_terminal_task` — one tx, three drivers

`complete_terminal_task` (free fn in `scheduler.rs`) is the **ONE** guarded
terminal-completion path for *terminal-kind* workers. Its drivers:

- **(a) live hook** — `TerminalTaskHook::on_terminal_exit` (`scheduler.rs`), installed
  by the dispatcher (`terminal_renderer.set_task_hook(TerminalTaskHook::new(...))`),
  fired from `attach_reader.rs` when the PTY exits.
- **(b) sweep** — `Scheduler::reconcile_running_terminal` (the scheduler's
  `running`-terminal reconcile arm). The boot supervisor reconcile persists dead PTYs as
  `terminals.exit_code = -1`, so a recorded exit runs the *same* guarded completion tx
  as the live hook.
- **(c) test** — a race-proof test in `reaper.rs` (not a production driver).

The shipped race test
(`sweep_exited_race_lost_after_live_terminal_completion_emits_no_second_event`) pairs the
live `complete_terminal_task` hook against the **reaper sweep** (`reaper.sweep_all()`),
not the scheduler sweep — so a maintainer hunting "the race test" finds it under the
reaper, though the DB-row arbitration is identical for either driver.

Both (a) and (b) call `complete_terminal_task(repo, events, write, task_id, wave_id,
card_id, exit_code, signal_killed)`. The status guard
(`status IN ('dispatched','running')`) makes **first writer win; the second no-ops** via
`race_lost_err`. There is no lock between live and sweep — the DB row is the
arbitration point.

### 6.2 The immutable-ownership (F2) check

The live hook resolves the task from the exiting **card's payload** `idempotency_key`,
which is **mutable** via `PATCH /api/cards/{id}` — so it is **not** proof of ownership. A
forged sibling payload could otherwise steal the report-beats-running-stamp window (the
unstamped `dispatched` row).

The proof is `worker_op_targets_card_tx(tx, task_id, card_id)` (`sqlite.rs`), which
returns `true` iff a row exists in `operations` with:
```
kind IN ('codex-worker','terminal-worker')
  AND idempotency_key = ?task_id
  AND target_type = 'card' AND target_id = ?card_id
  AND json_extract(payload_json, '$.actor.kind') = 'KernelDispatcher'
```
The op's `target_id` is stamped in the *same tx* that the adapter's `prepare_tx` creates
the card, and the operations table has **no client-reachable write path** — so it is
immutable from a card's perspective. The `'$.actor.kind' = 'KernelDispatcher'` clause
(round-5 review F2) further requires the op be **scheduler-created**, so a legacy
`calm.task.dispatch` op carrying the spec card's actor cannot collide on the same
idempotency key and flip the plan task during the unstamped window.

`complete_terminal_task` feeds the result as `owns_key` into `TaskReporter::Card`. The
flip then accepts an **unstamped** row only when `owns_key` is true; a **stamped** row is
still guarded by `worker_card_id = card`. A forged-payload card fails both sides → 0 rows
→ no event. The same `worker_op_targets_card_tx` proof is used by
`commit_worker_task_report` (site 3) for the identical window.

### 6.3 Kernel-reporter ownership bypass (reaper)

`converge_dead_worker` (the reaper's single production emit site) uses
`TaskReporter::Kernel`, which **deliberately bypasses the card-ownership guard**
(`binds() → (None, true)`). This is correct and intentional: the reaper is a
kernel-internal observer that has *already* determined the worker is dead by reading
session liveness; there is no live card to prove ownership against, and the row may even
be unstamped. The bypass is reserved for kernel callers that own the row by construction.
`fail_spawn` uses the same `Kernel` reporter for the same reason (the scheduler owns the
spawn-failure reconcile). **Preserve this bypass** — removing it would make the reaper
unable to fail a dead worker's unstamped row, re-opening the wedged-wave failure mode.

---

## 7. Sequence diagrams

### 7.1 Happy path — worker calls `task.complete` (ungated)

```mermaid
sequenceDiagram
    participant W as Worker card (MCP)
    participant E as emit.rs task_complete
    participant DS as commit_worker_task_report
    participant CAS as task_report_success_from_worker_tx
    participant WL as auto_transition_if_current_in_tx
    participant Bus as EventBus

    W->>E: calm.task.complete {idempotency_key, result}
    E->>E: require_role(Worker)
    E->>DS: commit_worker_task_report(identity, TaskCompleted)
    Note over DS: BEGIN IMMEDIATE
    DS->>DS: owns_key = worker_op_targets_card_tx(task_id, card)
    DS->>CAS: flip dispatched/running → done (gate_json IS NULL)
    CAS-->>DS: SuccessReportFlip::Done (rows=1)
    DS->>WL: Working → Reviewing (same tx)
    WL-->>DS: WaveLifecycleChanged + WaveUpdated
    Note over DS: append TaskCompleted + lifecycle events; COMMIT
    DS->>Bus: emit_envelope(TaskCompleted, ...) (post-commit)
    DS-->>W: ok
```

### 7.2 Race — live terminal hook vs sweep, both fire

```mermaid
sequenceDiagram
    participant H as TerminalTaskHook.on_terminal_exit (live)
    participant S as reconcile_running_terminal (sweep)
    participant CT as complete_terminal_task
    participant DB as tasks row (status guard)

    par live hook
        H->>CT: complete_terminal_task(task, exit_code=0)
        CT->>DB: UPDATE ... WHERE status IN ('dispatched','running')
        DB-->>CT: rows = 1 (WIN)
        Note over CT: append TaskCompleted + Working→Reviewing; COMMIT; broadcast
    and sweep
        S->>CT: complete_terminal_task(task, exit_code=0)
        CT->>DB: UPDATE ... WHERE status IN ('dispatched','running')
        DB-->>CT: rows = 0 (row already 'done')
        CT->>CT: return Err(race_lost_err())
        Note over CT: tx.rollback(); no event; no broadcast
        CT-->>S: Ok(())  (is_race_lost → silent no-op)
    end
```

### 7.3 Reaper — worker dies silently

```mermaid
sequenceDiagram
    participant R as Reaper
    participant CDW as converge_dead_worker
    participant CAS as task_fail_from_worker_tx (Kernel)
    participant WL as auto_transition_if_current_in_tx
    participant Bus as EventBus

    R->>R: detect dead worker session (liveness)
    R->>CDW: converge_dead_worker(session, reason)
    CDW->>CDW: resolve task_id via spawn_op_id → operation idempotency key
    Note over CDW: BEGIN IMMEDIATE
    CDW->>CAS: flip dispatched/running → failed (TaskReporter::Kernel — BYPASS card ownership)
    alt rows = 1
        CAS-->>CDW: 1
        CDW->>WL: Working → Reviewing (same tx)
        Note over CDW: append TaskFailed + lifecycle; COMMIT; broadcast
        CDW->>Bus: emit_envelope(TaskFailed)
    else rows = 0 (worker reported first, or already terminal)
        CAS-->>CDW: 0
        CDW->>CDW: Err(race_lost_err()) → rollback → Ok(())
    end
```

### 7.4 Spec verdict — `task.verdict` emits, NO row flip

```mermaid
sequenceDiagram
    participant Sp as Spec card (MCP)
    participant WS as wave_state.rs task_verdict
    participant CSV as commit_spec_verdict
    participant Bus as EventBus

    Sp->>WS: calm.task.verdict {idempotency_key, status: accepted|rejected}
    WS->>WS: require_role(Spec)
    WS->>WS: build Event::TaskCompleted/TaskFailed with result.status
    WS->>CSV: commit_spec_verdict(identity, message, lifecycle, event)
    Note over CSV: BEGIN IMMEDIATE
    CSV->>CSV: auto_promote_draft_in_tx (optional)
    CSV->>CSV: apply_requested_transition_in_tx (optional WAVE lifecycle move)
    CSV->>CSV: events.push(verdict event)  ❗ NO task_*_from_worker_tx, NO task_apply_gate_result_tx
    Note over CSV: COMMIT; broadcast
    CSV->>Bus: emit_envelope(verdict event)
    CSV-->>Sp: ok
    Note over CSV,Bus: tasks row status is UNCHANGED by this path
```

> Contrast: the **gate runner** (`task_verify_adapter.rs`,
> `apply_gate_result_with_guard_in_tx`) *does* flip `verifying → done|failed` via
> `task_apply_gate_result_tx` and emits `Event::TaskGateResult`. That is the path the
> issue mislabeled as "spec verdict". The actual `task.verdict` MCP tool does not flip
> rows.

---

## 8. Invariants & failure modes

### 8.1 Invariants (must hold for any modification)

- **I1 — Exactly-once terminalization.** A `tasks` row leaves the active set
  (`dispatched`/`running`) into `done`/`failed` (or `verifying` for the gated first hop)
  **exactly once**. Enforced by the `status IN ('dispatched','running')` guard
  (worker/kernel class) and `status = 'verifying' AND gate_attempt = ?` (gate class).
  All redundant drivers converge on these guards; the loser self-suppresses via
  `race_lost_err`.
- **I2 — Atomic row+event.** The row flip and the terminal event are written in the
  *same tx* and broadcast only post-commit. There is no event-persisted-but-row-stale
  window (the whole point of folding the flip into the emit tx, design §3).
- **I3 — Wave promotion rides terminalization.** `Working → Reviewing` is appended to the
  terminalization tx via `auto_transition_if_current_in_tx`; it is idempotent (no-op
  unless `current.lifecycle == Working`). For a **gated** success, the promotion is
  *suppressed* (`suppress_promotion`) because the gate-result tx promotes instead — so
  exactly one promotion per gated task.
- **I4 — Spec verdicts never flip rows.** `commit_spec_verdict` emits an advisory verdict
  event only. The row-flip hook is restricted to the worker-role handlers. (Asymmetry
  the issue omitted.)
- **I5 — Card-ownership is proven, not asserted.** A `Card` reporter can flip an
  unstamped row only with an F2 `owns_key` proof (`worker_op_targets_card_tx`); stamped
  rows require `worker_card_id = card`. Forged card payloads cannot terminalize a foreign
  task.
- **I6 — Kernel reporters bypass ownership by design.** `fail_spawn` and
  `converge_dead_worker` use `TaskReporter::Kernel`; they own the row by construction.
- **I7 — Two distinct CAS guard families.** Worker/kernel (from `dispatched/running`) and
  gate (from `verifying`, attempt-keyed) are *not* interchangeable.

### 8.2 Why a naive façade collapse is liveness-dangerous

A "façade" that unifies these paths to reduce surface area is tempting (5 emit sites, 3
guard families, smeared RUN-state writers). Each collapse below maps to a concrete
failure:

- **Drop the sweep arm (`reconcile_running_terminal`).** A terminal worker whose live
  hook never fired (kernel was down at exit; PTY recorded `exit_code = -1` on boot
  reconcile) is *never* terminalized → its task is stuck `running` → the wave wedges
  (§1). The sweep is the backstop for missed live exits.
- **Disable the live hook (`TerminalTaskHook`).** Every terminal completion then waits
  for the next sweep tick — latency balloons and any sweep gating bug (the boot
  `boot_sweep_done` gate) silently parks completions. The live hook is the fast path; the
  sweep is the safety net; **both** are required.
- **Remove the status guard / make the UPDATE unconditional.** Loses I1 → double
  terminalization, duplicate `TaskCompleted`/`TaskFailed`, and (worse) a late writer
  could re-flip a `failed` row to `done` or vice versa, corrupting the wave's review
  state.
- **Unify the two CAS classes (route gate results through `task_*_from_worker_tx`, or
  worker reports through `task_apply_gate_result_tx`).** The guards target different
  source statuses (`dispatched/running` vs `verifying`) and different keys
  (`worker_card_id`/`owns_key` vs `gate_attempt`). A unified guard would either let a
  worker self-report bypass the gate (gated rows would go straight to `done` — the worker
  claims success without evidence, defeating the gate) or let a stale gate attempt flip a
  row out from under a fresh worker report. **Keep them separate.**
- **Lose the Kernel-reporter bypass (force `converge_dead_worker` / `fail_spawn` through
  card ownership).** The reaper has no live card to prove ownership and the row may be
  unstamped → its flip returns 0 rows → the dead worker's task is never failed → wave
  wedges. The bypass is load-bearing, not a shortcut.
- **Collapse the spec-verdict path into the worker-report path (let `task.verdict` flip
  rows).** Breaks I4. A spec's advisory verdict would then race the worker's own report
  and the gate runner for the row flip, and a spec could terminalize a row the worker is
  still legitimately working — re-introducing exactly the ownership ambiguity F2 was
  built to remove.
- **Treat `liveness_feeder` writes as authoritative state transitions.** They are
  best-effort observational stamps (failures ignored). Gating terminalization on them
  would make terminalization lossy under write pressure.

---

## 9. Disposition / issue-vs-code reconciliation

This section records every place the issue #773 text (and the grounding map handed to
this author) diverged from the code as it stands on `main` (post-#679).

### 9.1 Confirmed corrections the map already made (re-verified true)

- **Two emit classes, not one.** Re-confirmed: the worker/kernel class (4 sites,
  `task_*_from_worker_tx`) and a separate guarded class are genuinely distinct. The
  issue's single-CAS framing hides this.
- **The two reaper "emit sites" are test-only.** Re-confirmed: `task_failed_events` is a
  test helper and the other cite is a test assertion, both under `#[cfg(test)]`. The
  reaper's only production terminalizing emit site is `converge_dead_worker`.
- **Spec-verdict no-flip asymmetry.** Re-confirmed and **stronger than the map stated**:
  `commit_spec_verdict` flips *nothing* and calls *no* task-status CAS at all.
- **Kernel bypass in the reaper.** Re-confirmed: `converge_dead_worker` uses
  `TaskReporter::Kernel` → `binds() = (None, true)` → ownership clause vacuously true.
- **`race_lost_err` is the only dedup mechanism.** Re-confirmed: constant string
  `"scheduler: race lost (guarded write no-op)"`, `CalmError::Conflict`, rollback +
  swallow in the trait impl, `is_race_lost → Ok(())` in callers.
- **F2 immutable-ownership check.** Re-confirmed: `worker_op_targets_card_tx`, including
  the round-5 `'$.actor.kind' = 'KernelDispatcher'` refinement.

### 9.2 Where the VERIFIED MAP itself was wrong (reported honestly)

> The task brief asked the map's claim #5 be re-confirmed and any disagreement flagged.
> **The map's claim #5 is partly wrong.**

- **MAP CLAIM #5 said:** the spec-verdict class "uses a DIFFERENT guard
  `task_apply_gate_result_tx` (guard: `status='verifying'` AND gate_attempt match)" and
  emits `TaskCompleted/TaskFailed`, reached via `commit_spec_verdict`.
- **CODE SAYS:** `commit_spec_verdict` does **not** call `task_apply_gate_result_tx` and
  does **not** flip any row. `task_apply_gate_result_tx`'s **only** production caller is
  `apply_gate_result_with_guard_in_tx` in
  `crates/calm-server/src/operation/task_verify_adapter.rs` — the **automated gate
  runner**, a separate subsystem — and that path emits `Event::TaskGateResult`, *not*
  `TaskCompleted`/`TaskFailed`.
- **Reconciliation in this doc:** §3 splits these explicitly. The spec-verdict emit site
  (#5) is a *no-flip advisory event*. The `task_apply_gate_result_tx` guard is documented
  in §4.2 as the **gate-runner** class, with a note that the issue/map mislabeled it as
  "spec verdict". The "second CAS class that terminalizes a row" is therefore the **gate
  runner**, reached from the operation pipeline, not from the MCP `task.verdict` tool.
  The MCP `task.verdict` tool and the gate runner are two different things that the map
  fused. The asymmetry invariant (I4) the doc locks is the *correct, stronger* statement:
  spec verdicts emit but never flip.

- **Minor:** the map labels `converge_dead_worker`'s detail as a generic failure;
  the code reuses `status_detail = "spawn-failed"` even for a *runtime* death (the worker
  started fine then died). Harmless but worth knowing when grepping `status_detail`.
- **Minor:** the helper is `write_with_actor_events_typed` (free fn) wrapping the
  `RepoEventWrite::write_with_actor_events` trait method; the issue/map name only
  `write_with_actor_events`.

### 9.3 Façade decision record (stub)

- **Decision:** *not yet greenlit.* This document is the doc-first deliverable for
  #773 Item 2. It exists to make the terminalization protocol legible **before** any
  consolidation is attempted.
- **What a façade would have to preserve** (from §8.2): both live + sweep drivers; the
  status-guarded first-writer-wins; the two distinct CAS guard families; the F2
  ownership proof; the Kernel-reporter bypass; the spec-verdict no-flip asymmetry; the
  observational (non-authoritative) status of `liveness_feeder`.
- **Open questions for a reviewer** (see summary): the gate-runner path
  (`task_apply_gate_result_tx`) sits adjacent to terminalization and shares the
  `verifying` status — a future façade scope decision must state whether the gate runner
  is *in* or *out* of the terminalization façade. This doc places it *adjacent*
  (documented in §4.2/§9.2) but **out** of the 5-site emit map, because it emits a
  different event class (`TaskGateResult`).

---

## Appendix A — File / symbol index

| Symbol | File |
|--------|------|
| `task_complete_from_worker_tx`, `task_start_verifying_from_worker_tx`, `task_report_success_from_worker_tx`, `task_fail_from_worker_tx`, `task_apply_gate_result_tx`, `task_gate_attempt_bump_tx`, `worker_op_targets_card_tx`, `TaskReporter`, `SuccessReportFlip` | `crates/calm-truth/src/db/sqlite.rs` |
| `RepoEventWrite::write_with_actor_events` (rollback/commit/broadcast) | `crates/calm-truth/src/db/sqlite.rs` |
| `write_with_actor_events_typed` (free fn) | `crates/calm-truth/src/db/mod.rs`, re-wrapped in `crates/calm-server/src/db/mod.rs` |
| `fail_spawn`, `complete_terminal_task`, `reconcile_running_terminal`, `TerminalTaskHook::on_terminal_exit`, `race_lost_err`, `is_race_lost`, `RACE_LOST` | `crates/calm-server/src/scheduler.rs` |
| `converge_dead_worker` | `crates/calm-server/src/reaper.rs` |
| `commit_worker_task_report`, `commit_spec_verdict` | `crates/calm-server/src/decision_sink.rs` |
| `task_complete`, `task_fail`, `commit_worker_task_report_for_identity` | `crates/calm-server/src/mcp_server/tools/emit.rs` |
| `task_verdict` | `crates/calm-server/src/mcp_server/tools/wave_state.rs` |
| `apply_gate_result_in_tx`, `apply_gate_result_with_guard_in_tx` (gate runner) | `crates/calm-server/src/operation/task_verify_adapter.rs` |
| `auto_transition_if_current_in_tx` | `crates/calm-server/src/wave_lifecycle.rs` |
| `bind_entry`, `WorkerSessionState` writers | `crates/calm-server/src/pending_codex_threads.rs`, `operation/{codex,claude,terminal}_adapter.rs` |
| `run_liveness_feeder` (observational) | `crates/calm-server/src/liveness_feeder.rs` |
| `WorkerSessionState` enum | `crates/calm-types/src/worker.rs` |

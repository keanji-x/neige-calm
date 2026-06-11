
# Parked operations: durable continuation + owned-process bookkeeping in the saga (issue #653)

Status: design, revision 3 (round-1/round-2 review dispositions: §11). No code in
this PR. All file:line citations refer to the tree at the time of writing
(`main`, post-#642/#643). First consumer: the #644 gate runner (PR-C,
`docs/architecture/644-plan-then-schedule.md` §6.2/§8 — **superseded in part
by this design**, see the §6 supersession note); second declared consumer:
blocking remote-CI watch gate steps.

Adds three things to the operations saga: a `parked` phase (the op durably
waits on external work while holding no lease), a sanctioned post-spawn
persistence hook (`record_spawn_artifacts` — pid/pgid/starttime/boot_id/log
path on the op row), and an in-tx completion call (`complete_parked_tx`).
Plus: the owned-process identity helpers are lifted out of
`spec_appserver.rs` into a shared module.

---

## 1. Current state (what the code actually does)

### 1.1 Phase machine

`Phase` (`crates/calm-server/src/operation/mod.rs:151-162`):
`Pending → TxCommitted → [AppServerInteract] → SpawnStarted → SpawnSucceeded
→ Succeeded`, with `Compensating → Failed` and `Stuck` branches. Persisted as
a `phase` discriminant + `phase_detail_json` (`serialize_split` /
`deserialize_join`, `mod.rs:232-311`); the DB enforces the discriminant set
via a CHECK constraint (`migrations/0029_operations.sql:12-22`). Adapters
declare which phases they use via `phases()` (`mod.rs:382`); the runtime
consults it to skip `AppServerInteract`/`SpawnStarted` (`mod.rs:772,785`) —
adapters that omit `SpawnStarted` go `TxCommitted → spawn → Succeeded`
directly (`mod.rs:785-813`).

### 1.2 Lease / drive / wait

- Leases are 60s (`OPERATION_LEASE_MS`, `mod.rs:32`). `claim_drive_batch`
  selects non-terminal phases from an explicit allowlist
  (`'pending','tx_committed','app_server_interact','spawn_started',
  'spawn_succeeded','compensating'`) where the lease is absent or expired,
  and stamps a fresh `lease_owner`/`lease_until_ms` (`mod.rs:1222-1282`).
  Every subsequent write is fenced on `WHERE lease_owner = ?`
  (`required_lease_owner`, `mod.rs:1747-1754`; e.g. `set_phase`,
  `mod.rs:1385-1419`), and every phase transition **clears** the lease
  (`mod.rs:1397-1398`).
- There is **no background driver**: only `submit()` (`mod.rs:605-630`) and
  `wait()`'s 25ms poll arm (`mod.rs:641-665`) ever call `drive()`
  (`mod.rs:667-690`); the singleton driver loop is an unbuilt TODO
  (`mod.rs:570-571`), and `abandoned_running_operations_steady_state` is
  explicitly "reserved for PR2" (`mod.rs:452-453,1303-1323`).
- Completion fan-out is a broadcast bus (`OperationCompletionBus`,
  `mod.rs:535-562`, capacity 128); `wait()` checks the durable row first,
  then subscribes, then poll-drives — so a result is never lost to a race.

### 1.3 Boot recovery

`recover_on_boot` scans **all** non-terminal ops (same phase allowlist,
`abandoned_running_operations_on_boot`, `mod.rs:1284-1301`) and plans
`Recover` (re-drive) for pre-terminal phases, `Compensate` for
`compensating`, `Skip` for terminal (`plan_recovery_for`, `mod.rs:1032-1060`).
`apply_recovery` claims each op (`claim_operation_for_boot_recovery`,
`mod.rs:503-532` — same phase allowlist) and re-drives synchronously
(`mod.rs:702-724,1062-1116`). Invoked from `recover_operations_on_boot`
(`crates/calm-server/src/lib.rs:139-145`); boot ordering is asserted by
`boot_order_tests` (`lib.rs:510-547`).

### 1.4 Why long jobs cannot live in a phase today

Three structural facts (all verified):

1. A phase that runs longer than 60s outlives its lease and invites re-claim
   by any concurrent `drive()` (`mod.rs:1238,1264`).
2. With no background driver, an op whose submitter dies sits non-terminal
   until *something* calls `drive()` — boot recovery, another `submit()` of
   any kind (the drive batch is global, `mod.rs:670`), or a `wait()`.
3. `tx_output` is frozen pre-spawn. The write points are
   `prepare_tx_and_advance` (`mod.rs:1329-1383`), the
   `app_server_interact` output update (`mod.rs:830-851` via
   `set_phase_and_tx_output`, `mod.rs:1421-1473`), and the in-tx checkpoint
   (`checkpoint_app_server_interact_tx`, `mod.rs:1662-1705`). After
   `spawn_side_effect` returns, the runtime's only writes are
   `set_phase(SpawnSucceeded)` then `set_phase(Succeeded)`
   (`mod.rs:866-904`) — a spawned child's identity cannot be persisted
   inside the saga.

Consequence: #644's gate runner had to "succeed at spawn" and rebuild
durability outside the saga — seven interlocking mechanisms (644 doc §6.2/§8).
This design moves that durability into the saga once.

### 1.5 Identity helpers to lift

`crates/calm-server/src/spec_appserver.rs`: `read_proc_start_time` (:40-43),
`parse_starttime_from_stat` (:58-69), `read_boot_id` (:94-107),
`verify_owned_pid` (:145-159), `signal_process_group` (:174-196) — all `pub`.
In-crate consumer today: `shared_codex_appserver.rs` (import :41-42; uses
:832,:960-961,:1092-1107,:1335,:1479-1484,:1673-1721). Tests:
`tests/inv_02_killpg.rs:86`, `tests/inv_05_pid_ownership_strong.rs:48`,
`tests/shared_codex_appserver.rs:18,425`,
`tests/codex_e2e_shared_appserver.rs:171`. See §7 and mismatch M1.

---

## 2. Schema change

### 2.1 Columns on `operations`, not a sidecar table

Decision: three new nullable columns on `operations`. Rationale: the data is
strictly 1:1 with the op, must be read by the same `operation_from_row`
materialization (`mod.rs:1707-1745`), and every write to it must share the
row's lease/phase fences (`WHERE id = ? AND lease_owner = ?` /
`AND phase = ?`) — a sidecar table would force cross-table fencing for zero
benefit. Nothing FKs into `operations` and it FKs into nothing
(`0029_operations.sql`; no `REFERENCES operations` anywhere in
`migrations/`), so the rebuild below is clean.

### 2.2 Migration

`phase` has a CHECK constraint (`0029:12-22`); SQLite cannot alter a CHECK in
place, so the migration is a table rebuild — the repo's established
COPY/DROP/RENAME dance with hand-redeclared indexes
(`migrations/0011_terminals_card_id_restrict.sql:19-62` precedent; no FK
pragma juggling needed here since `operations` has no FKs).

Migration number: the migrations dir tops out at `0040_wave_vcs_tree_hash_index.sql`,
so the next free number is **0041** — but the #644 design (644 doc §2) has
already claimed `0041_tasks.sql` for PR-A, which proceeds in parallel. This
doc assumes **`0042_operations_parked.sql`**; renumber to next-free at land
time (whichever PR merges second renumbers).

```sql
-- Issue #653 — parked phase + spawn-artifact bookkeeping.
CREATE TABLE operations_new (
  -- existing columns verbatim from 0029 (no later migration touches
  -- `operations` as of 0040 — re-verify the copy list at land time), except:
  phase TEXT NOT NULL CHECK (phase IN (
    'pending', 'tx_committed', 'app_server_interact',
    'spawn_started', 'spawn_succeeded', 'parked',
    'succeeded', 'compensating', 'failed', 'stuck'
  )),
  -- new:
  spawn_artifacts_json TEXT NULL CHECK (
    spawn_artifacts_json IS NULL OR json_valid(spawn_artifacts_json)
  ),
  parked_at_ms INTEGER NULL,
  parked_deadline_ms INTEGER NULL,
  CHECK (phase <> 'parked' OR (
    spawn_artifacts_json IS NOT NULL
    AND parked_at_ms IS NOT NULL
    AND parked_deadline_ms IS NOT NULL
  ))
);
INSERT INTO operations_new (…existing cols…) SELECT …existing cols… FROM operations;
DROP TABLE operations;
ALTER TABLE operations_new RENAME TO operations;
-- re-declare the three 0029 indexes verbatim (0029:36-44).
```

- `parked_deadline_ms` is **required at park time**, not optional — every
  parked op has a hard upper bound after which the kernel may kill-and-fail
  it (§4.4). A consumer that wants "no timeout" picks a large explicit value;
  the column stays `NULL`-typed only because non-parked rows don't have one
  (the CHECK makes it required exactly where it is meaningful).
- `spawn_artifacts_json` shape is the serialized `SpawnArtifacts` struct
  (§3.2). It may be set while the op is still `spawn_started` (the hook runs
  pre-park, §3.2) — the CHECK only forces it for `parked`.

### 2.3 Phase enum

`Phase::Parked` (unit variant) + `PhaseTag::Parked` (`"parked"`), wired
through `serialize_split`/`deserialize_join` (no detail payload; `parked_at`/
deadline/artifacts are columns, not phase detail) and
`PhaseTag::from_db_str` (`mod.rs:199-215`). `parked_at_ms`,
`parked_deadline_ms`, and `spawn_artifacts: Option<SpawnArtifacts>` join the
`Operation` struct (`mod.rs:42-60`) and `operation_from_row`.

Complete transition set in/out of `parked`:

| Transition | Trigger | Fence |
|---|---|---|
| `spawn_started → parked` | `spawn_side_effect` returns `SpawnOutcome::Parked` (§3.1); runtime calls `set_parked`, then spawns the adapter-supplied observer **after** the commit (§3.1) | lease (`WHERE lease_owner = ?`) + `spawn_artifacts_json IS NOT NULL`; clears the lease |
| `parked → succeeded` | `complete_parked_tx` with `ParkedOutcome::Succeeded` (§3.3) | phase (`WHERE phase = 'parked'`); **also clears the lease** (§3.3) — this is what fences out a concurrent claim holder |
| `parked → failed` | `complete_parked_tx` with `ParkedOutcome::Failed`; or deadline/liveness enforcement (§4.4) after `claim_parked` | phase fence + lease clear (in-tx path) / `claim_parked` (phase+lease predicate, §4.1) then lease-fenced `mark_failed` (enforcement path) |
| `parked → compensating` | `cancel_parked` (§5) | `claim_parked` (§4.1) then `set_compensating` (`mod.rs:1475-1525`) |
| `parked → parked` (no-op) | boot recovery verifies external work alive (§4.2) | read-only |

Explicitly **not** transitions: `pending/tx_committed/app_server_interact →
parked` (parking is only legal from the `SpawnStarted` branch — adapters that
park must declare `PhaseTag::SpawnStarted` and `PhaseTag::Parked` in
`phases()`; a `Parked` return from the direct `TxCommitted` spawn branch
(`mod.rs:785-813`) is an `Internal` error). `parked → spawn_started`
(re-drive) does not exist: drive never claims a parked op (§4.1).

`drive_one`'s match (`mod.rs:733-923`) gains a defensive `Phase::Parked` arm
that logs and returns `Ok(())` — unreachable via `claim_drive_batch`, reachable
only if a future claim-path bug admits it.

---

## 3. API design

### 3.1 Requesting parking: `SpawnOutcome`

Today `spawn_side_effect` returns `Result<SpawnHandle>` (`mod.rs:410-415`)
and the runtime ignores the handle's content (`Ok(_handle)`, `mod.rs:791,872`)
— success of the call *is* the transition signal. Minimal change that makes
the phase decision explicit instead of smuggling it into `SpawnHandle`:

```rust
/// Future that watches the parked work and eventually calls
/// complete_parked_tx. Owns whatever it needs (typically the Child handle).
pub type ParkedObserver = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub enum SpawnOutcome {
    /// Today's behavior: advance toward Succeeded.
    Ready(SpawnHandle),
    /// Park: external work is running; the op will be completed later by
    /// complete_parked_tx. Artifacts must already be recorded (§3.2).
    /// The runtime tokio::spawns `observer` only AFTER set_parked commits.
    Parked { deadline_ms: TimestampMs, observer: ParkedObserver },
}

async fn spawn_side_effect(
    &self,
    output: &TxOutput,
    op: &Operation,
    ctx: &SpawnCtx,
) -> Result<SpawnOutcome>;
```

All nine existing impls (`terminal_adapter.rs:322,646`,
`codex_adapter.rs` ×2, `claude_adapter.rs`, `claude_restart_adapter.rs`,
`spec_harness_start_adapter.rs:566`, `spec_harness_interrupt_adapter.rs`,
`spec_harness_shutdown_adapter.rs`) mechanically wrap their returns in
`SpawnOutcome::Ready(handle)` — zero behavior change (`pending → … →
succeeded` paths untouched, per the issue's non-goal). The same mechanical
wrap applies to the three test adapters (§9 PR-2 checklist):
`tests/dispatcher.rs:357` (`FastReportAdapter`), `tests/dispatcher.rs:443`
(`FailingSpawnAdapter`), `tests/spec_card_reset.rs:58`
(`FailingSpawnSpecHarnessStartAdapter`).

**Why the observer rides the return value instead of being spawned by the
adapter.** The runtime writes `parked` only *after* `spawn_side_effect`
returns (the `Phase::SpawnStarted` branch calls `set_phase` post-return
today, `mod.rs:866-881`; `set_parked` slots in the same place). An observer
the adapter spawned *inside* `spawn_side_effect` could therefore watch a fast
external job exit and call `complete_parked_tx` while the phase is still
`spawn_started` — the phase fence would return `AlreadyResolved`, the verdict
would be swallowed, and the subsequent `set_parked` would park the op with
nobody left watching it (it would sit until deadline/boot, then fail —
verdict lost). Handing the observer to the runtime makes the ordering
structural: **an observer for a given park exists only after that park
committed**, so the observer's first completion attempt can never see
`spawn_started`. The observer owning the `Child` keeps this loss-free even
when the child exits before the park commits — its `child.wait()` simply
returns immediately once the observer finally runs.

Runtime handling at the `Phase::SpawnStarted` branch (`mod.rs:866-894`):
`Ready` → `set_phase(SpawnSucceeded)` as today; `Parked { deadline_ms,
observer }` → assert `adapter.phases().contains(&PhaseTag::Parked)`, then
`repo.set_parked(&op, deadline_ms)`, then — only on `rows_affected == 1` —
`tokio::spawn(observer)`:

```sql
UPDATE operations
SET phase = 'parked', phase_detail_json = NULL,
    parked_at_ms = ?, parked_deadline_ms = ?,
    lease_owner = NULL, lease_until_ms = NULL, updated_at_ms = ?
WHERE id = ? AND lease_owner = ? AND spawn_artifacts_json IS NOT NULL
```

`rows_affected == 0` → two distinguishable causes; the runtime issues **one
bounded re-read** of the row instead of assuming lost lease:

- Lease no longer ours → genuinely lost: `log_lost_lease` pattern
  (`mod.rs:1756-1762`) — and the runtime **drops the observer without
  spawning it** (safety below).
- Lease still ours + `spawn_artifacts_json IS NULL` → the adapter parked
  without ever calling `record_spawn_artifacts` — a contract violation.
  Without this branch the op would sit `spawn_started` holding a valid lease
  until expiry, get re-claimed, spawn fresh external work, miss `set_parked`
  again — a real-process spawn loop at 60s cadence, forever. Instead: treat
  as an `Internal` error → the normal `fail_with_compensation` path
  (`mod.rs:882-891,925-945`) terminates the op; the observer is dropped
  un-spawned here too. One probe, no loop.

Dropping the observer is safe: the artifacts are durable, and whoever took
the lease re-drives `spawn_started`, whose mandatory kill-prior step (§3.2
contract) reaps the recorded group. Adapters should set `kill_on_drop` on the
`Child` they move into the observer so the drop also reaps promptly, but
correctness does not depend on it.

Crash window — kernel dies after `spawn_side_effect` returns but before
`set_parked` commits: the observer never ran (it is spawned post-commit), the
row is `spawn_started` with `spawn_artifacts_json` set. Boot recovery plans
`Recover` for `spawn_started` as today (`mod.rs:1037-1046`); the re-driven
`spawn_side_effect`'s kill-prior reads this op's own recorded artifacts and
reaps the released external work before spawning fresh (§6.4 walks this for
the gate). A verdict the external work produced in that window is lost and
the work re-runs — at-least-once, identical to the v4 #644 design's same
window; parking consumers' work must be re-runnable (644 doc risk R1).

Note `parked` deliberately does **not** stamp `completed_at_ms` — it is
non-terminal.

### 3.2 Post-spawn persistence hook: `record_spawn_artifacts`

Decision: a hook callable **inside** `spawn_side_effect`, *not* folded into
the `Parked` return. Reason: the #644 record-then-release handshake (644 doc
§6.2 steps 2-4, kept by the issue) requires the identity record to be durable
*before* the held child is released — i.e. before `spawn_side_effect`
returns. A return-value-only design would persist after release, reopening
the fork-window orphan the handshake exists to close.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnArtifacts {
    pub pid: i32,
    pub pgid: i32,
    pub start_time: u64,   // /proc/<pid>/stat field 22 at spawn
    pub boot_id: String,   // /proc/sys/kernel/random/boot_id at spawn
    pub log_path: Option<String>,
    #[serde(default)]
    pub extra: Value,      // adapter-private (e.g. exit-status file path, §6)
}

impl SpawnCtx {
    pub async fn record_spawn_artifacts(
        &self,
        op: &Operation,
        artifacts: &SpawnArtifacts,
    ) -> Result<()>;
}
```

`SpawnCtx` (`mod.rs:85-130`) gains an `operation_repo: Arc<dyn OperationRepo>`
field (constructed before `OperationRuntime`, which already receives the
`SpawnCtx` — `mod.rs:574-603`; no cycle). The repo method:

```sql
UPDATE operations SET spawn_artifacts_json = ?, updated_at_ms = ?
WHERE id = ? AND lease_owner = ? AND phase = 'spawn_started'
```

`rows_affected == 0` → `Err` (lease lost mid-spawn). **Parking adapter
contract** (MUST, not #644-specific — the generic crash windows depend on
it):

1. On a `record_spawn_artifacts` `Err`, the adapter must not let the spawned
   work proceed (kill the held child, propagate the error → normal
   `fail_with_compensation` path, `mod.rs:882-891,925-945`).
2. **Reap-own-recorded-artifacts**: a parking `spawn_side_effect` MUST, as
   its first step, read its own op row's `spawn_artifacts` and, if set,
   `verify_owned_pid` + `SIGKILL` the recorded group before spawning. This is
   what makes a `spawn_started` re-drive (crash or lost lease between
   `record_spawn_artifacts` and `set_parked`, §3.1) safe: the overwrite of
   the prior attempt's artifacts is correct *only because* the re-driving
   invocation has already disposed of the recorded predecessor. An adapter
   that skips this leaks a duplicate live external process. (#644
   additionally kills the *previous attempt's* op artifacts — §6.1 step 1 —
   that part is consumer policy; reaping your own row is not.)

The identity triple fields are required, not `Option` — every parking
consumer needs all of them for `verify_owned_pid`.

### 3.3 Completion: `complete_parked_tx`

Matches the repo's in-tx idiom — a free `pub(crate)` fn taking the `Tx`
alias, exactly like `checkpoint_app_server_interact_tx`
(`mod.rs:1662-1705`; `Tx<'tx> = Transaction<'tx, Sqlite>`, `mod.rs:31`) and
composable with `event_append_for_operation_tx` /
`events_append_for_operation_tx` (`crates/calm-server/src/db/sqlite.rs:262-307`)
inside one `begin_immediate_tx` (`db/sqlite.rs:309-332`):

```rust
pub enum ParkedOutcome {
    Succeeded { result: Value },
    Failed { last_error: String, last_error_class: Option<String> },
}

pub enum ParkedCompletion {
    /// This call won; the op is now terminal with the given result.
    Completed(OperationResult),
    /// The op was not parked (already completed, compensating, failed…).
    AlreadyResolved { phase: PhaseTag },
}

pub(crate) async fn complete_parked_tx(
    tx: &mut Tx<'_>,
    op_id: &OperationId,
    outcome: &ParkedOutcome,
) -> Result<ParkedCompletion>;

/// In-tx key→id resolution for callers that hold (kind, idempotency_key).
pub(crate) async fn find_operation_id_by_kind_idempotency_tx(
    tx: &mut Tx<'_>,
    kind: &str,
    idempotency_key: &str,
) -> Result<Option<OperationId>>;
```

Implementation: read the row in-tx; if `phase != 'parked'` →
`AlreadyResolved` (no write). Otherwise, for `Succeeded`, splice
`outcome.result` into the stored `TxOutput.result` — this is load-bearing:
`operation_result_from` derives `OperationOutcome::Succeeded { result }` from
`tx_output.result` (`mod.rs:1786-1795`), so the parked outcome must land
there to surface through `operation_result`/`wait`. Then one guarded UPDATE:

```sql
UPDATE operations
SET phase = ?, phase_detail_json = ?,    -- failed: {"from_phase":"parked","last_error_class":…}
                                         -- succeeded: binds NULL explicitly
    tx_output_json = ?, last_error = ?,
    lease_owner = NULL, lease_until_ms = NULL,
    parked_deadline_ms = NULL,
    completed_at_ms = ?, updated_at_ms = ?
WHERE id = ? AND phase = 'parked'
```

The read-then-guarded-UPDATE pair is race-free only inside a write
transaction: **callers must open the tx via `begin_immediate_tx`**
(`db/sqlite.rs:309-332`, the repo idiom) — the signature cannot enforce it,
so it is a stated contract.

Two fence properties, both load-bearing:

- The **WHERE is a phase fence, not a lease fence**: a parked op holds no
  lease (`set_parked` cleared it) and the completer is not a driver. The
  fence deliberately does *not* check `lease_owner` — a completion is
  authoritative and may land even while a sweep/cancel/boot claim holder
  (§4.1) holds the lease; a real verdict beats enforcement.
- The **SET clears the lease anyway** (`lease_owner = NULL, lease_until_ms =
  NULL`). This upholds the repo invariant that every phase transition clears
  the lease (§1.2, `mod.rs:1397-1398`) and is what makes the
  completion-vs-enforcement race single-winner: the claim holder's subsequent
  `mark_failed`/`set_compensating` are **lease-only fences with no phase
  predicate** (`mod.rs:1562-1572`, `mod.rs:1484-1498`), so after a completion
  commits, their `WHERE lease_owner = ?` matches `NULL` against the holder's
  id and misses. Without the clear, a completion landing between a claim and
  its `mark_failed` would be silently overwritten by a lease-fenced terminal
  write — the v1 design had exactly that hole. Full ordering walk: §4.4.

Idempotency / misuse semantics:

- **Double-complete**: second call gets `AlreadyResolved { Succeeded|Failed }`
  — benign, but the return value is a **write gate, not a hint**: a caller
  performing same-tx consumer writes (tasks-row flip, event append) MUST
  issue them only when `complete_parked_tx` returned `Completed`; on any
  `AlreadyResolved` it writes nothing else in that tx. Consumer-side status
  guards are *not* a substitute: when enforcement won via `mark_failed` —
  an op-only write (`mod.rs:1562-1572`) — the consumer row is untouched, so
  #644's `WHERE status='verifying' AND gate_attempt=N` flip guard still
  matches; an `AlreadyResolved`-blind observer would flip the task per its
  own verdict while the op is failed — a divergence the reconcile never
  repairs (it only handles op-terminal-and-row-still-`verifying`, §6.2).
  Full ordering walk: §6.1 step 6.
- **Complete-after-compensation/cancel**: `AlreadyResolved { Compensating |
  Failed }` — the kill won; the late exit-observer's verdict is discarded.
- **Complete-before-park** (`AlreadyResolved { SpawnStarted }`): structurally
  impossible for a well-behaved adapter — the runtime spawns the observer
  only after the park commits (§3.1), so an observer never races its own
  park write. Seeing this return therefore indicates a contract violation
  (adapter spawned its own observer inside `spawn_side_effect`); it is a bug
  to retry around, not a state to handle.
- **Who may call**: `pub(crate)` — kernel-internal only (the parking
  adapter's exit observer, its `recover_parked` impl, `sweep_parked`'s
  verdict-recovery path (§4.4), the boot `VerifyParked` `Complete` arm
  (§4.2), and nothing else). The phase fence makes even
  a buggy caller unable to clobber a terminal op.

Post-commit notification: `complete_parked_tx` cannot publish to the
completion bus (the tx may roll back). New thin method
`OperationRuntime::publish_completion(&self, result: OperationResult)`
(wraps `self.completion.complete`, `mod.rs:553-555`) that the caller invokes
after commit when it got `Completed`. Skipping it is safe — `wait()`'s 25ms
poll re-reads the row (`mod.rs:657-662`) — the publish only trims latency.

**Observer plumbing** — what an adapter-built observer (§3.1/§6.1) actually
holds. It needs a `SqlitePool` (to open `begin_immediate_tx`) and the
completion bus, but it is built inside `spawn_side_effect` with only
`&SpawnCtx` — and today's ctx carries neither: `SpawnCtx.repo` is
`Arc<dyn RouteRepo>` with `sqlite_pool()` deliberately kept off `RouteRepo`
(`db/mod.rs:846-852`), and the bus is constructed inside
`OperationRuntime::new_unchecked` (`mod.rs:598`), *after* the `SpawnCtx` the
runtime receives. Two minimal additions, no cycle:

- **Pool**: `OperationRepo` (`mod.rs:435`) gains
  `fn sqlite_pool(&self) -> SqlitePool`; `SqlxOperationRepo` returns its
  owned pool (`mod.rs:494-496`), and the two test wrappers delegate to their
  `inner` (`tests/no_double_spawn.rs:100,219`). Reached through the
  `operation_repo` field §3.2 already adds to `SpawnCtx` — no new ctx field.
- **Bus**: `OperationCompletionBus` is already a standalone `Clone` struct
  (`mod.rs:538-541`); hoist its construction out of `new_unchecked` to the
  composition roots (`state.rs:553-573` and the :715/:950 variants,
  `dispatcher.rs:239-253`), pass a clone into `SpawnCtx::new` as a new
  `completion: OperationCompletionBus` field and the same instance into the
  runtime constructor. `publish_completion` stays as specced for callers
  that hold the runtime (sweep, boot recovery); the observer calls
  `ctx.completion.complete(result)` directly post-commit — same channel,
  no runtime handle needed.

`submit()` dedupe interaction: re-submitting `(kind, idempotency_key)` of a
parked op returns the existing op id and calls `drive()` (`mod.rs:612-617`),
which cannot claim it (§4.1) — safe no-op, the caller `wait()`s as usual.

---

## 4. Drive exclusion, `wait()`, boot recovery, deadlines

### 4.1 Lease-scan exclusion

`claim_drive_batch` (`mod.rs:1228-1264`) and
`abandoned_running_operations_steady_state` (`mod.rs:1303-1323`) use explicit
phase **allowlists**; `'parked'` is simply not added — drive can never claim
or re-drive a parked op, and the reserved PR2 background driver inherits the
exclusion. The allowlists that **do** change:

- `abandoned_running_operations_on_boot` (`mod.rs:1284-1301`): **add**
  `'parked'` — boot recovery must see parked ops to verify them (§4.2). The
  issue's "excluded from the boot-recovery re-drive scan" is implemented as
  *included in the scan, never planned as `Recover`* (mismatch M2).
- New repo method `claim_parked(op_id)` — a **dedicated** claim for the
  enforcement paths (sweep §4.4, cancel §5, boot `Fail` §4.2):

  ```sql
  UPDATE operations SET lease_owner = ?, lease_until_ms = ?, updated_at_ms = ?
  WHERE id = ? AND phase = 'parked'
    AND (lease_owner IS NULL OR lease_until_ms < ?)
  ```

  Two deliberate properties. (a) The `phase = 'parked'` predicate is the
  enforcement side's half of the single-winner protocol: a completion that
  commits *before* the claim flips the phase, so the claim misses and the
  enforcer aborts (§4.4 ordering A). (b) `claim_operation_for_boot_recovery`
  (`mod.rs:503-532`) is **not** reused with `'parked'` added to its IN list:
  that allowlist also contains `'compensating'`, so a sweep claim racing
  `cancel_parked` (which flips `parked → compensating`) could claim the
  now-compensating row and `mark_failed` it, skipping the compensation steps.
  The exact-phase predicate makes that impossible.
  `claim_operation_for_boot_recovery` itself is unchanged — `parked` is never
  planned as `Recover`/`Compensate`, so its allowlist never needs the
  variant.

### 4.2 Boot recovery for `parked`

`plan_recovery_for` (`mod.rs:1032-1060`) gains:

```rust
Phase::Parked => RecoveryItem::VerifyParked { op_id: op.id.clone() },
```

`apply_recovery_item` (`mod.rs:1062-1116`), new arm:

1. Deserialize `spawn_artifacts` (NULL on a parked row is impossible per the
   migration CHECK; treat a parse failure as dead — defensive).
2. `alive = verify_owned_pid(artifacts.pid, artifacts.start_time,
   &artifacts.boot_id)` (lifted helper, §7). Cross-reboot: `boot_id` mismatch
   short-circuits to `false` — every pid of the prior boot is gone regardless
   of stamp (`spec_appserver.rs:145-159` doc comment); same-boot pid recycle
   is rejected by the strictly-later starttime; ENOENT → dead.

   **Honest caveat on the error direction**: `false` means "cannot prove this
   is our live process", not "provably dead". The helper returns `false` on a
   `read_boot_id()` read failure and unconditionally on non-Linux targets
   (`spec_appserver.rs:142-159`). For its original use (skip-the-kill),
   `false` is the *safe* direction; for this new use (fail-the-op), it is the
   *unsafe* direction — a transient `/proc` read failure or a non-Linux host
   would fail every parked op `parked_dead` at boot while the external work
   may be alive. Accepted: deployment is Linux-prod, `/proc` reads on a local
   procfs do not transiently fail in practice, and the failure mode is a
   spurious op failure (consumer re-runs re-runnable work), not a wrong kill.
   Stated so the trade-off is a decision, not an accident.
3. Delegate the decision to the adapter:

```rust
pub enum ParkedRecovery {
    /// External work is owned again (adapter re-established an observer
    /// that will eventually call complete_parked_tx). Op stays parked.
    LeaveParked,
    /// Adapter recovered a definitive outcome (e.g. exit-status file, §6).
    Complete(ParkedOutcome),
    /// Kill (if alive) and fail the op with this reason.
    Fail { reason: String },
}

/// Which arm is asking, and what it will do with the answer — the adapter's
/// side effects (notably spawning a reattach observer) must match the mode.
pub enum RecoveryMode {
    /// Boot VerifyParked (§4.2): LeaveParked means "I re-established
    /// observation"; reattach observers belong here.
    Boot,
    /// sweep_parked pre-deadline dead-probe (§4.4): only Complete is acted
    /// on; Fail/LeaveParked are ignored (no premature fail).
    PreDeadlineProbe,
    /// sweep_parked past-deadline arm (§4.4): the op WILL terminate now.
    /// The adapter MUST NOT spawn a reattach observer (it would watch work
    /// the caller kills next); LeaveParked is a contract violation here
    /// (handled defensively as Fail, §4.4).
    PastDeadline,
}

// ProviderAdapter, default impl provided:
async fn recover_parked(
    &self,
    _op: &Operation,
    _artifacts: &SpawnArtifacts,
    alive: bool,
    _mode: RecoveryMode,
    _ctx: &SpawnCtx,
) -> Result<ParkedRecovery> {
    Ok(if alive {
        ParkedRecovery::LeaveParked        // deadline backstop still applies
    } else {
        ParkedRecovery::Fail { reason: "parked process dead with no recorded outcome".into() }
    })
}
```

   The default matches the issue's stated semantics exactly. Adapters
   override to do better — the gate adapter recovers the real verdict from a
   wrapper-written exit-status file and re-attaches to a still-running gate
   (§6.3); a naïve consumer gets safe behavior for free. The boot arm calls
   with `RecoveryMode::Boot` — **unless `now > parked_deadline_ms` already at
   boot, in which case it skips the `Boot` delegation and runs the §4.4
   past-deadline arm directly** (mode `PastDeadline`): a `Boot`-mode call
   would spawn a reattach observer that the boot-end `sweep_parked` (§4.4
   call site b) kills moments later.
4. Apply: `LeaveParked` → nothing (no claim, no write). `Complete(outcome)` →
   own tx: `complete_parked_tx` + commit + `publish_completion`. `Fail` →
   `claim_parked` (§4.1), kill the recorded group if alive
   (`verify_owned_pid` re-check then `signal_process_group(pgid, SIGKILL)` —
   double-kill-safe, §5; if the group **was** alive, follow the kill with
   the same post-kill `recover_parked(alive = false, PastDeadline, …)`
   re-check as §4.4 — mode `PastDeadline` explicitly, the op terminates now;
   the natural-exit-before-kill window is identical), `mark_failed(reason,
   PhaseTag::Parked, Some("parked_dead"))` (`mod.rs:1554-1595`; the claim
   provides the lease the fence needs) + `publish_completion`.

How the consumer learns: the op result carries `OperationOutcome::Failed {
from_phase: PhaseTag::Parked, last_error_class: Some("parked_dead"), .. }`
(`operation_result_from` reads `from_phase`/`last_error_class` from
`phase_detail_json`, `mod.rs:1797-1807,1819-1831`), surfaced via the
completion bus to live `wait()`ers and via
`find_by_kind_idempotency` (`mod.rs:1646-1660`) + `operation_result` to
poll/sweep consumers (#644's op-terminal-but-row-`verifying` reconcile, §6.4).
Boot recovery does **not** write any consumer table — consumer propagation is
the consumer's reconcile, keeping the saga consumer-agnostic.

### 4.3 `wait()` on a parked op

`wait()` works unchanged: the row check returns `None` for `parked`
(`operation_result_from` → `Ok(None)` for non-terminal, `mod.rs:1815`), the
bus delivers the eventual `publish_completion`, and the 25ms poll arm's
`drive()` cannot touch the parked op (§4.1) — it burns a claim query per tick
exactly as it does today for any in-flight op. One addition to the poll arm
(`mod.rs:657-662`): after the row check, call
`self.enforce_parked_deadline(op_id)` (§4.4) so a waiter never blocks past
the deadline on an op nobody else is watching.

### 4.4 Deadline + dead-work sweep: `sweep_parked`

```rust
impl OperationRuntime {
    /// For every parked op: recover a durable verdict if the adapter can,
    /// kill-and-fail past-deadline work, leave healthy work parked. Safe to
    /// call from anywhere, any number of times (fences single-winner).
    pub async fn sweep_parked(&self) -> Result<()>;
    async fn enforce_parked_deadline(&self, op_id: &OperationId) -> Result<()>; // one-op variant
}
```

The sweep is **not** a blind kill-fail: it consults the adapter's
`recover_parked` (§4.2 — mode `PastDeadline` / `PreDeadlineProbe`
respectively) before and, on the kill path, *after* the kill, so a
recoverable verdict (e.g. the gate's exit file, §6.3) is not discarded by
deadline enforcement. Per parked op:

- **Past deadline** (`now > parked_deadline_ms`): `claim_parked` (§4.1) →
  `alive = verify_owned_pid` → `adapter.recover_parked(op, artifacts, alive,
  PastDeadline, ctx)`:
  - `Complete(outcome)` → kill the recorded group first if alive (the
    deadline is a hard stop; an adapter returning `Complete` with a live
    group is asserting the outcome is already durable), then
    `complete_parked_tx` + commit + `publish_completion`.
  - `Fail { reason }` — or, defensively, `LeaveParked` (a mode-contract
    violation in `PastDeadline`, §4.2; logged, treated as
    `Fail("parked deadline exceeded")`) → kill if alive → **post-kill
    re-check**: one more `recover_parked(alive = false, PastDeadline, …)`
    before the terminal write. Why: the work can exit *naturally* and
    durably record its verdict between the first `recover_parked` and the
    kill, and the kill cannot detect this — `signal_process_group` swallows
    `ESRCH` and reports an already-gone group exactly like a no-op
    (`spec_appserver.rs:187-195`) — so without the re-check a real verdict
    in that window is discarded in favor of `parked_deadline`. Re-check
    `Complete(outcome)` → `complete_parked_tx` + commit +
    `publish_completion` (cannot be a false green: a verdict durable here
    either predates the kill or — per ordering B below — is a legitimate
    kill-induced failure verdict). Anything else → `mark_failed(reason,
    PhaseTag::Parked, Some("parked_deadline"))` + `publish_completion`. One
    re-check, not a loop — and near-final, not absolute: `kill(2)` returning
    does not mean the target is already dead, so an in-flight `rename` can
    still land *after* the re-check's read; that verdict falls to
    `parked_deadline`. Sub-millisecond window, in-contract for a hard
    deadline (§6.1's tmp+rename still guarantees a mid-write kill leaves no
    file, never a truncated one).
- **Before deadline**: no claim, no kill. `verify_owned_pid`; if **dead** →
  `recover_parked(alive = false, PreDeadlineProbe, …)`; on `Complete(outcome)` →
  `complete_parked_tx` (phase fence suffices; no claim needed for a pure
  completion) + `publish_completion`. On `Fail`/`LeaveParked` → **do
  nothing**: the live observer may be milliseconds from committing its own
  verdict (it `wait()`ed the child directly and does not need the durable
  artifact the adapter looks for) — failing here would race a real verdict.
  Dead work without a recoverable outcome is failed by the deadline arm or
  by the next boot, never early. This dead-probe is what bounds
  observer-death verdict latency to one sweep tick instead of the full
  deadline (risk R3).

**Single-winner orderings** (enforcement = the past-deadline arm; completion
= any `complete_parked_tx` caller). The two fences interlock: the enforcer's
claim has a `phase = 'parked'` predicate (§4.1) and its terminal write is
lease-fenced (`mark_failed`, `mod.rs:1562-1572`); the completion is
phase-fenced and clears the lease (§3.3).

- **A — completion commits before the claim**: phase is no longer `'parked'`
  → `claim_parked` misses → enforcer aborts (re-read sees terminal).
  Completion wins.
- **B — claim commits first, completion commits before `mark_failed`**: the
  claim did not change `phase`, so the completion's `WHERE phase = 'parked'`
  still matches; it commits the terminal phase **and NULLs the lease** →
  the enforcer's `mark_failed … WHERE lease_owner = ?` misses →
  `log_lost_lease`, enforcer yields. Completion wins. (The enforcer may have
  already SIGKILLed the group in this window — benign even when the
  committed verdict was *caused by* the kill: a live observer `wait()`ing
  the child sees the kill-induced exit and commits the corresponding
  failure verdict — though the observer prefers a present, parseable exit
  file over a signal-death wait status (green-but-late: the gate exited 0
  and renamed, the wrapper caught the SIGKILL pre-exit; the durable success
  wins, §6.3) — which is a legitimate outcome — never a false green,
  since a SIGKILLed child cannot exit 0 — so the race is only over which
  *flavor* of failure lands, the observer's verdict vs the enforcer's
  `parked_deadline`. A verdict that predates the kill is simply the real
  result. The kill of an already-gone group is ESRCH-swallowed.)
- **C — claim and `mark_failed` both commit before the completion**: the
  completion's phase fence sees `'failed'` → `AlreadyResolved`, verdict
  discarded by design (deadline already enforced). Enforcer wins.

Every interleaving lands exactly one terminal write — never both, never
neither. Ordering B is the case the v1 design got wrong: without the lease
clear in `complete_parked_tx`, the lease-only `mark_failed` would have
overwritten the committed success (round-1 finding, §11).

Call sites: (a) `wait()` poll arm (one-op variant, §4.3); (b) boot, at the
end of `recover_on_boot` application (an alive-but-expired op found at boot is
enforced immediately); (c) consumer-owned ticks — #644's
`NEIGE_SCHEDULER_RECONCILE_SECS` reconcile tick (644 doc §5.1) calls
`sweep_parked()`. **Honest limitation**: the saga still has no background
loop of its own (the `mod.rs:570` TODO is untouched, per the issue's
non-goal), so deadline liveness is only as good as its callers — an op that
nobody waits on, in a deployment with no consumer tick, sits parked past
deadline until the next boot or unrelated `wait()`. Flagged as risk R2.

---

## 5. Compensation / cancel

`plan_compensation` already receives `from_phase: PhaseTag` (`mod.rs:417-423`)
and adapters build their own step lists (e.g. `terminal_adapter.rs:436-459`).
For a parking adapter, `from_phase == PhaseTag::Parked` means "external work
may be running": the plan must include a kill step carrying the identity
triple + pgid from `op.spawn_artifacts` (available on the `Operation` the
adapter receives), e.g. `{"op": "kill_parked_group", "args": {pid, pgid,
start_time, boot_id}}`, ahead of its row-cleanup steps.

Entry point — new runtime method (there is no general op-cancel API today;
compensation is only entered from drive failures, `mod.rs:801-810,853-861,882-891`):

```rust
impl OperationRuntime {
    /// Cancel a parked operation: kill the external work, fail the op.
    /// Returns false if the op was not parked (already resolved/racing).
    pub async fn cancel_parked(&self, op_id: &OperationId, reason: &str) -> Result<bool>;
}
```

Implementation: `claim_parked` (§4.1) → `adapter.plan_compensation(
PhaseTag::Parked, reason, output, &op)` → `set_compensating`
(`mod.rs:1475-1525`; flips `phase` to `'compensating'`, fencing out any late
`complete_parked_tx`) → `drive()` (compensating is in the drive allowlist, so
the normal `resume_compensation` machinery runs the steps and lands
`mark_failed(reason, from_phase: Parked)`, `mod.rs:947-983`). Crash mid-cancel
recovers via the existing `Compensating → RecoveryItem::Compensate` boot path
(`mod.rs:1047-1053`).

**Double-kill safety**: the kill step is `verify_owned_pid(pid, start_time,
boot_id)` then `signal_process_group(pgid, SIGKILL)`. Verify-fail (already
dead, recycled, rebooted) → skip, step still completes; `ESRCH` is swallowed
(`spec_appserver.rs:188-195`); non-positive pgid refused
(`spec_appserver.rs:175-184`). The residual verify-to-kill TOCTOU window is
the one the triple already narrows to same-boot-same-starttime — the
documented accepted risk of the existing pattern (`spec_appserver.rs:126-143`).
Cancel racing complete: the §4.4 orderings apply verbatim with
`set_compensating` (also a lease-only fence, `mod.rs:1484-1498`) in place of
`mark_failed` — `claim_parked`'s phase predicate covers ordering A, the
completion's lease clear covers ordering B, single-winner either way.

---



## 6. The #644 gate runner on parked ops

> **Supersession notice.** This section supersedes parts of the #644 design
> (`644-plan-then-schedule.md`, currently v4) for PR-C. **Do not implement
> PR-C from the #644 doc's §6.2/§8 as written**; that doc gets a v5 revision
> after #653 lands (it is deliberately *not* edited now — one design moves at
> a time). Superseded #644 mechanisms:
>
> - `tasks.gate_pid` / `gate_pid_starttime` / `gate_pid_boot_id` columns
>   (644 §2, migration `0041_tasks.sql` lines ~182-184) → replaced by
>   `operations.spawn_artifacts_json` (§2.2/§3.2). **The #644 v5 revision
>   must drop these columns from the 0041 spec** (PR-A; if PR-A has already
>   landed them, v5 specs the drop).
> - The in-memory waiter registry + join-handle liveness definition +
>   drop-guard deregistration (644 §6.2 step 5) → replaced by the durable
>   parked row + runtime-spawned observer (§3.1, §6.1 step 6).
> - Sweep `verifying` arm 1 (live-waiter skip) and arm 3's
>   kill-healthy-gate-then-resubmit behavior (644 §8) → replaced by
>   `VerifyParked` boot recovery (§4.2), `recover_parked` reattach (§6.3),
>   and `sweep_parked` (§4.4).
> - The "operation succeeds at spawn" fiction (644 §6.2/§7) → the op is
>   `parked` while the gate runs.
>
> Still valid in the #644 doc and reused here: the stdin record-then-release
> handshake (644 §6.2 steps 2-4), the kill-prior rule (step 1 — now reading
> op-row artifacts instead of the tasks triple), per-attempt idempotency
> keys, sweep arm 2's single-flight `wait()` re-drive (kept in reduced form,
> §6.2 below), arms 4-5, the gated self-report suppression (§6.5), and the
> lifecycle-promotion move (§3).

### 6.1 One-operation flow

`task-verify` op per attempt, key `"{task.id}#g{N}"` (unchanged).
`prepare_tx`: guarded `gate_attempt` bump + freeze gate/cwd into
`tx_output.data` (unchanged). `spawn_side_effect`:

1. **Kill prior** (kept): resolve the *previous attempt's* artifacts via
   `find_by_kind_idempotency("task-verify", "{task.id}#g{N-1}")` →
   `op.spawn_artifacts`; `verify_owned_pid` → `SIGKILL` group. (The
   `tasks.gate_pid/gate_pid_starttime/gate_pid_boot_id` columns from 644 doc
   §2 are **deleted from that design** — artifacts live on the op row.)
   Same-op re-drive reads its own row's artifacts the same way (§3.2
   contract item 2).
2. **Unlink the stale exit file**: remove `{task_id}-g{N}.exit` if present —
   strictly *after* the kill (step 1), strictly *before* the spawn (step 3).
   The exit path is deterministic per attempt, so a same-op re-drive (crash
   post-release/pre-park, §3.1; boot re-drives the same `spawn_started` op,
   `mod.rs:1037-1046,1062-1116`) would otherwise leave the PRIOR run's
   verdict on disk for §6.3 to recover as if it were the re-run's — a stale
   verdict for work that re-ran. Unlinking is safe exactly at this point
   because step 1 has already killed any recorded predecessor — no live
   writer remains — so it composes with the MUST kill-prior contract.
   (Alternative considered: a per-spawn unique exit path recorded in
   `SpawnArtifacts.extra` — rejected: it accumulates unconsumed files, still
   requires the kill-first ordering for the *running* predecessor, and the
   unlink is one syscall in a step that already exists.)
3. **Spawn held** (kept): `setsid` wrapper, stdin record-then-release
   handshake, POSIX `read -r _go || exit 75` — verbatim 644 §6.2 step 2. The
   wrapper additionally writes its exit code to
   `<data_dir>/gate-logs/{task_id}-g{N}.exit` as its last action, **via
   write-to-temp-file + `rename(2)`** (atomic on the same filesystem): a
   SIGKILL landing mid-write leaves no file, never a truncated one — §6.3
   can treat any present-but-unparseable file as a foreign artifact. (Enables
   §6.3 reattach and the §4.4 post-kill re-check.)
4. **Record** (was a bespoke tasks-row write; now the hook):
   `ctx.record_spawn_artifacts(op, &SpawnArtifacts { pid, pgid, start_time,
   boot_id, log_path, extra: {exit_path} })`. `Err` → kill held child, bail.
5. **Release** (kept): write `"go\n"`, close stdin.
6. **Build the observer** (not spawn — the runtime spawns it after the park
   commits, §3.1): a boxed future owning the `Child`: wait for exit (or kill
   the group at `timeout_secs` — live timeout enforcement stays with the
   handle-owner; the parked deadline is set to `timeout_secs + slack` as the
   backstop), then one tx: `complete_parked_tx(op_id, verdict)` — **and
   branch on its return before writing anything else** (§3.3 write-gate
   contract):
   - `Completed` → same tx: guarded tasks-row flip + `Event::TaskGateResult`
     append (`events_append_for_operation_tx`) → commit →
     `ctx.completion.complete(result)` (§3.3 plumbing). **This is the
     issue's "op-completion + tasks-row flip + event append in ONE tx"
     requirement, satisfied by §3.3's design.**
   - `AlreadyResolved { .. }` → **write nothing**; roll back and exit. The
     enforcement outcome is already on the op; the consumer reconcile arm
     ("row `verifying`, op terminal → copy outcome", §6.2) propagates it to
     the task row. The divergence this gate closes, walked: (i)
     deadline/cancel enforcement claims the op and `mark_failed`s it — an
     **op-only** write (`mod.rs:1562-1572`); the task row is still
     `status='verifying' AND gate_attempt=N` because reconcile has not run
     yet; (ii) the late observer's `complete_parked_tx` returns
     `AlreadyResolved { Failed }`; (iii) had the observer trusted its
     tasks-row guard alone, the guard would still match and the task would
     flip per the observer's verdict while the op stays failed — permanent
     divergence (reconcile skips non-`verifying` rows). With the gate, (iii)
     never executes and reconcile copies the op's failure to the row.

   A gate that exits before the park commits loses nothing: the observer
   owns the `Child`, so its `wait()` returns instantly with the buffered
   exit status once it runs — and because it runs only post-park, its
   `complete_parked_tx` never sees `spawn_started` (§3.1).
7. Return `SpawnOutcome::Parked { deadline_ms, observer }`.

### 6.2 Deleted from / kept in the v4 design

Deleted:

- The in-memory waiter registry (`DashMap<TaskId, WaiterHandle>`) and its
  attempt-guarded `remove_if` deregistration — the parked row *is* the
  registry; the observer is a runtime-spawned task whose death is recoverable
  from durable state.
- The waiter-liveness sweep arm (v4 §8 arm 1) and its boot-order derivation.
- The operation-succeeds-at-spawn fiction itself, and the `tasks` gate-pid
  triple columns.

**Kept (corrected from v1 of this doc, which wrongly listed it as deleted):
v4 §8 arm 2, the steady-state re-drive.** Arm 2 exists because of the
no-background-driver fact (§1.2): a submitter that dies after the op insert
(kernel alive) leaves a non-terminal op that nobody `wait()`s — boot-true but
steady-state-false, in v4's words (644:1047-1051). **Parking does not change
that**: `sweep_parked` only touches `phase = 'parked'` rows, not a stranded
`pending`/`tx_committed`/`spawn_started` op. So the #644 consumer keeps arm
2's mechanism verbatim — single-flight (op-id-keyed) `operation_runtime.wait
(&op_id)` for any non-terminal `task-verify` op found by the reconcile tick —
with one simplification: the arm's *precondition* no longer consults the
waiter registry (deleted), it is just "row `verifying`, op non-terminal".
Calling `wait()` on an op that is (or becomes) `parked` is correct and
useful: the drive arm cannot claim it (§4.1), but the poll arm runs
`enforce_parked_deadline` (§4.3), so the re-drive task doubles as a live
deadline watcher for the orphaned op. Honest scope statement: parked subsumes
the *post-park* half of v4 arm 2's job (the op no longer falsely succeeds at
spawn); the *pre-park* strand is untouched by this design and still needs the
arm. (A saga-generalized `redrive_stalled(kind)` helper was considered and
rejected for now: it would be new background-ish saga surface duplicating an
already-specced, reviewed consumer mechanism — revisit when the `mod.rs:570`
driver-loop TODO is built, which subsumes both.)

Also kept: the handshake (cleanest fork-window closure — now it protects the
*hook* write instead of a tasks-row write), the kill-prior rule, per-attempt
idempotency keys, the consumer's residual reconcile arm "row `verifying`, op
terminal → copy outcome to row" (needed because boot-time `Fail` writes only
the op, §4.2), and the `sweep_parked()` call from the reconcile tick (§4.4).

### 6.3 Gate adapter's `recover_parked`

Called from three arms — boot `VerifyParked` (§4.2), `sweep_parked`'s
pre-deadline dead-probe, and its past-deadline arm (§4.4, including the
post-kill re-check) — distinguished by `RecoveryMode`. Decision order is
**exit file first, liveness second**:

- Exit file present and parseable → `Complete(verdict-from-exit-code)`,
  regardless of `alive` and mode. The live observer (§6.1 step 6) applies
  the same preference to its own wait status: a signal-death status with a
  present, parseable exit file reports the file's verdict (§4.4 ordering B). The verdict is durable the instant the
  wrapper's `rename` lands (§6.1 step 3); keying on `alive` first would
  discard it in the ms window where the wrapper has renamed the file but not
  yet exited — past deadline, that turned a real durable verdict into a
  SIGKILL + `parked_deadline` fail. §4.4's `Complete`-with-live-group
  contract covers the residue: the deadline arm kills the (exiting) group
  before completing.
- Exit file present but unparseable → `Fail("gate-infra")`. Truncation is
  impossible (tmp+rename, §6.1 step 3) and §6.1 step 2 unlinked the path
  before this run spawned, so an unparseable file is a foreign artifact —
  fail loudly rather than guess.
- No exit file, `alive == false` → `Fail("gate-infra")` (runtime default
  would also do; explicit for the error class). The §4.4 pre-deadline
  dead-probe deliberately does *not* act on this return — only `Boot` and
  `PastDeadline` do — so a live observer about to commit is never raced by a
  premature infra-fail.
- No exit file, `alive == true` — mode decides:
  - `Boot` → re-attach: spawn an observer that polls `verify_owned_pid`
    until false, then reads the exit file (present → verdict; absent →
    infra-fail), completing via the same Completed-gated one-tx body (§6.1
    step 6); return `LeaveParked`. (A non-child cannot be `waitpid`ed —
    polling + exit-file is the only cross-restart observation; cadence ~2s,
    bounded by the parked deadline.)
  - `PreDeadlineProbe` → `LeaveParked`, **no observer spawned** (defensive:
    §4.4 only probes dead work, so this branch is unreachable there).
  - `PastDeadline` → `Fail("gate timeout")`, **no reattach observer**: the
    caller kills the group next (then runs the post-kill re-check, §4.4), so
    an observer spawned here would watch a corpse and double-report — the
    mode exists exactly so the adapter knows not to.

Better than v4 in both recovery regimes — with one honest latency caveat:

- **Boot**: v4 §8 arm 3 killed and re-ran, losing any verdict that landed
  while the kernel was down; here the exit file recovers it, and a healthy
  running gate survives the restart (reattach instead of kill).
- **Steady state** (observer panicked, kernel alive, gate exited with a
  durable verdict): v4's liveness sweep detected the dead waiter within one
  ~300s tick and killed + re-ran (verdict via re-execution); here the §4.4
  dead-probe recovers the *original* verdict within one consumer tick — no
  re-execution. Caveat: dead work *without* a durable verdict waits for the
  deadline backstop instead of being re-run at tick latency (the
  no-false-fail trade in §4.4); the deadline (`timeout_secs + slack`) bounds
  it.

### 6.4 §8 crash matrix, collapsed

Same crash points as 644 §8's walk:

- Before `prepare_tx` commit / after it, before fork: op non-terminal → boot
  recovery re-drives (`mod.rs:1037-1046`); kill-prior finds nothing.
  (Kernel-alive submitter death in the same window: kept arm 2 re-drives,
  §6.2.)
- After fork, before the artifact record commits: child held; kernel death
  EOFs the pipe → exits 75 having run nothing; re-drive spawns fresh.
  (Unchanged mechanism, different write target.)
- After record, before release / before `set_parked`: phase still
  `spawn_started` → boot recovery re-drives; the observer never existed (it
  is runtime-spawned only after the park commits, §3.1), so there is no
  half-registered watcher to reason about; held child self-exited on EOF
  (or, post-release, is alive and gets killed by the re-drive's kill-prior
  reading this op's own artifacts — §3.2 contract item 2). A post-release
  gate that *finished* in this window loses its verdict and re-runs
  (at-least-once, §3.1; gates are re-runnable, 644 risk R1). Never
  concurrent.
- Parked, gate running, kernel dies: boot → `VerifyParked` → alive →
  reattach (§6.3). (Was: arm 3 kill + new attempt — a healthy gate now
  survives a kernel restart.)
- Parked, gate finished while kernel down: dead + exit file → real verdict;
  dead + no file → op fails `parked_dead`/`gate-infra`; consumer reconcile
  arm flips the task row. (Was: arm 3, verdict lost.)
- After the completion tx: op terminal; everything idempotent/no-op.

Same-attempt lease-expiry re-drive while the first invocation is alive
(kernel never died): re-claim happens only if `spawn_side_effect` exceeds its
60s lease — the parking spawn is sub-second past the handshake (the 60s
kernel-side release timeout from 644 §6.2 step 3 bounds it); if it ever does
get re-claimed, the re-driver's kill-prior reaps the recorded live group
first. Never two concurrent gates per attempt.

---

## 7. Shared identity module

New module `crates/calm-server/src/proc_identity.rs`. Moves, verbatim
(including the `cfg(target_os)` stubs and doc comments, with path references
updated): `read_proc_start_time`, `parse_starttime_from_stat`, `read_boot_id`,
`verify_owned_pid`, `signal_process_group`
(`spec_appserver.rs:40-196`). Stays in `spec_appserver.rs` (appserver-specific,
not identity primitives): `socket_owned_by_appserver` (:244),
`cleanup_sock_dir` (:209), `SockDirCleanupOutcome`.

No re-export shim — one implementation, one path. Call sites updated:

- `shared_codex_appserver.rs:41-42` (import) — uses at
  :832,:960-961,:1092-1107,:1335,:1479-1484,:1673-1721 follow the import.
- Tests: `tests/inv_02_killpg.rs:86`,
  `tests/inv_05_pid_ownership_strong.rs:48` (+ doc-comment references
  :19-35), `tests/shared_codex_appserver.rs:18,425`,
  `tests/codex_e2e_shared_appserver.rs:171`.
- New consumers: `operation/mod.rs` (§4.2/§4.4/§5) and the #644 PR-C gate
  adapter.

The `signal_process_group` doc comment's spec-push framing
(`spec_appserver.rs:163-173`) is generalized; its `pgid <= 1` guard and ESRCH
semantics are unchanged.

Out of scope (mismatch M1): `calm-proc-supervisor` does **not** hand-roll the
triple — it signals pgids of registry-held children without identity
verification (`crates/calm-proc-supervisor/src/lib.rs:169-196,636-666`,
acceptable because it owns the `Child` handles); `neige-app` has its own
simpler `signal_process_group` (`crates/neige-app/src/main.rs:1671`). Both
are separate crates; unifying them would need a new shared crate and is not
part of this issue.

---

## 8. Testing strategy

Conventions: unit tests live in `operation/mod.rs`'s `#[cfg(test)] mod tests`
(`mod.rs:1900-2161`) using in-memory `SqlxRepo` + direct `sqlx::query` phase
surgery (`stale_driver_cannot_win_final_transition_after_reclaim`,
`mod.rs:2104-2160`, is the template for fence tests); integration tests build
fake adapters implementing `ProviderAdapter` (`tests/no_double_spawn.rs`
pattern — hook-injected spawn behavior, full `AppState` boot).

1. **Phase plumbing**: `Phase::Parked` joins
   `phase_split_round_trips_all_variants` (`mod.rs:1904-1935`); migration CHECK
   rejects `parked` without artifacts/deadline (sqlx negative test).
2. **Fence unit tests** (mod.rs tests, modeled on `mod.rs:2063-2160`):
   `set_parked` requires lease + artifacts; `record_spawn_artifacts` rejects
   stale lease; `complete_parked_tx` double-complete →
   `AlreadyResolved`; complete-after-`set_compensating` →
   `AlreadyResolved`; cancel-after-complete → `cancel_parked` returns false;
   `claim_drive_batch` never returns a parked row; **ordering B** (§4.4):
   `claim_parked` then `complete_parked_tx` then `mark_failed` with the
   claim's lease → completion wins, `mark_failed` rows_affected 0 (asserts
   the lease clear); `claim_parked` misses a row whose phase was flipped to
   `compensating`/terminal between scan and claim (asserts the phase
   predicate, §4.1); `set_parked` miss with the lease still held + artifacts
   NULL → `Internal`/compensation, vs. lease genuinely lost → log-and-drop
   (the §3.1 bounded probe — asserts no spawn loop).
3. **Fake parking adapter** (new `tests/parked_operations.rs`): adapter whose
   `spawn_side_effect` records artifacts for a real spawned `sleep`-style
   child (the inv_02/inv_05 tests already spawn real children for identity
   assertions — follow them), parks, completes from the returned observer;
   asserts `wait()` returns the spliced result and `from_phase` on failure.
   **Observer ordering**: a fast-exit child (observer's first act is
   `complete_parked_tx`) still lands `Completed`, never
   `AlreadyResolved { SpawnStarted }` — asserts the runtime spawns the
   observer only post-park (§3.1); plus a lost-lease `set_parked` →
   observer dropped, never spawned.
4. **Crash-recovery tests**: insert a `parked` row via direct UPDATE with
   (a) artifacts of a live test child → `recover_on_boot` plans
   `VerifyParked`, default `LeaveParked`; (b) artifacts of a dead pid /
   foreign `boot_id` → op fails with `last_error_class="parked_dead"`;
   (c) deadline in the past + live child → `sweep_parked` kills (assert via
   `verify_owned_pid` flip) and fails with `parked_deadline`;
   (d) adapter override returning `Complete` → op succeeds with recovered
   result; (e) **deadline + recoverable verdict**: deadline in the past,
   child dead, adapter `recover_parked` returns `Complete` → op succeeds
   with the recovered result, not `parked_deadline` (§4.4); (f)
   **pre-deadline dead-probe**: deadline in the future, child dead, adapter
   returns `Complete` → completed at sweep time; adapter returns `Fail` →
   op stays parked (no premature fail); (g) **post-kill re-check** (§4.4):
   deadline in the past, first `recover_parked` returns `Fail`, a durable
   verdict appears before the kill (test-hook injected) → the second
   `recover_parked(alive=false)` recovers it; op completes with the verdict,
   not `parked_deadline`; (h) **mode plumbing**: a recording fake adapter
   sees `Boot` from boot recovery, `PreDeadlineProbe`/`PastDeadline` from
   the respective sweep arms, and `PastDeadline` from boot when the deadline
   already expired (§4.2).
5. **Recovery-vs-completion race**: parked op + concurrent
   `complete_parked_tx` and `cancel_parked`/`sweep_parked` — exactly one
   terminal outcome, no `Stuck`.
6. **Boot order**: no new boot step is added — parked recovery rides
   `recover_operations_on_boot` (`lib.rs:139-145`), so `boot_order_tests`
   (`lib.rs:510-547`) is unchanged; add one assertion-style unit test that
   `recover_on_boot`'s plan contains `VerifyParked` items for parked rows
   (extends the recovery-plan coverage rather than the order test).
7. **Identity lift**: `tests/inv_02_killpg.rs` / `inv_05_pid_ownership_strong.rs`
   keep passing against the new module path — they are the lift's acceptance
   tests.

**PR-C (#644) test obligations.** The above covers the generic saga fences;
the two #644-consumer fences land as **#644 PR-C acceptance tests, outside
PR-2's scope**: (a) Completed-gated consumer writes — task flip + event
append happen only on `ParkedCompletion::Completed`; `AlreadyResolved`
writes nothing and the reconcile copies the enforcement outcome to the row
(§3.3/§6.1 step 6); (b) unlink-across-re-drive — a same-op re-drive never
recovers the prior run's exit file (§6.1 step 2); (c) exit-file tmp+rename
atomicity — present-but-unparseable ⇒ infra-fail (§6.1 step 3/§6.3).

---

## 9. PR slicing

Two PRs, each green (fmt, clippy `-D warnings`, tests; no OpenAPI surface —
operations are not exposed via HTTP routes/schemas, so no regen expected;
gate it anyway per repo policy if anything drifts):

1. **PR-1 — `proc_identity` lift.** Pure move + call-site updates (§7), zero
   behavior change. Lands independently; shrinks PR-2 and unblocks any #644
   PR-C prework that wants the helpers at the shared path.
2. **PR-2 — parked primitive.** Migration 0042 (rebuild), `Phase::Parked` +
   plumbing, `SpawnOutcome` + `ParkedObserver` (mechanical
   `SpawnOutcome::Ready` wraps for the **nine production impls §3.1 *and*
   the three test `ProviderAdapter` impls** — `tests/dispatcher.rs:357`
   `FastReportAdapter`, `tests/dispatcher.rs:443` `FailingSpawnAdapter`,
   `tests/spec_card_reset.rs:58` `FailingSpawnSpecHarnessStartAdapter` —
   without them the slice does not compile),
   `SpawnCtx::record_spawn_artifacts`, `set_parked` + post-park observer
   spawn, `complete_parked_tx` + lookup helper + `publish_completion`,
   `claim_parked`, `recover_parked` + `RecoveryItem::VerifyParked`,
   `cancel_parked`, `sweep_parked` + `wait()` deadline arm, drive/recovery
   allowlist edits, tests §8. No shipped adapter parks yet — the first real
   consumer is #644 PR-C, which depends on this PR (issue sequencing) and
   **must be implemented from §6 here, not from 644 v4 §6.2/§8** (§6
   supersession notice; 644 v5 revision follows this landing).

Not sliced further: the schema, enum, and `SpawnOutcome` are mutually
load-bearing; splitting them ships dead columns or an unconstructable
variant.

---

## 10. Issue-vs-code mismatches, risks, open questions

Mismatches (issue assumptions vs. the code):

- **M1 — "three call sites" overcounts.** The issue claims the
  proc-supervisor's PTY reconcile is a third hand-rolled identity
  implementation. It is not: `calm-proc-supervisor` kills pgids of children
  it holds handles for, with no `(pid, starttime, boot_id)` logic
  (`calm-proc-supervisor/src/lib.rs:169-196,636-666`), and it is a separate
  crate that cannot use a `calm-server` module. The real duplicate is
  `neige-app`'s private `signal_process_group`
  (`crates/neige-app/src/main.rs:1671`).
  Post-lift in-crate call sites: shared_codex_appserver, parked recovery,
  gate adapter (with #644 PR-C). Cross-crate unification: out of scope (§7).
- **M2 — "excluded from the boot-recovery re-drive scan"** is implemented as
  *included in the scan, planned as `VerifyParked` instead of `Recover`*
  (§4.1-4.2). Literal exclusion would orphan parked ops at boot — the scan is
  how the liveness check runs. Matches the issue's intent (never re-driven),
  not its letter.
- **M3 — `complete_parked(op_id, outcome_json)` under-specifies.** The
  result must be spliced into `tx_output.result` (or `operation_result_from`,
  `mod.rs:1786-1795`, surfaces `Null`), the completion needs a post-commit
  bus publish for `wait()` latency, and the call needs a typed
  already-resolved return for the cancel/deadline races — §3.3 supplies all
  three.
- **M4 — "alive → leave parked" is not sufficient on its own.** A parked op
  whose observer died with the kernel has nobody watching it even though the
  work is alive; without the adapter `recover_parked` hook (reattach) and the
  required deadline (backstop), "leave parked" means "leak until deadline".
  The issue's text is the safe *default*; the hook is what makes the gate
  consumer actually work across restart (§4.2, §6.3).
- **M5 — "deletes the re-drive arm entirely" overclaims.** The issue says
  parking lets #644 delete its steady-state re-drive arm outright; §6.2
  keeps it in reduced form — parking subsumes only the *post-park* half of
  the arm's job, while the pre-park strand (submitter dies after the op
  insert, kernel alive) is untouched by this design (644:1047-1051). The
  issue text is to be corrected when #653 is scheduled.
- Verified true as claimed: 60s lease (`mod.rs:32`), no background driver
  (`mod.rs:570,605-630,641-665`), `tx_output` frozen pre-spawn
  (`mod.rs:866-904,1329-1383`), boot recovery scans non-terminal only
  (`mod.rs:1284-1301`).

Risks:

- **R1 — table-rebuild migration.** The CHECK forces a COPY/DROP/RENAME of
  `operations` (§2.2). Bounded: server is down during migration, table is
  small, no FKs either direction; the three indexes must be re-declared by
  hand (the 0011 precedent documents the foot-gun).
- **R2 — deadline liveness depends on callers.** No background loop is added
  (issue non-goal); a parked op with no waiter and no consumer tick outlives
  its deadline until the next boot or incidental `wait()` (§4.4). Acceptable
  for the declared consumers (the #644 scheduler has a reconcile tick); a
  future saga-owned driver loop (`mod.rs:570` TODO) subsumes this.
- **R3 — observer death with a live kernel.** A panicked exit-observer task
  leaves a parked op unobserved. If the work later dies with a durable
  verdict, `sweep_parked`'s pre-deadline dead-probe recovers it within one
  sweep tick (§4.4); if it dies *without* one, the op waits for the deadline
  backstop (the deliberate no-false-fail trade, §4.4) — that tail is the
  residual regression vs v4's ~300s kill-and-re-run detection, bounded by
  `timeout_secs + slack`. Work still alive with a dead observer is invisible
  until deadline/boot; a consumer wanting faster detection can re-run its
  adapter's `recover_parked` from its own tick — out of saga scope.
- **R4 — `wait()` poll cost over long parks.** A 30-minute parked gate with a
  live waiter issues a `claim_drive_batch` query every 25ms (`mod.rs:657-662`)
  — pre-existing `wait()` behavior, now exercised for much longer spans —
  and the §4.3 `enforce_parked_deadline` arm adds the pre-deadline
  dead-probe on top: one `/proc` read (`verify_owned_pid`) per tick per
  waiter for the whole park. Harmless on a local procfs. Candidate
  follow-up: back off the poll interval (and with it the probe cadence)
  when the row is `parked`; not required for correctness.

Open questions:

1. Migration number 0041 vs 0042 — race with #644 PR-A (§2.2); assign at land
   time.
2. Should `enforce_parked_deadline` run in `wait()`'s poll arm at all
   (§4.3/§4.4)? It makes a passive reader perform kills; the alternative
   (consumer ticks only) leaves a waiting caller blocked past deadline. Lean:
   keep it — the claim fence makes it race-safe, and "deadline means the op
   terminates" is the contract.
3. Completion-bus capacity (128, `mod.rs:545`) under many long-parked
   waiters: `Lagged` falls back to the 25ms poll (`mod.rs:651`), so
   correctness holds; bump only if profiling says so.
4. Should `SpawnArtifacts` be recorded for *non-parking* spawns too (e.g.
   codex worker PTYs) as free observability? Cheap, but widens the PR; lean
   no for this issue.

---

## 11. Review disposition

### Round 1 (v1 → v2)

Both channels returned REQUEST-CHANGES on v1. Every finding was verified
against the cited code before fixing; none rejected. Cross-validated cluster
(subagent#1 ≡ codex#1, lease-fence race) was the load-bearing one.

- **subagent#1 (MAJOR) ≡ codex#1 (BLOCKER) — single-winner claim false in
  ordering B.** Verified: `mark_failed`/`set_compensating` are lease-only
  fences with no phase predicate (`mod.rs:1562-1572`, `1484-1498`), and v1's
  `complete_parked_tx` did not clear the lease — a completion landing between
  a claim and its `mark_failed` was silently overwritten; v1's §4.4 prose
  ("both fences are on phase") was wrong against the code. → §3.3 SQL now
  clears `lease_owner`/`lease_until_ms` (restoring the `mod.rs:1397-1398`
  invariant); §4.4 rewritten as an explicit A/B/C ordering walk. Claim path
  decided: dedicated `claim_parked` with an exact `phase='parked'` predicate
  (§4.1) instead of widening `claim_operation_for_boot_recovery`'s allowlist
  — the reuse had its own hazard (the allowlist contains `'compensating'`,
  so a sweep claim racing `cancel_parked` could fail a compensating op past
  its compensation steps).
- **codex#2 (BLOCKER) — fast completion before park.** Verified: the runtime
  writes the post-spawn phase only after `spawn_side_effect` returns
  (`mod.rs:866-881`), so a v1 adapter-spawned observer could complete against
  `spawn_started`, get `AlreadyResolved`, and strand the op parked forever.
  → Mechanism (b): `SpawnOutcome::Parked` now carries a `ParkedObserver`
  future the runtime `tokio::spawn`s only after `set_parked` commits (§3.1)
  — observer-after-park is structural, not a convention; lost-lease drops
  the observer un-spawned; the kernel-death window between return and
  `set_parked` resolves via the existing `spawn_started` re-drive + the §3.2
  MUST kill-prior (verdict lost, work re-runs — at-least-once, same as v4).
  Candidate (a) retry-on-`NotYetParked` rejected (polling a race), (c)
  accept-from-`spawn_started` rejected (breaks the fence discipline).
- **subagent#2 (MAJOR), partially codex#4 — steady-state re-drive
  regression.** Verified against 644:1047-1051: arm 2 exists because a dead
  submitter strands a non-terminal op nobody drives; `sweep_parked` only
  touches parked rows, so v1's "deleted" claim recreated the v3 bug v4
  fixed. → §6.2 keeps v4 arm 2's single-flight `wait()` mechanism in the
  consumer (precondition simplified: no waiter registry), doubling as a live
  deadline watcher via `wait()`'s poll arm; a saga-generalized
  `redrive_stalled(kind)` was considered and rejected (new background-ish
  saga surface duplicating a reviewed consumer mechanism; the future driver
  loop subsumes both). Honest-scope sentence added.
- **subagent#3 (MAJOR) ≡ codex#4 (MAJOR) — deadline sweep discards
  recoverable verdicts.** → §4.4 sweep now consults `recover_parked` before
  any kill-fail, and gains a pre-deadline dead-probe that recovers durable
  verdicts at tick latency while deliberately *not* failing dead-no-verdict
  work early (no-false-fail vs a racing live observer); §6.3's "strictly
  better than v4" replaced with a regime-by-regime comparison including the
  residual dead-no-verdict latency tail (R3 updated).
- **codex#3 (MAJOR) — doc drift with 644 v4.** → Prominent supersession
  notice atop §6 + §9 PR-C note: PR-C implements from this doc; 644 gets a
  v5 (not edited now); exact superseded list (gate_pid* columns — 0041 must
  drop them — waiter registry, sweep arms 1/3, succeeds-at-spawn) vs
  still-valid list (handshake, kill-prior, per-attempt keys, arm 2 reduced,
  arms 4-5, §6.5 suppression, promotion move).
- **subagent#4 (MINOR) — `verify_owned_pid` error direction.** → §4.2
  paragraph: `false` = "cannot prove ours", inverted safety for the
  fail-the-op use; accepted for Linux-prod, failure mode is spurious re-run
  not wrong kill.
- **subagent#5 (MINOR) — kill-prior was a parenthetical.** → §3.2 parking
  adapter contract item 2: MUST reap-own-recorded-artifacts; #644's
  previous-attempt kill stays consumer policy.
- **codex#5 (MINOR) — test adapters missing from the mechanical wrap.** →
  §3.1 + §9 PR-2 list all three (codex cited two; `tests/dispatcher.rs:443`
  `FailingSpawnAdapter` is the third).
- **NITs (subagent#6/#7, codex#6)** — Succeeded branch binds
  `phase_detail_json` NULL explicitly (§3.3 SQL comment);
  `begin_immediate_tx` caller contract sentence (§3.3); M1 path corrected to
  `crates/neige-app/src/main.rs:1671`; §2.2 copy-list pinned "as of 0040".

### Round 2 (v2 → v3)

Channel A: APPROVE-WITH-NITS (3 MINOR + 4 NIT; all round-1 clusters
re-verified resolved). Codex: REQUEST-CHANGES (3 MAJOR + 1 MINOR). All seven
distinct findings verified against the cited code and accepted; none
rejected; no round-1 regressions introduced.

- **codex#1 (MAJOR) — consumer writes were not `Completed`-gated.**
  Verified: enforcement's `mark_failed` writes only the op
  (`mod.rs:1562-1572`); a task row still `verifying` at attempt N satisfies
  the #644 flip guard, so a late observer relying on row guards alone flips
  the task against a failed op — divergence no reconcile repairs. → §3.3
  double-complete bullet is now a hard write-gate contract (same-tx consumer
  writes only on `Completed`; on `AlreadyResolved` write nothing — reconcile
  copies the enforcement outcome); §6.1 step 6 branches explicitly and walks
  the divergence ordering (i)–(iii).
- **codex#2 (MAJOR) — verdict landing between pre-kill check and the fail
  write was discarded.** Verified: `signal_process_group` cannot distinguish
  "killed it" from "already gone" (`ESRCH` swallowed,
  `spec_appserver.rs:187-195`), so a natural exit + durable verdict in that
  window fell to `parked_deadline`. → §4.4 past-deadline arm gains a
  **post-kill re-check**: one more `recover_parked(alive=false,
  PastDeadline)` between the kill and `mark_failed`; `Complete`
  short-circuits to the recovered verdict. One probe, not a loop — after a
  delivered SIGKILL nothing new becomes durable (tmp+rename excludes
  partial files). The "never discarded" claim softened to match. Interacts
  with subagent#4 below: a kill-induced verdict the re-check (or a racing
  live observer) recovers is legitimate, never false-green.
- **codex#3 (MAJOR) — stale exit file across same-op re-drive.** Verified:
  boot re-drives the same `spawn_started` op (`mod.rs:1037,1067`) and the
  deterministic `{task_id}-g{N}.exit` survives a post-release/pre-park
  crash, so §6.3 would recover the prior run's verdict for re-run work. →
  **Chose unlink-before-spawn** over per-spawn unique exit paths: new §6.1
  step 2 unlinks the path after kill-prior and before the held spawn (exact
  ordering specced: kill recorded predecessor → unlink exit file → spawn
  held → record → release); safe because the kill removes any live writer,
  and it composes with the §3.2 MUST. Unique-path alternative rejected in
  place (accumulates unconsumed files; still needs kill-first).
- **codex#4 (MINOR) + subagent#3 (MINOR) — `recover_parked` lacked context.**
  → §4.2 `RecoveryMode { Boot, PreDeadlineProbe, PastDeadline }` parameter.
  §6.3 rewritten to check the **exit file first** (a durable verdict beats
  `alive` — closes subagent#3's ms window where the wrapper renamed the
  file but has not exited, which previously meant `LeaveParked` → override
  → kill + `parked_deadline` over a real verdict) and to **never spawn a
  reattach observer in `PastDeadline`** (codex#4's doomed observer); boot
  with an already-expired deadline skips `Boot` mode and runs the
  past-deadline arm directly (§4.2 step 3 note).
- **subagent#1 (MINOR) — observer plumbing unspecified.** Verified:
  `SpawnCtx.repo` is `Arc<dyn RouteRepo>` with `sqlite_pool()` deliberately
  off it (`db/mod.rs:846-852`); the bus is born inside `new_unchecked`
  (`mod.rs:598`). → §3.3 plumbing paragraph: `OperationRepo::sqlite_pool()`
  (`SqlxOperationRepo` returns its pool, `mod.rs:494-496`; the two
  `no_double_spawn.rs` wrappers delegate), reached via the §3.2
  `SpawnCtx.operation_repo` field; `OperationCompletionBus` (already
  standalone + `Clone`, `mod.rs:538-541`) hoisted to the composition roots
  and carried on `SpawnCtx` as `completion` — the observer publishes via
  `ctx.completion.complete`, no runtime handle, no cycle.
- **subagent#2 (MINOR) — `set_parked` miss conflated lost-lease with
  missing-artifacts.** A contract-violating adapter (parks without
  recording) re-spawned real external work every 60s forever. → §3.1: one
  bounded re-read on `rows_affected == 0`; lease-still-ours + artifacts
  NULL → `Internal` → `fail_with_compensation` (`mod.rs:925-945`); genuine
  lost lease keeps log-and-drop. Test added (§8 item 2).
- **NITs (subagent#4-7)** — §4.4 ordering-B justification reworded
  (kill-induced verdicts are legitimate failure outcomes, never false-green
  — a SIGKILLed child cannot exit 0; the race is over the failure flavor
  only); exit file written via tmp+rename, present-but-unparseable ⇒
  infra-fail (§6.1 step 3, §6.3); the 25ms per-waiter dead-probe cost folded
  into R4's backoff follow-up; M5 added to §10 (the issue's "deletes the
  re-drive arm entirely" overclaim — §6.2 keeps the pre-park strand; issue
  text to be corrected).

Round-3 verdicts: subagent APPROVE-WITH-NITS, codex APPROVE-WITH-NITS; nits
folded — PR-C test obligations (§8), boot-Fail `RecoveryMode` (§4.2),
SIGKILL-finality wording (§4.4), exit-file-over-signal preference
(§4.4 ordering B / §6.3).


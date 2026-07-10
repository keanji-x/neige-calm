use sqlx::Sqlite;
use sqlx::Transaction;

use crate::error::{CalmError, Result};
use crate::model::*;

// ---------------------------------------------------------------------------
// Tasks (issue #644 — wave-scoped task plan, migration 0041)
//
// The `_tx` helpers run inside the caller's eventized write so the row
// writes and the `plan.updated` event land (or roll back) together —
// same shape as `wave_update_tx` above. Reads are mirrored on
// `RepoRead` for the tool layer's pre-checks and `calm.plan.list`.
// ---------------------------------------------------------------------------

/// Shared SELECT column list for `tasks` rows. One spelling so the
/// `FromRow` mapping can't drift between the pool reads and the in-tx
/// reads.
pub(super) const TASK_COLUMNS: &str = "id, wave_id, key, kind, goal, context_json, acceptance_criteria, \
     cwd, depends_on_json, priority, gate_json, status, status_detail, worker_card_id, \
     gate_result_json, gate_attempt, gate_pid, gate_pid_starttime, gate_pid_boot_id, \
     running_deadline_ms, created_at_ms, updated_at_ms, finished_at_ms";

/// In-tx read of a wave's full plan, in scheduler order
/// (`priority DESC, created_at_ms ASC, key ASC` — design §5.2). Used by
/// `calm.plan.upsert` so dep/cycle/mutability validation sees state
/// consistent with the rows it is about to write.
pub async fn tasks_by_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<Vec<Task>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM tasks WHERE wave_id = ?1 \
         ORDER BY priority DESC, created_at_ms ASC, key ASC"
    );
    let rows = sqlx::query_as::<_, Task>(&sql)
        .bind(wave_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows)
}

/// Insert one fresh plan row (`status = 'pending'`). The caller
/// (`calm.plan.upsert`) has already validated key shape + per-wave
/// uniqueness inside the same tx; the `UNIQUE (wave_id, key)`
/// constraint backs that check, so a violation here is surfaced as a
/// conflict rather than swallowed.
pub async fn task_insert_tx(tx: &mut Transaction<'_, Sqlite>, t: &Task) -> Result<()> {
    let res = sqlx::query(
        r#"INSERT INTO tasks
           (id, wave_id, key, kind, goal, context_json, acceptance_criteria, cwd,
                depends_on_json, priority, gate_json, status, status_detail, worker_card_id,
                gate_result_json, gate_attempt, gate_pid, gate_pid_starttime, gate_pid_boot_id,
                running_deadline_ms, created_at_ms, updated_at_ms, finished_at_ms)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                   ?18, ?19, ?20, ?21, ?22, ?23)"#,
    )
    .bind(&t.id)
    .bind(&t.wave_id)
    .bind(&t.key)
    .bind(t.kind)
    .bind(&t.goal)
    .bind(&t.context_json)
    .bind(&t.acceptance_criteria)
    .bind(&t.cwd)
    .bind(&t.depends_on_json)
    .bind(t.priority)
    .bind(&t.gate_json)
    .bind(t.status)
    .bind(&t.status_detail)
    .bind(&t.worker_card_id)
    .bind(&t.gate_result_json)
    .bind(t.gate_attempt)
    .bind(t.gate_pid)
    .bind(t.gate_pid_starttime)
    .bind(&t.gate_pid_boot_id)
    .bind(t.running_deadline_ms)
    .bind(t.created_at_ms)
    .bind(t.updated_at_ms)
    .bind(t.finished_at_ms)
    .execute(&mut **tx)
    .await;
    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
            CalmError::Conflict(format!("tasks ({}, {}) already exists", t.wave_id, t.key)),
        ),
        Err(e) => Err(e.into()),
    }
}

/// Revise a still-`pending` plan row. Only the spec-revisable payload
/// columns move (design §4.1 rule 5: goal/context/acceptance/cwd/deps/
/// priority/gate); identity, status, and the gate bookkeeping columns
/// are untouched. Guarded `WHERE status = 'pending'`: a row that left
/// `pending` between the caller's in-tx read and this write surfaces as
/// `Conflict` so the whole batch rolls back instead of half-applying.
pub async fn task_update_pending_tx(tx: &mut Transaction<'_, Sqlite>, t: &Task) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET kind = ?1, goal = ?2, context_json = ?3, acceptance_criteria = ?4, cwd = ?5,
               depends_on_json = ?6, priority = ?7, gate_json = ?8, updated_at_ms = ?9
           WHERE id = ?10 AND status = 'pending'"#,
    )
    .bind(t.kind)
    .bind(&t.goal)
    .bind(&t.context_json)
    .bind(&t.acceptance_criteria)
    .bind(&t.cwd)
    .bind(&t.depends_on_json)
    .bind(t.priority)
    .bind(&t.gate_json)
    .bind(t.updated_at_ms)
    .bind(&t.id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::Conflict(format!(
            "task {} is no longer pending; concurrent state change",
            t.key
        )));
    }
    Ok(())
}

/// In-tx single-row read of one plan row. Used by `calm.plan.cancel`
/// to disambiguate a 0-row guarded flip (concurrent cancel → idempotent
/// success vs. concurrent dispatch → conflict) against state consistent
/// with the write it just attempted.
pub async fn task_get_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<Option<Task>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
    let row = sqlx::query_as::<_, Task>(&sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row)
}

/// In-tx wave-existence guard for the plan writers. `tasks.wave_id`
/// deliberately has no FK to `waves` (design §2 — events-outlive-rows
/// convention), so without this check a delete/upsert race could insert
/// plan rows for a wave whose row was just removed. Surfaced as
/// `Conflict` so the tool layer maps it onto the 409-style vocabulary.
pub async fn require_wave_exists_tx(tx: &mut Transaction<'_, Sqlite>, wave_id: &str) -> Result<()> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::Conflict(format!(
            "wave {wave_id} was deleted concurrently"
        )));
    }
    Ok(())
}

/// Guarded `pending → canceled` flip (design §3.1). Returns the number
/// of rows moved (`0` = the task was not `pending`; the caller decides
/// between idempotent success and the in-flight refusal).
pub async fn task_cancel_tx(tx: &mut Transaction<'_, Sqlite>, id: &str, now: i64) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'canceled', updated_at_ms = ?1, finished_at_ms = ?1
           WHERE id = ?2 AND status = 'pending'"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-B — in-tx read of one wave's lifecycle plus its raw
/// `task_budget` override. The scheduler's claim tx re-checks
/// schedulability against this (not the pre-claim snapshot) so a wave
/// moved to Blocked/Canceled/Done between the ready-set pass and the
/// claim can never have new work claimed (review F4), and the budget is
/// revalidated in the same tx so a PATCH that shrank it mid-window
/// cannot over-fill the wave (round-2 review F1). `None` = the wave row
/// is gone (concurrent delete); the inner `Option<i64>` is the nullable
/// `task_budget` column (NULL = kernel default).
pub async fn wave_lifecycle_and_budget_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<Option<(WaveLifecycle, Option<i64>)>> {
    // #679 PR1 — `WaveLifecycle` lost its `sqlx::Type` derive when it
    // moved to calm-types; decode TEXT and parse via `TryFrom<String>`.
    let row: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT lifecycle, task_budget FROM waves WHERE id = ?1")
            .bind(wave_id)
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(lifecycle, budget)| {
        WaveLifecycle::try_from(lifecycle)
            .map(|lifecycle| (lifecycle, budget))
            .map_err(|e| CalmError::Internal(format!("waves.lifecycle decode: {e}")))
    })
    .transpose()
}

/// Issue #644 PR-C — the wave-level gate policy flag
/// (`waves.require_task_gates`, §6.6), read inside `calm.plan.upsert`'s
/// tx for the rule-6 check. A gone wave row reads as `false` — the
/// caller's `require_wave_exists_tx` already errored that case loudly.
pub async fn wave_require_task_gates_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT require_task_gates FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.is_some_and(|(v,)| v != 0))
}

/// Issue #644 PR-B — the scheduler's single-winner claim
/// (`pending → dispatched`, design §5.4). Returns rows moved (`0` =
/// someone else won the claim; the caller skips silently). Runs inside
/// the same tx that appends `Event::TaskDispatched` and the
/// `Dispatching → Working` promotion so projections never observe a
/// claimed row without its dispatch record.
pub async fn task_claim_pending_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'dispatched',
               updated_at_ms = ?1
           WHERE id = ?2 AND status = 'pending'"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-B — the scheduler's post-spawn running stamp (design
/// §3/§5.4). Guarded `WHERE status = 'dispatched'`: a fast worker that
/// already reported (`done`/`failed`, or `verifying` once gates land)
/// makes this a no-op so the late scheduler write can never regress the
/// row. `worker_card_id` is `COALESCE`-stamped — whichever side (this
/// stamp or the report tx) lands first wins; neither overwrites.
pub async fn task_mark_running_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    worker_card_id: Option<&str>,
    now: i64,
    running_deadline_ms: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'running',
               worker_card_id = COALESCE(worker_card_id, ?1),
               running_deadline_ms = ?2,
               updated_at_ms = ?3
           WHERE id = ?4 AND status = 'dispatched'"#,
    )
    .bind(worker_card_id)
    .bind(running_deadline_ms)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Upgrade/lost-stamp backfill for agent tasks already running when
/// liveness deadlines were introduced. Guarded so terminal rows and
/// live-path stamped rows are untouched.
pub async fn task_stamp_missing_running_deadline_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    now: i64,
    running_deadline_ms: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET running_deadline_ms = ?1,
               updated_at_ms = ?2
           WHERE id = ?3
             AND kind IN ('codex', 'claude')
             AND status = 'running'
             AND running_deadline_ms IS NULL"#,
    )
    .bind(running_deadline_ms)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Round-4 review F1/F2 — durable ownership proof for the
/// unstamped-row window: is `card_id` the card the worker-spawn
/// operation for `task_id` actually created?
///
/// The worker-spawn op (`kind 'codex-worker' | 'terminal-worker' |
/// 'claude-worker'`,
/// `idempotency_key = task id`) records its created card as the
/// operation target: `prepare_tx_and_advance` stamps
/// `target_type = 'card'` / `target_id` in the SAME tx in which the
/// adapter's `prepare_tx` creates the card, and the operations table
/// has no client-reachable write path. Card payloads, by contrast,
/// stay patchable via `PATCH /api/cards/{id}` (the kind validators
/// allow extra fields), so a payload `idempotency_key` echo proves
/// nothing.
///
/// Round-5 review F2: the op must additionally be SCHEDULER-created —
/// its persisted `payload_json` actor is `ActorId::KernelDispatcher`
/// (`build_worker_payload` stamps it; serde shape
/// `{"actor":{"kind":"KernelDispatcher"}}`). A legacy
/// `calm.task.dispatch` operation carries the requesting envelope's
/// actor (the spec card, `{"kind":"AiSpec",...}`) and could otherwise
/// collide on the same idempotency key — that foreign op's worker card
/// must NOT be able to flip the plan task during the unstamped
/// `dispatched` window (the scheduler classifies the payload-hash
/// conflict as a permanent spawn failure instead).
///
/// Returns `false` when no scheduler worker op row targets the card —
/// including the crash window between the claim and the op insert,
/// where NO ownership is provable: unstamped reports are rejected
/// there, the sweep's dispatched arm resubmits the op, and the real
/// worker spawned by that resubmit can report.
pub async fn worker_op_targets_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    card_id: &str,
) -> Result<bool> {
    let owns: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM operations
               WHERE kind IN ('codex-worker', 'terminal-worker', 'claude-worker')
                 AND idempotency_key = ?1
                 AND target_type = 'card'
                 AND target_id = ?2
                 AND json_extract(payload_json, '$.actor.kind') = 'KernelDispatcher'
           )"#,
    )
    .bind(task_id)
    .bind(card_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(owns)
}

/// Who is asserting a worker-report flip (round-2 review F2).
///
/// The two-sided `worker_card_id` guard from round 1 only protects
/// rows that already carry a stamp; an UNSTAMPED `dispatched` row (the
/// report-beat-the-running-stamp window) would otherwise accept any
/// same-wave worker that echoes the task id. The ownership proof for
/// that window is the worker-spawn operation's immutable target card
/// ([`worker_op_targets_card_tx`], round-4 review F1/F2) — NOT the
/// reporting card's payload, which is mutable via
/// `PATCH /api/cards/{id}` and therefore forgeable.
#[derive(Clone, Copy, Debug)]
pub enum TaskReporter<'a> {
    /// Kernel-internal caller that owns the row by construction (the
    /// scheduler's spawn-failure reconcile). Bypasses the card guard
    /// and leaves `worker_card_id` untouched (NULL COALESCE arm).
    Kernel,
    /// A worker card's report. `owns_key` must be the result of
    /// [`worker_op_targets_card_tx`] for the REPORTING card — `true`
    /// is the unstamped-row ownership proof; stamped rows are still
    /// guarded by `worker_card_id = card_id`.
    Card { card_id: &'a str, owns_key: bool },
}

impl<'a> TaskReporter<'a> {
    /// `(card_id bind, owns_key bind)` for the shared SQL guard shape.
    fn binds(self) -> (Option<&'a str>, bool) {
        match self {
            TaskReporter::Kernel => (None, true),
            TaskReporter::Card { card_id, owns_key } => (Some(card_id), owns_key),
        }
    }
}

/// Issue #644 PR-B — worker-reported success flip
/// (`dispatched/running → done`, design §3), run **inside** the
/// `calm.task.complete` emit tx (and by the terminal-exit completion
/// paths) so there is no event-persisted-but-row-stale crash window.
///
/// `dispatched` is included because a fast worker can report before the
/// scheduler's `wait()` returns. `gate_json IS NULL` is load-bearing
/// since PR-C: a gated row goes to `verifying` (see
/// [`task_start_verifying_from_worker_tx`]), never straight to `done` —
/// the worker's self-report is a claim, not evidence (§3/§6).
///
/// `wave_id` is part of the guard so a caller can never flip another
/// wave's row even if it echoes a foreign task id.
///
/// The card guard is two-sided (review F3 + round-2 F2 + round-4 F1):
/// besides the COALESCE stamp, a [`TaskReporter::Card`] caller only
/// flips a row whose `worker_card_id` matches it, or an unstamped row
/// when the reporting card proves op-target ownership (`owns_key`,
/// [`worker_op_targets_card_tx`]). A sibling worker echoing another
/// task's idempotency key — even via a forged card payload — can
/// therefore never terminalize that row, stamped or not.
/// [`TaskReporter::Kernel`] bypasses — reserved for kernel callers
/// that own the row.
pub async fn task_complete_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    now: i64,
) -> Result<u64> {
    let (worker_card_id, owns_key) = reporter.binds();
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'done',
               status_detail = NULL,
               worker_card_id = COALESCE(worker_card_id, ?1),
               updated_at_ms = ?2,
               finished_at_ms = ?2
           WHERE id = ?3 AND wave_id = ?4
             AND status IN ('dispatched', 'running')
             AND gate_json IS NULL
             AND (?1 IS NULL OR worker_card_id = ?1
                  OR (worker_card_id IS NULL AND ?5))"#,
    )
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(wave_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Which row flip a successful worker report performed (issue #644
/// PR-C). `Done` = ungated row terminalized; `Verifying` = gated row
/// handed to the gate runner (lifecycle promotion is suppressed — the
/// gate-result tx promotes instead, §3); `None` = the guarded UPDATEs
/// matched nothing (no row / already moved on / ownership miss — the
/// caller disambiguates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuccessReportFlip {
    Done,
    Verifying,
    None,
}

/// Issue #644 PR-C — worker-reported success flip for GATED rows
/// (`dispatched/running → verifying`, design §3): the same write that
/// persists the worker's `task.completed` hands the row to the gate
/// runner instead of terminalizing it. Identical guards to
/// [`task_complete_from_worker_tx`] except the gate condition is
/// inverted (`gate_json IS NOT NULL`). `gate_result_json` from any
/// prior wave of the plan is untouched (rows can only re-enter
/// `verifying` via a fresh report on a non-terminal row, which the
/// status guard already excludes).
pub async fn task_start_verifying_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    now: i64,
) -> Result<u64> {
    let (worker_card_id, owns_key) = reporter.binds();
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'verifying',
               status_detail = NULL,
               worker_card_id = COALESCE(worker_card_id, ?1),
               updated_at_ms = ?2
           WHERE id = ?3 AND wave_id = ?4
             AND status IN ('dispatched', 'running')
             AND gate_json IS NOT NULL
             AND (?1 IS NULL OR worker_card_id = ?1
                  OR (worker_card_id IS NULL AND ?5))"#,
    )
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(wave_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-C — the ONE success-report flip both report paths
/// (`calm.task.complete` emit tx, terminal-exit completion) run:
/// ungated rows terminalize (`done`), gated rows enter `verifying`.
/// The two guarded UPDATEs are mutually exclusive on `gate_json`, so
/// at most one matches.
pub async fn task_report_success_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    now: i64,
) -> Result<SuccessReportFlip> {
    if task_complete_from_worker_tx(tx, id, wave_id, reporter, now).await? > 0 {
        return Ok(SuccessReportFlip::Done);
    }
    if task_start_verifying_from_worker_tx(tx, id, wave_id, reporter, now).await? > 0 {
        return Ok(SuccessReportFlip::Verifying);
    }
    Ok(SuccessReportFlip::None)
}

/// Issue #644 PR-C — the gate adapter's guarded attempt bump (design
/// §6.2 `prepare_tx`): exactly one `task-verify` operation may prepare
/// attempt `N`, and only while the row is still `verifying`. 0 rows =
/// a different attempt won or the task moved on; the caller fails the
/// op benignly.
pub async fn task_gate_attempt_bump_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    attempt: i64,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET gate_attempt = ?1, updated_at_ms = ?2
           WHERE id = ?3 AND gate_attempt = ?4 AND status = 'verifying'"#,
    )
    .bind(attempt)
    .bind(now)
    .bind(id)
    .bind(attempt - 1)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-C — the gate-result flip
/// (`verifying → done|failed`, design §3/§6.2): records the verdict,
/// clears the gate-process bookkeeping triple, and stamps
/// `finished_at_ms`, guarded on `status = 'verifying'` AND the attempt
/// number so a superseded attempt's late observer writes nothing.
/// Callers append `Event::TaskGateResult` + the lifecycle promotion in
/// the SAME tx only when this returns 1.
pub async fn task_apply_gate_result_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    attempt: i64,
    passed: bool,
    status_detail: Option<&str>,
    gate_result_json: &str,
    now: i64,
) -> Result<u64> {
    let status = if passed { "done" } else { "failed" };
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = ?1,
               status_detail = ?2,
               gate_result_json = ?3,
               gate_pid = NULL,
               gate_pid_starttime = NULL,
               gate_pid_boot_id = NULL,
               updated_at_ms = ?4,
               finished_at_ms = ?4
           WHERE id = ?5 AND status = 'verifying' AND gate_attempt = ?6"#,
    )
    .bind(status)
    .bind(status_detail)
    .bind(gate_result_json)
    .bind(now)
    .bind(id)
    .bind(attempt)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-B — worker-reported / kernel-observed failure flip
/// (`dispatched/running → failed`, design §3). Same guards as the
/// success flip except the gate condition: a worker failure never runs
/// a gate (§3), so gated rows fail the same way. `status_detail`
/// distinguishes `'worker-reported'` (the worker said so, or its
/// terminal exited non-zero) from `'spawn-failed'` (the scheduler could
/// not start it).
///
/// `reporter` carries the same two-sided guard as the success flip
/// (review F3 + round-2 F2 + round-4 F1): a card only flips a
/// matching-stamp row or an unstamped row it proves op-target
/// ownership of; `Kernel` bypasses.
pub async fn task_fail_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    status_detail: &str,
    now: i64,
) -> Result<u64> {
    let (worker_card_id, owns_key) = reporter.binds();
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'failed',
               status_detail = ?1,
               worker_card_id = COALESCE(worker_card_id, ?2),
               updated_at_ms = ?3,
               finished_at_ms = ?3
           WHERE id = ?4 AND wave_id = ?5
             AND status IN ('dispatched', 'running')
             AND (?2 IS NULL OR worker_card_id = ?2
                  OR (worker_card_id IS NULL AND ?6))"#,
    )
    .bind(status_detail)
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(wave_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

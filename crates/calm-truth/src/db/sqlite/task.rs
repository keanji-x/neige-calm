use sqlx::Sqlite;
use sqlx::Transaction;

use crate::error::{CalmError, Result};
use crate::model::*;

/// One spelling so the `FromRow` mapping can't drift between pool and in-tx reads.
pub(super) const TASK_COLUMNS: &str = "id, track_id, key, kind, goal, context_json, acceptance_criteria, \
     cwd, depends_on_json, priority, gate_json, status, status_detail, worker_card_id, \
     gate_result_json, gate_attempt, gate_pid, gate_pid_starttime, gate_pid_boot_id, \
     running_deadline_ms, context_stale_at_ms, declared_by, spawn, created_at_ms, updated_at_ms, \
     finished_at_ms";

pub async fn tasks_by_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<Vec<Task>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM current_tasks WHERE track_id = ?1 \
         ORDER BY priority DESC, created_at_ms ASC, key ASC"
    );
    let rows = sqlx::query_as::<_, Task>(&sql)
        .bind(track_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows)
}

/// Guarded `WHERE status = 'pending'`: a row that left `pending` since the
/// caller's read surfaces as `Conflict` so the whole batch rolls back.
pub async fn task_update_pending_tx(tx: &mut Transaction<'_, Sqlite>, t: &Task) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET kind = ?1, goal = ?2, context_json = ?3, acceptance_criteria = ?4, cwd = ?5,
               depends_on_json = ?6, priority = ?7, gate_json = ?8, spawn = ?9, updated_at_ms = ?10
           WHERE id = ?11 AND status = 'pending'"#,
    )
    .bind(t.kind)
    .bind(&t.goal)
    .bind(&t.context_json)
    .bind(&t.acceptance_criteria)
    .bind(&t.cwd)
    .bind(&t.depends_on_json)
    .bind(t.priority)
    .bind(&t.gate_json)
    .bind(&t.spawn)
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

pub async fn task_get_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<Option<Task>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
    let row = sqlx::query_as::<_, Task>(&sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row)
}

/// Sub-track parents are long-lived orchestration rows, not workers. They do
/// not own a worker card and deliberately have no running deadline.
pub async fn task_mark_sub_track_running_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    now: i64,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE tasks SET status='running',worker_card_id=NULL,running_deadline_ms=NULL,updated_at_ms=?1 \
         WHERE id=?2 AND status='dispatched' AND spawn=?3 AND child_track_id IS NOT NULL",
    )
    .bind(now)
    .bind(id)
    .bind(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// `tasks.track_id` has no FK to `tracks`, so without this check a
/// delete/upsert race could insert plan rows for a removed track.
pub async fn require_track_exists_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<()> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::Conflict(format!(
            "track {track_id} was deleted concurrently"
        )));
    }
    Ok(())
}

/// Returns rows moved (`0` = the task was not `pending`; the caller decides).
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

/// `running → canceled` for the Planner's in-flight cancel (#1785). The card guard pins the
/// worker the caller marks for reaping; `dispatched` is excluded because its card may be unbound.
/// Returns rows moved (`0` = the row left `running` or changed worker; the caller re-reads).
pub async fn task_cancel_running_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    worker_card_id: &str,
    status_detail: &str,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'canceled', status_detail = ?1, updated_at_ms = ?2, finished_at_ms = ?2
           WHERE id = ?3 AND status = 'running' AND worker_card_id = ?4"#,
    )
    .bind(status_detail)
    .bind(now)
    .bind(id)
    .bind(worker_card_id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// The claim tx re-checks schedulability against this, not the pre-claim
/// snapshot. `None` = track row gone; inner `None` = NULL `task_budget`.
pub async fn track_lifecycle_and_budget_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<Option<(TrackLifecycle, Option<i64>)>> {
    let row: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT lifecycle, task_budget FROM tracks WHERE id = ?1")
            .bind(track_id)
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(lifecycle, budget)| {
        TrackLifecycle::try_from(lifecycle)
            .map(|lifecycle| (lifecycle, budget))
            .map_err(|e| CalmError::Internal(format!("tracks.lifecycle decode: {e}")))
    })
    .transpose()
}

/// A gone track row reads as `false`; `require_track_exists_tx` already errored that case.
pub async fn track_require_task_gates_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT require_task_gates FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.is_some_and(|(v,)| v != 0))
}

/// Single-winner claim `pending → dispatched`; `0` rows = someone else won.
/// Runs in the same tx as the dispatch event.
const TASK_CLAIM_PENDING_SQL: &str = r#"UPDATE tasks
           SET status = 'dispatched',
               claim_context_json = ?1,
               context_closure_truncated = ?2,
               updated_at_ms = ?3
           WHERE id = ?4 AND status = 'pending'
             AND EXISTS (SELECT 1 FROM current_task_attempt_allocations a WHERE a.attempt_id=tasks.id)"#;

pub async fn task_claim_pending_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    now: i64,
    context: &[calm_types::event::TaskContextRef],
    closure_truncated: bool,
) -> Result<u64> {
    let context_json = serde_json::to_string(context)
        .map_err(|e| CalmError::Internal(format!("serialize task context: {e}")))?;
    let res = sqlx::query(TASK_CLAIM_PENDING_SQL)
        .bind(&context_json)
        .bind(i64::from(closure_truncated))
        .bind(now)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() != 0 {
        sqlx::query("DELETE FROM task_ref_index WHERE task_id = ?1")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        for reference in context {
            sqlx::query(
                "INSERT INTO task_ref_index (task_id, dst_track_id, block_id) VALUES (?1, ?2, ?3)",
            )
            .bind(id)
            .bind(reference.track_id.as_str())
            .bind(&reference.block_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(res.rows_affected())
}

#[cfg(test)]
mod claim_sql_tests {
    use super::TASK_CLAIM_PENDING_SQL;

    #[test]
    fn pending_claim_has_no_context_stale_predicate() {
        assert!(!TASK_CLAIM_PENDING_SQL.contains("context_stale_at_ms"));
    }
}

/// Guarded on `dispatched` so a fast worker's report is never regressed;
/// `worker_card_id` is COALESCE-stamped — whichever side lands first wins.
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

/// Backfill for agent tasks already running when liveness deadlines were introduced.
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

/// Ownership proof for the unstamped-row window: the scheduler-created
/// worker-spawn op (actor `KernelDispatcher`, `idempotency_key` = task id)
/// records its created card as the op target in the same tx, and `operations`
/// has no client-reachable write path — unlike card payloads. `false` in the
/// crash window between the claim and the op insert.
pub async fn worker_op_targets_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    card_id: &str,
) -> Result<bool> {
    let owns: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM operations
               WHERE kind IN ('codex-worker', 'terminal-worker', 'claude-worker', 'codex-isolated-worker')
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

/// Who is asserting a worker-report flip. An UNSTAMPED `dispatched` row needs
/// the op-target proof, not the reporting card's (forgeable) payload.
#[derive(Clone, Copy, Debug)]
pub enum TaskReporter<'a> {
    /// Kernel-internal caller that owns the row by construction; bypasses the card guard.
    Kernel,
    /// `owns_key` must be [`worker_op_targets_card_tx`] for the REPORTING card.
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

/// `dispatched/running → done`, run inside the emit tx. `dispatched` is
/// included because a fast worker can report before the scheduler's `wait()`
/// returns; `gate_json IS NULL` because a gated row goes to `verifying`, never
/// straight to `done`. `track_id` plus the two-sided card guard keep a sibling
/// worker from ever terminalizing another task's row.
pub async fn task_complete_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_id: &str,
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
           WHERE id = ?3 AND track_id = ?4
             AND status IN ('dispatched', 'running')
             AND gate_json IS NULL
             AND (?1 IS NULL OR worker_card_id = ?1
                  OR (worker_card_id IS NULL AND ?5))"#,
    )
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(track_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// `None` = the guarded UPDATEs matched nothing (no row / already moved on /
/// ownership miss — the caller disambiguates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuccessReportFlip {
    Done,
    Verifying,
    None,
}

/// Same guards as [`task_complete_from_worker_tx`] with the gate condition inverted.
pub async fn task_start_verifying_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_id: &str,
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
           WHERE id = ?3 AND track_id = ?4
             AND status IN ('dispatched', 'running')
             AND gate_json IS NOT NULL
             AND (?1 IS NULL OR worker_card_id = ?1
                  OR (worker_card_id IS NULL AND ?5))"#,
    )
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(track_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// The two guarded UPDATEs are mutually exclusive on `gate_json`, so at most one matches.
pub async fn task_report_success_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_id: &str,
    reporter: TaskReporter<'_>,
    now: i64,
) -> Result<SuccessReportFlip> {
    if task_complete_from_worker_tx(tx, id, track_id, reporter, now).await? > 0 {
        return Ok(SuccessReportFlip::Done);
    }
    if task_start_verifying_from_worker_tx(tx, id, track_id, reporter, now).await? > 0 {
        return Ok(SuccessReportFlip::Verifying);
    }
    Ok(SuccessReportFlip::None)
}

/// Exactly one `task-verify` op may prepare attempt `N`, and only while the
/// row is still `verifying`; 0 rows = the caller fails the op benignly.
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

/// Guarded on `verifying` AND the attempt number so a superseded attempt's
/// late observer writes nothing; callers append the event only when this returns 1.
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

/// The `status_detail` classifier of a gated row the Planner released by abandoning its failed
/// Git delivery (#1727 S4 slice 3). Not a pre-gate class: the dispatcher does not push the
/// `task.failed` that carries it (the tool receipt is the Planner's answer).
pub const TASK_STATUS_DETAIL_DELIVERY_ABANDONED: &str = "delivery-abandoned";

/// `verifying → failed/delivery-abandoned` for a gated row whose Git delivery the Planner gave
/// up (#1727 S4 slice 3): the budget release of `calm.task.delivery{abandon}`. Clears the same
/// gate-process columns as `task_apply_gate_result_tx` so a gate still running is orphaned from
/// the row (its late verdict misses the `verifying` guard and writes nothing). Guarded on
/// `verifying` alone — not on `gate_attempt`, which a still-running gate holds — and never on
/// `dispatched | running`, which `task_fail_from_worker_tx` owns. `0` rows = the gate already
/// flipped the row (`already_terminal` to the caller).
pub async fn task_abandon_delivery_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_id: &str,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'failed',
               status_detail = ?1,
               gate_pid = NULL,
               gate_pid_starttime = NULL,
               gate_pid_boot_id = NULL,
               updated_at_ms = ?2,
               finished_at_ms = ?2
           WHERE id = ?3 AND track_id = ?4 AND status = 'verifying'"#,
    )
    .bind(TASK_STATUS_DETAIL_DELIVERY_ABANDONED)
    .bind(now)
    .bind(id)
    .bind(track_id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Operation `last_error` texts are unbounded; the row only needs the readable head.
const STATUS_DETAIL_REASON_MAX: usize = 480;

/// `status_detail` is a CLASSIFIER followed by an optional `": "` + reason
/// tail; dispatch on the class, never on the whole string.
pub fn status_detail_class(detail: &str) -> &str {
    detail.split_once(": ").map_or(detail, |(class, _)| class)
}

/// An empty reason degrades to the bare classifier; the tail is single-line
/// and at most [`STATUS_DETAIL_REASON_MAX`] chars INCLUDING the ellipsis.
pub fn status_detail_with_reason(class: &str, reason: &str) -> String {
    // Fail-closed: a classifier containing the separator would parse as a prefix of itself.
    debug_assert!(
        !class.contains(": "),
        "status_detail classifier must not contain the \": \" separator: {class:?}"
    );
    let tail = fold_reason_tail(reason.chars());
    if tail.is_empty() {
        return class.to_string();
    }
    format!("{class}: {tail}")
}

/// Takes an iterator on purpose: the test counts how much input the fold
/// pulls. One streaming pass (`last_error` is unbounded), and once the budget
/// is met the fold pulls AT MOST ONE more char.
fn fold_reason_tail(reason: impl Iterator<Item = char>) -> String {
    let mut tail = String::new();
    let mut len = 0usize;
    let mut pending_space = false;
    let mut truncated = false;
    for ch in reason {
        if ch.is_whitespace() {
            if len >= STATUS_DETAIL_REASON_MAX {
                // Budget full: bail rather than scan the rest of the whitespace run. A reason
                // ending in whitespace is conservatively marked truncated.
                truncated = true;
                break;
            }
            pending_space = len > 0;
            continue;
        }
        if len + usize::from(pending_space) + 1 > STATUS_DETAIL_REASON_MAX {
            truncated = true;
            break;
        }
        if pending_space {
            tail.push(' ');
            len += 1;
            pending_space = false;
        }
        tail.push(ch);
        len += 1;
    }
    if truncated && !tail.is_empty() {
        // Make room so the ellipsis fits INSIDE the budget.
        while len >= STATUS_DETAIL_REASON_MAX {
            tail.pop();
            len -= 1;
        }
        tail.push('…');
    }
    tail
}

/// `dispatched/running → failed`. A worker failure never runs a gate, so gated
/// rows fail the same way; `reporter` carries the same guard as the success flip.
pub async fn task_fail_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_id: &str,
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
           WHERE id = ?4 AND track_id = ?5
             AND status IN ('dispatched', 'running')
             AND (?2 IS NULL OR worker_card_id = ?2
                  OR (worker_card_id IS NULL AND ?6))"#,
    )
    .bind(status_detail)
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(track_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

#[cfg(test)]
mod status_detail_tests {
    use super::{
        STATUS_DETAIL_REASON_MAX, fold_reason_tail, status_detail_class, status_detail_with_reason,
    };

    #[test]
    fn class_is_the_head_and_survives_a_reason_tail() {
        assert_eq!(status_detail_class("spawn-failed"), "spawn-failed");
        assert_eq!(
            status_detail_class(
                "spawn-failed: track w1 cwd /home/kenji is not a git repository: x"
            ),
            "spawn-failed"
        );
        // A bare colon (no space) is not a separator.
        assert_eq!(status_detail_class("gate-red"), "gate-red");
        assert_eq!(status_detail_class("weird:thing"), "weird:thing");
    }

    #[test]
    fn empty_reason_degrades_to_the_bare_classifier() {
        assert_eq!(
            status_detail_with_reason("spawn-failed", ""),
            "spawn-failed"
        );
        assert_eq!(
            status_detail_with_reason("spawn-failed", "  \n "),
            "spawn-failed"
        );
    }

    #[test]
    fn reason_is_single_line_bounded_and_round_trips_through_the_class() {
        let detail = status_detail_with_reason("spawn-failed", " not a\n  git repo ");
        assert_eq!(detail, "spawn-failed: not a git repo");
        assert_eq!(status_detail_class(&detail), "spawn-failed");

        let long = "错误".repeat(4000);
        let detail = status_detail_with_reason("spawn-failed", &long);
        assert_eq!(status_detail_class(&detail), "spawn-failed");
        let tail = detail.strip_prefix("spawn-failed: ").expect("tail");
        assert_eq!(tail.chars().count(), STATUS_DETAIL_REASON_MAX);
        assert!(tail.ends_with('…'));

        let spaced = "错误 ".repeat(4000);
        let tail = status_detail_with_reason("spawn-failed", &spaced)
            .strip_prefix("spawn-failed: ")
            .expect("tail")
            .to_string();
        assert!(tail.chars().count() <= STATUS_DETAIL_REASON_MAX);
        assert!(tail.ends_with('…'));
        assert!(!tail.contains("  "), "runs of whitespace must fold");
    }

    #[test]
    fn exactly_full_reason_is_not_marked_truncated() {
        let exact = "x".repeat(STATUS_DETAIL_REASON_MAX);
        let detail = status_detail_with_reason("spawn-failed", &exact);
        let tail = detail.strip_prefix("spawn-failed: ").expect("tail");
        assert_eq!(tail.chars().count(), STATUS_DETAIL_REASON_MAX);
        assert!(!tail.ends_with('…'), "nothing was dropped, got {tail:?}");

        let one_more = "x".repeat(STATUS_DETAIL_REASON_MAX + 1);
        let detail = status_detail_with_reason("spawn-failed", &one_more);
        let tail = detail.strip_prefix("spawn-failed: ").expect("tail");
        assert_eq!(tail.chars().count(), STATUS_DETAIL_REASON_MAX);
        assert!(tail.ends_with('…'));
    }

    /// Deliberately NOT a wall-clock test: any non-flaky timing bound is loose
    /// enough to let the regression through.
    #[test]
    fn stops_pulling_once_the_budget_is_met() {
        // The long WHITESPACE run separates "stops at the budget" from "keeps
        // scanning"; the trailing `y` proves the run was skipped, not merely ended.
        let pathological = format!(
            "{}{}y",
            "x".repeat(STATUS_DETAIL_REASON_MAX),
            " ".repeat(50_000_000),
        );
        let pulled = std::cell::Cell::new(0usize);
        let tail = fold_reason_tail(pathological.chars().inspect(|_| {
            pulled.set(pulled.get() + 1);
        }));
        assert!(
            pulled.get() <= STATUS_DETAIL_REASON_MAX + 1,
            "fold pulled {} chars for a {}-char budget; it must stop at the \
             budget instead of walking the whitespace run",
            pulled.get(),
            STATUS_DETAIL_REASON_MAX,
        );
        assert_eq!(tail.chars().count(), STATUS_DETAIL_REASON_MAX);
        assert!(tail.ends_with('…'), "the `y` was dropped, so say so");

        let trailing_only = format!(
            "{}{}",
            "x".repeat(STATUS_DETAIL_REASON_MAX),
            " ".repeat(50_000_000),
        );
        let pulled = std::cell::Cell::new(0usize);
        let tail = fold_reason_tail(trailing_only.chars().inspect(|_| {
            pulled.set(pulled.get() + 1);
        }));
        assert!(
            pulled.get() <= STATUS_DETAIL_REASON_MAX + 1,
            "a whitespace-only tail must not be walked either, pulled {}",
            pulled.get(),
        );
        assert_eq!(tail.chars().count(), STATUS_DETAIL_REASON_MAX);
    }

    #[test]
    fn pathological_reason_stays_bounded_end_to_end() {
        let pathological = format!(
            "{}{}y",
            "x".repeat(STATUS_DETAIL_REASON_MAX),
            " ".repeat(50_000_000),
        );
        let started = std::time::Instant::now();
        let detail = status_detail_with_reason("spawn-failed", &pathological);
        let elapsed = started.elapsed();
        let tail = detail.strip_prefix("spawn-failed: ").expect("tail");
        assert_eq!(tail.chars().count(), STATUS_DETAIL_REASON_MAX);
        // Smoke only — `stops_pulling_once_the_budget_is_met` owns the real oracle.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "took {elapsed:?}"
        );
    }

    // `debug_assert!` compiles away in release, where `should_panic` would fail.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "must not contain")]
    fn classifier_carrying_the_separator_is_rejected_in_debug() {
        let _ = status_detail_with_reason("spawn-failed: oops", "boom");
    }
}

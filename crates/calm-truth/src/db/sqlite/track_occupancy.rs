//! #1830 S2 D5, #1917: who is using a track's checkout, and which pending tasks may start in it.
//! The scheduler's ready set, its claim transaction and the `trackBusy` pending reason all read
//! [`checkout_occupancy`] and [`checkout_admission`]; nothing else decides it.

use std::collections::BTreeSet;

use sqlx::SqliteConnection;

use super::task::TASK_COLUMNS;
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskAccess, TaskStatus};

/// Who is using a track's checkout, apart from the attempt asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckoutOccupancy {
    /// Nothing.
    Free,
    /// Read-only attempts only.
    Readers,
    /// An attempt that changes the checkout, or a delivery not yet landed.
    Busy,
}

impl CheckoutOccupancy {
    /// A task that changes the checkout needs it free; a read-only task may join other readers.
    pub fn admits(self, access: TaskAccess) -> bool {
        match self {
            CheckoutOccupancy::Free => true,
            CheckoutOccupancy::Readers => access == TaskAccess::ReadOnly,
            CheckoutOccupancy::Busy => false,
        }
    }

    fn join(self, access: TaskAccess) -> Self {
        match (self, access) {
            (CheckoutOccupancy::Busy, _) | (_, TaskAccess::ReadWrite) => CheckoutOccupancy::Busy,
            (_, TaskAccess::ReadOnly) => CheckoutOccupancy::Readers,
        }
    }
}

/// The checkout's occupancy, ignoring `except_attempt`. Three terms, each covering what the others
/// miss: an in-tree worker task that is `dispatched`/`running`/`verifying` (a gate reading the tree
/// after the release; a claim before its lease exists); a lease `held`/`releasing` unless its owner
/// op is `stuck` (a canceled worker not yet killed); a delivery not yet settled (a commit not yet
/// landed, always `Busy`). Each task and lease counts with its own access.
pub async fn checkout_occupancy(
    conn: &mut SqliteConnection,
    track_id: &str,
    except_attempt: &str,
) -> Result<CheckoutOccupancy> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM current_tasks WHERE track_id = ?1 AND id <> ?2 \
         AND status IN ('dispatched','running','verifying')"
    );
    let in_flight = sqlx::query_as::<_, Task>(&sql)
        .bind(track_id)
        .bind(except_attempt)
        .fetch_all(&mut *conn)
        .await?;
    let mut occupancy = CheckoutOccupancy::Free;
    for task in in_flight
        .iter()
        .filter(|task| task.runs_in_track_checkout())
    {
        occupancy = occupancy.join(task.access);
    }
    // `'stuck'` is the operations phase of an owner whose outcome is unknown (`PhaseTag::Stuck`).
    let lease_modes: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT wl.access_mode FROM workspace_leases wl \
         LEFT JOIN operations o ON o.id = wl.lease_owner \
         WHERE wl.track_id = ?1 AND wl.state IN ('held','releasing') \
         AND o.idempotency_key IS NOT ?2 \
         AND o.phase IS NOT 'stuck'",
    )
    .bind(track_id)
    .bind(except_attempt)
    .fetch_all(&mut *conn)
    .await?;
    for mode in lease_modes {
        occupancy = occupancy.join(TaskAccess::try_from(mode).map_err(CalmError::Internal)?);
    }
    let delivery_unsettled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_git_deliveries \
         WHERE track_id = ?1 AND settlement IS NULL AND producer_attempt_id <> ?2)",
    )
    .bind(track_id)
    .bind(except_attempt)
    .fetch_one(&mut *conn)
    .await?;
    if delivery_unsettled {
        return Ok(CheckoutOccupancy::Busy);
    }
    Ok(occupancy)
}

/// Why a pending task whose dependencies are done may not start in the checkout yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckoutWait {
    /// The occupancy does not admit its access.
    InUse,
    /// A task that changes the checkout is waiting ahead of this read-only one (no starvation).
    WriterAhead,
}

/// The one admission rule over a track's tasks in scheduler order (`priority DESC, created_at_ms
/// ASC, key ASC`): every `pending` task whose dependencies are all `done`, with `None` when it may
/// be dispatched now and else why it waits. A child-track task never waits for the checkout. A
/// checkout task (codex, claude or terminal) waits when `occupancy` does not admit its access;
/// once a task that changes the checkout waits, every later read-only task waits behind it.
///
/// Every admitted task is offered and its claim transaction re-runs this rule against the
/// occupancy apart from itself, so one claim that fails does not hold the others.
/// Dependencies order executions; `done` does not prove artifact delivery or Planner acceptance.
pub fn checkout_admission(
    tasks: &[Task],
    occupancy: CheckoutOccupancy,
) -> Vec<(&Task, Option<CheckoutWait>)> {
    let done_keys: BTreeSet<&str> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Done)
        .map(|task| task.key.as_str())
        .collect();
    let mut writer_waiting = false;
    let mut admission = Vec::new();
    for task in tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Pending)
    {
        if !task
            .depends_on()
            .iter()
            .all(|dependency| done_keys.contains(dependency.as_str()))
        {
            continue;
        }
        let wait = if !task.runs_in_track_checkout() {
            None
        } else if task.access == TaskAccess::ReadOnly && writer_waiting {
            Some(CheckoutWait::WriterAhead)
        } else if occupancy.admits(task.access) {
            None
        } else {
            writer_waiting |= task.access == TaskAccess::ReadWrite;
            Some(CheckoutWait::InUse)
        };
        admission.push((task, wait));
    }
    admission
}

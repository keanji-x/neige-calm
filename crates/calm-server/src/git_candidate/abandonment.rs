//! `task_git_delivery_abandonments`: the Planner's abandonment of one failed delivery
//! (`calm.task.delivery{action:"abandon"}`, #1727 S4 slice 3).
//!
//! One immutable row per abandoned delivery (`delivery_id` PK). `task_outcome` records what the
//! abandonment transaction did to the tasks row and `task_status` the status it observed; both are
//! written once and never derived again. `(producer_attempt_id, request_idempotency_key)` is the
//! replay key the action reads before any admission.

use sqlx::Row;

use super::view::AbandonmentFacts;
use crate::error::{CalmError, Result};
use crate::model::TaskStatus;
use crate::operation::Tx;

/// What `calm.task.delivery{abandon}` did to the tasks row in its own transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbandonTaskOutcome {
    /// A gated `verifying` row was flipped to `failed/delivery-abandoned` (budget released) and
    /// one `task.failed` appended.
    Failed,
    /// An ungated row (`done` since its report) was left alone; nothing else changed.
    DoneUnchanged,
    /// A gated row the gate had already flipped to `done | failed`; nothing was written.
    AlreadyTerminal,
}

impl AbandonTaskOutcome {
    pub(crate) fn wire_str(self) -> &'static str {
        match self {
            AbandonTaskOutcome::Failed => "failed",
            AbandonTaskOutcome::DoneUnchanged => "done_unchanged",
            AbandonTaskOutcome::AlreadyTerminal => "already_terminal",
        }
    }

    fn from_column(value: &str) -> Result<Self> {
        match value {
            "failed" => Ok(AbandonTaskOutcome::Failed),
            "done_unchanged" => Ok(AbandonTaskOutcome::DoneUnchanged),
            "already_terminal" => Ok(AbandonTaskOutcome::AlreadyTerminal),
            other => Err(CalmError::Internal(format!(
                "task_git_delivery_abandonments.task_outcome {other:?} is not an outcome"
            ))),
        }
    }
}

/// One `task_git_delivery_abandonments` row, every column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AbandonmentRow {
    pub delivery_id: String,
    pub track_id: String,
    pub producer_attempt_id: String,
    pub request_idempotency_key: String,
    pub reason: Option<String>,
    pub task_outcome: AbandonTaskOutcome,
    /// The tasks row status observed in the abandonment transaction (`done | failed`).
    pub task_status: TaskStatus,
    pub created_at_ms: i64,
}

impl AbandonmentRow {
    /// The facts the read-surface derivation (`view::delivery_state`) takes.
    pub(crate) fn facts(&self) -> AbandonmentFacts {
        AbandonmentFacts {
            reason: self.reason.clone(),
            task_outcome: self.task_outcome.wire_str().to_string(),
            task_status: task_status_wire(self.task_status).to_string(),
        }
    }
}

const ABANDONMENT_COLUMNS: &str = "delivery_id, track_id, producer_attempt_id, \
     request_idempotency_key, reason, task_outcome, task_status, created_at_ms";

/// Insert the abandonment row. The CHECKs refuse an outcome/status pair the transaction did not
/// produce; the PK refuses a second abandonment of the same delivery.
pub(crate) async fn insert_abandonment_tx(tx: &mut Tx<'_>, row: &AbandonmentRow) -> Result<()> {
    sqlx::query(
        "INSERT INTO task_git_delivery_abandonments (delivery_id, track_id, producer_attempt_id, \
         request_idempotency_key, reason, task_outcome, task_status, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(&row.delivery_id)
    .bind(&row.track_id)
    .bind(&row.producer_attempt_id)
    .bind(&row.request_idempotency_key)
    .bind(&row.reason)
    .bind(row.task_outcome.wire_str())
    .bind(row.task_status)
    .bind(row.created_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The abandonment of one delivery, if the Planner gave it up.
pub(crate) async fn abandonment_for_delivery_tx(
    tx: &mut Tx<'_>,
    delivery_id: &str,
) -> Result<Option<AbandonmentRow>> {
    let sql = format!(
        "SELECT {ABANDONMENT_COLUMNS} FROM task_git_delivery_abandonments WHERE delivery_id = ?1"
    );
    let row = sqlx::query(&sql)
        .bind(delivery_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_abandonment).transpose()
}

/// The abandonment one request key already produced for this attempt in this Track (the replay
/// key, Track-scoped like the retry row's).
pub(crate) async fn abandonment_by_request_key_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    producer_attempt_id: &str,
    request_idempotency_key: &str,
) -> Result<Option<AbandonmentRow>> {
    let sql = format!(
        "SELECT {ABANDONMENT_COLUMNS} FROM task_git_delivery_abandonments \
         WHERE track_id = ?1 AND producer_attempt_id = ?2 AND request_idempotency_key = ?3"
    );
    let row = sqlx::query(&sql)
        .bind(track_id)
        .bind(producer_attempt_id)
        .bind(request_idempotency_key)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_abandonment).transpose()
}

/// The serde spelling of a task status (`TaskStatus` is `rename_all = "lowercase"`).
pub(crate) fn task_status_wire(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Dispatched => "dispatched",
        TaskStatus::Running => "running",
        TaskStatus::Verifying => "verifying",
        TaskStatus::Done => "done",
        TaskStatus::Failed => "failed",
        TaskStatus::Canceled => "canceled",
    }
}

fn row_to_abandonment(row: sqlx::sqlite::SqliteRow) -> Result<AbandonmentRow> {
    let task_outcome: String = row.try_get("task_outcome")?;
    Ok(AbandonmentRow {
        delivery_id: row.try_get("delivery_id")?,
        track_id: row.try_get("track_id")?,
        producer_attempt_id: row.try_get("producer_attempt_id")?,
        request_idempotency_key: row.try_get("request_idempotency_key")?,
        reason: row.try_get("reason")?,
        task_outcome: AbandonTaskOutcome::from_column(&task_outcome)?,
        task_status: row.try_get("task_status")?,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

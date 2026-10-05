//! Terminal report admission, under the same write transaction as its effects.
use crate::db::sqlite::{TaskReporter, status_detail_class, task_get_tx};
use crate::error::{CalmError, Result};
use crate::event::{ArtifactRef, Event};
use crate::git_candidate::commit_message::DeliveryMessage;
use crate::git_candidate::delivery::AttemptOutcome;
use crate::model::TaskStatus;
use crate::operation::workspace_lease::ReleaseDelivery;

pub(super) const REPEATED: &str = "worker report: recorded outcome already admitted";

/// Returns true only for an already-recorded report of the same outcome.
/// An operation binding is immutable; card payload fields are not authority.
pub(super) async fn admit_worker_report_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: &str,
    track_id: &str,
    reporter: TaskReporter<'_>,
    success: bool,
) -> Result<bool> {
    let TaskReporter::Card { card_id, owns_key } = reporter else {
        return Err(CalmError::Forbidden("worker report requires a card".into()));
    };
    let scheduled_keys: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM operations WHERE target_type = 'card' \
         AND target_id = ?1 AND kind IN ('codex-worker', 'claude-worker', 'terminal-worker') \
         AND json_extract(payload_json, '$.actor.kind') = 'KernelDispatcher' \
         AND idempotency_key IS NOT NULL \
         UNION SELECT id FROM tasks WHERE worker_card_id = ?1",
    )
    .bind(card_id)
    .fetch_all(&mut **tx)
    .await?;
    if let Some(expected) = scheduled_keys.iter().find(|key| key.as_str() != task_id) {
        return Err(CalmError::Conflict(format!(
            "worker card {card_id} belongs to attempt_id {expected}; report with that attempt_id"
        )));
    }
    let Some(row) = task_get_tx(tx, task_id).await? else {
        // Pre-scheduler workers have no plan row. A scheduled worker must
        // never silently take this legacy path after a key/row mismatch.
        return if !scheduled_keys.is_empty() {
            Err(CalmError::NotFound(format!("task {task_id}")))
        } else {
            Ok(false)
        };
    };
    let owns = row
        .worker_card_id
        .as_deref()
        .map_or(owns_key, |id| id == card_id);
    if row.track_id != track_id || !owns {
        return Err(CalmError::Forbidden(format!(
            "task {task_id} is not owned by reporting card {card_id}; report rejected"
        )));
    }
    match row.status {
        TaskStatus::Dispatched | TaskStatus::Running => Ok(false),
        TaskStatus::Done | TaskStatus::Verifying if success => Ok(true),
        TaskStatus::Failed
            if !success
                && row.status_detail.as_deref().map(status_detail_class)
                    == Some("worker-reported") =>
        {
            Ok(true)
        }
        _ => Err(CalmError::Conflict(format!(
            "task {task_id} is {:?} ({}); worker report conflicts with its recorded outcome",
            row.status,
            row.status_detail.as_deref().unwrap_or("no detail")
        ))),
    }
}

/// How a worker says its attempt ended. Only a completion carries a commit message (#2139), so a
/// worker message on a failed attempt has no representation.
#[derive(Clone, Debug)]
pub enum WorkerTaskReport {
    /// `neige_task_done`.
    Completed {
        attempt_id: String,
        result: serde_json::Value,
        artifacts: Vec<ArtifactRef>,
        commit_message: DeliveryMessage,
    },
    /// `neige_task_fail`.
    Failed { attempt_id: String, reason: String },
}

impl WorkerTaskReport {
    pub(super) fn attempt_id(&self) -> &str {
        match self {
            WorkerTaskReport::Completed { attempt_id, .. }
            | WorkerTaskReport::Failed { attempt_id, .. } => attempt_id,
        }
    }

    /// The event the report emits: `task.completed` or `task.failed`.
    pub fn event(&self) -> Event {
        match self {
            WorkerTaskReport::Completed {
                attempt_id,
                result,
                artifacts,
                ..
            } => Event::TaskCompleted {
                idempotency_key: attempt_id.clone(),
                result: result.clone(),
                artifacts: artifacts.clone(),
                agent_message: None,
            },
            WorkerTaskReport::Failed { attempt_id, reason } => Event::TaskFailed {
                idempotency_key: attempt_id.clone(),
                reason: reason.clone(),
                details: None,
                agent_message: None,
            },
        }
    }

    /// How the release in the report transaction commits the attempt (#1830 S2 D7).
    pub(super) fn release_delivery(&self) -> ReleaseDelivery {
        match self {
            WorkerTaskReport::Completed {
                commit_message: DeliveryMessage::Kernel,
                ..
            } => ReleaseDelivery::Commit(AttemptOutcome::Completed),
            WorkerTaskReport::Completed {
                commit_message: DeliveryMessage::Worker(message),
                ..
            } => ReleaseDelivery::CommitWorkerMessage(message.clone()),
            WorkerTaskReport::Failed { .. } => ReleaseDelivery::Commit(AttemptOutcome::Failed),
        }
    }
}

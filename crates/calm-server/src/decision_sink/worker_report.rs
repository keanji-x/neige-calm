//! Terminal report admission, under the same write transaction as its effects.
use crate::db::sqlite::{
    WorkerBinding, WorkerOf, status_detail_class, task_get_tx, worker_binding_tx,
};
use crate::error::{CalmError, Result};
use crate::event::{ArtifactRef, Event};
use crate::git_candidate::commit_message::DeliveryMessage;
use crate::git_candidate::delivery::AttemptOutcome;
use crate::model::TaskStatus;
use crate::operation::workspace_lease::ReleaseDelivery;

pub(super) const REPEATED: &str = "worker report: recorded outcome already admitted";

/// Returns true only for an already-recorded report of the same outcome.
/// The reporting session's binding (#2493) is the authority; card payload fields are not.
pub(super) async fn admit_worker_report_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: &str,
    track_id: &str,
    card_id: &str,
    session_id: &str,
    success: bool,
) -> Result<bool> {
    // The MCP identity is active at its handshake (only active tokens resolve); a report racing
    // the session's end is refused (NoSession below), as intended. `Live` and `Parked` both name the one attempt this session serves: a terminal
    // attempt answers a repeated report below; `Unbound` is a worker with no attempt.
    let bound = match worker_binding_tx(tx, WorkerOf::Session(session_id)).await? {
        WorkerBinding::Live { attempt_id, .. }
        | WorkerBinding::Parked {
            last_attempt_id: attempt_id,
            ..
        } => Some(attempt_id),
        WorkerBinding::Unbound { .. } => None,
        WorkerBinding::NoSession => {
            return Err(CalmError::Forbidden(format!(
                "worker session {session_id} has ended; report rejected"
            )));
        }
    };
    if let Some(expected) = bound.as_deref().filter(|bound| *bound != task_id) {
        return Err(CalmError::Conflict(format!(
            "worker card {card_id} belongs to attempt_id {expected}; report with that attempt_id"
        )));
    }
    let Some(row) = task_get_tx(tx, task_id).await? else {
        // Pre-scheduler workers have no plan row. A bound worker must never silently take this
        // legacy path after a key/row mismatch.
        return if bound.is_some() {
            Err(CalmError::NotFound(format!("task {task_id}")))
        } else {
            Ok(false)
        };
    };
    if row.track_id != track_id || bound.is_none() {
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

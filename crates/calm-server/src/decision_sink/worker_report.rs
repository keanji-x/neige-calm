//! Terminal report admission, under the same write transaction as its effects.
use crate::db::sqlite::{TaskReporter, status_detail_class, task_get_tx, task_result_contract_tx};
use crate::error::{CalmError, Result};
use crate::event::{ArtifactRef, Event};
use crate::git_candidate::commit_message::CommitMessage;
use crate::git_candidate::delivery::AttemptOutcome;
use crate::ids::TrackId;
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::{CardRole, TaskStatus};
use crate::operation::workspace_lease::ReleaseDelivery;
use calm_truth::decision_gate::{GateDecision, PrincipalDecisionGate};
use sqlx::{Sqlite, Transaction};

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

/// #2599: a completion of a task declared with a `result_contract` must carry a result of that
/// shape. Run after admission, inside the report transaction: a refusal rolls it back, so the
/// attempt stays running with its lease held. The tasks row is the authority, never the card
/// payload; a rowless legacy worker has no contract.
pub(super) async fn check_result_contract_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: &str,
    report: &WorkerTaskReport,
) -> Result<()> {
    let WorkerTaskReport::Completed { result, .. } = report else {
        return Ok(());
    };
    let Some(contract) = task_result_contract_tx(tx, task_id).await? else {
        return Ok(());
    };
    contract
        .check(result)
        .map_err(|refusal| CalmError::BadRequest(format!("neige_task_done: {refusal}")))
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
        commit_message: Option<CommitMessage>,
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

    /// Done's explicit input applies to a delivery commit; existing run commits keep their text.
    pub(super) fn release_delivery(&self) -> ReleaseDelivery {
        match self {
            Self::Completed {
                commit_message: Some(message),
                ..
            } => ReleaseDelivery::CommitWorkerMessage(message.clone()),
            Self::Completed { .. } => ReleaseDelivery::Commit(AttemptOutcome::Completed),
            Self::Failed { .. } => ReleaseDelivery::Commit(AttemptOutcome::Failed),
        }
    }
}

/// Worker stamping and reporting must re-prove the same session/card/role admission.
pub(crate) async fn admit_worker_write_tx(
    tx: &mut Transaction<'_, Sqlite>,
    identity: &ToolCallIdentity,
    track: &TrackId,
) -> Result<(), CalmError> {
    if identity.role != CardRole::Worker {
        return Err(CalmError::Forbidden(
            "worker write requires Worker identity".into(),
        ));
    }
    let principal = identity
        .to_principal()
        .ok_or_else(|| CalmError::Forbidden("worker write requires an agent session".into()))?;
    match PrincipalDecisionGate::new(principal)
        .decide_card_write(
            tx,
            track,
            &identity.card_id.clone().into(),
            CardRole::Worker,
        )
        .await?
    {
        GateDecision::Allow => Ok(()),
        GateDecision::Deny(reason) => Err(CalmError::Forbidden(format!(
            "worker write denied: {reason}"
        ))),
    }
}

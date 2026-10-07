//! Reconcile durable native ownership before the kernel transfers or reissues queue entries.
use crate::db::Repo;
use crate::db::sqlite::{AcpSubmission, AcpSubmissionState};
use crate::error::{CalmError, Result};
use crate::harness::{HarnessPhaseTag, HarnessSnapshot};
use serde_json::Value;

async fn checkpoint_receipt<'e, E>(
    executor: E,
    worker: &str,
    snapshot: &HarnessSnapshot,
) -> Result<Option<AcpSubmission>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let client = snapshot.projection_client_id.as_ref().map(|id| id.as_str());
    Ok(sqlx::query_as(concat!(
        "SELECT worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,outcome_json ",
        "FROM acp_submissions WHERE worker_session_id=?1 AND ",
        "((?2 IS NOT NULL AND client_id=?2) OR (?2 IS NULL AND state IN ('sending','unknown'))) ",
        "ORDER BY created_at_ms DESC LIMIT 1"
    )).bind(worker).bind(client).fetch_optional(executor).await?)
}

fn retire(snapshot: &mut HarnessSnapshot, receipt: &AcpSubmission) -> Result<bool> {
    let input: Value = serde_json::from_str(&receipt.input_json)?;
    let claims = input
        .get("queue")
        .ok_or_else(|| CalmError::Conflict("ACP receipt has no queue ownership proof".into()))?;
    crate::harness::submission_claims::retire(snapshot, &receipt.client_id, claims)
}

/// A reset inherits only input still owned by the queue, never input already dispatched.
pub(crate) async fn retire_before_transfer(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker: &str,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    if let Some(receipt) = checkpoint_receipt(&mut **tx, worker, snapshot).await? {
        retire(snapshot, &receipt)?;
    }
    Ok(())
}

/// The caller has claimed recovery and stopped any predecessor before this runs.
pub(crate) async fn recover(
    repo: &dyn Repo,
    worker: &str,
    card: &str,
    track: &str,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    let pool = repo
        .sqlite_pool()
        .ok_or_else(|| CalmError::Internal("ACP recovery requires durable storage".into()))?;
    let Some(receipt) = checkpoint_receipt(&pool, worker, snapshot).await? else {
        return Ok(());
    };
    retire(snapshot, &receipt)?;
    let outcome = match receipt.state {
        AcpSubmissionState::Completed => {
            serde_json::from_str(receipt.outcome_json.as_deref().ok_or_else(|| {
                CalmError::Conflict("ACP completed receipt has no outcome".into())
            })?)?
        }
        AcpSubmissionState::Sending | AcpSubmissionState::Unknown => {
            crate::db::sqlite::acp_submission_finish(&pool, worker, &receipt.client_id, None)
                .await?;
            super::session::unknown_outcome(&receipt.turn_id)
        }
    };
    crate::harness::turn_outcome::record(
        repo,
        worker,
        card,
        track,
        &receipt.thread_id,
        &receipt.turn_id,
        &outcome,
    )
    .await?;
    snapshot.phase = HarnessPhaseTag::TurnCompleted;
    snapshot.last_thread_id = Some(receipt.thread_id);
    snapshot.last_turn_id = Some(receipt.turn_id);
    snapshot.projection_client_id = None;
    snapshot.issued_input_segments = None;
    Ok(())
}

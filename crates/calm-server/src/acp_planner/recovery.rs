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
    let sql = format!(
        concat!(
            "SELECT worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,outcome_json,kept_cleanup_json ",
            "FROM acp_submissions WHERE worker_session_id=?1 AND ",
            "((?2 IS NOT NULL AND client_id=?2) OR (?2 IS NULL AND ",
            "(({blocking}) OR turn_id=?3))) ",
            "ORDER BY created_at_ms DESC LIMIT 1"
        ),
        blocking = crate::db::sqlite::ACP_BLOCKING_RECEIPT_SQL
    );
    Ok(sqlx::query_as(&sql)
        .bind(worker)
        .bind(client)
        .bind(snapshot.last_turn_id.as_deref())
        .fetch_optional(executor)
        .await?)
}

fn retire(snapshot: &mut HarnessSnapshot, receipt: &AcpSubmission) -> Result<bool> {
    match crate::db::sqlite::AcpSubmissionInput::decode(&receipt.input_json)? {
        crate::db::sqlite::AcpSubmissionInput::Historical(input) => {
            crate::harness::submission_claims::retire(snapshot, &receipt.client_id, &input.queue)
        }
        crate::db::sqlite::AcpSubmissionInput::Canonical(_) => Err(CalmError::Conflict(
            "Canonical ACP receipts require retained recovery".into(),
        )),
    }
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

/// Apply the same receipt rule to every source the generic harvest can encounter.
pub(crate) async fn retire_card_sources(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
) -> Result<()> {
    let rows: Vec<(String, String)> = sqlx::query_as(concat!(
        "SELECT ws.id,ws.handle_state_json FROM worker_sessions ws ",
        "JOIN acp_managed_sessions owned ON owned.worker_session_id=ws.id ",
        "WHERE ws.card_id=?1 AND ws.handle_state_json IS NOT NULL"
    ))
    .bind(card)
    .fetch_all(&mut **tx)
    .await?;
    for (worker, json) in rows {
        let mut snapshot =
            HarnessSnapshot::parse_known(serde_json::from_str(&json)?).ok_or_else(|| {
                CalmError::Conflict("Managed dispatch source has an unsupported snapshot".into())
            })?;
        if let Some(receipt) = checkpoint_receipt(&mut **tx, &worker, &snapshot).await?
            && retire(&mut snapshot, &receipt)?
        {
            crate::db::sqlite::session_set_handle_state_of_any_runtime_tx(
                tx,
                &worker,
                Some(serde_json::to_value(&snapshot)?),
                crate::model::now_ms(),
            )
            .await?;
        }
    }
    Ok(())
}

/// The caller has claimed recovery and stopped any predecessor before this runs.
/// Read the complete checkpoint only after quiescence, never merge a live queue
/// into an old operation snapshot: its dispatch identity must travel with it.
pub(crate) async fn load_quiesced(
    repo: &dyn Repo,
    worker: &str,
    card: &str,
    track: &str,
) -> Result<HarnessSnapshot> {
    let json = repo
        .session_projection_handle_state_by_id(worker)
        .await?
        .ok_or_else(|| CalmError::Conflict("Managed session checkpoint is missing".into()))?;
    let mut snapshot = HarnessSnapshot::parse_known(json)
        .ok_or_else(|| CalmError::Conflict("Managed session checkpoint is unsupported".into()))?;
    recover(repo, worker, card, track, &mut snapshot).await?;
    Ok(snapshot)
}

pub(crate) async fn recover(
    repo: &dyn Repo,
    worker: &str,
    card: &str,
    track: &str,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    let pool = repo
        .sqlite_db()
        .ok_or_else(|| CalmError::Internal("ACP recovery requires durable storage".into()))?;
    let Some(receipt) = checkpoint_receipt(pool.read_pool(), worker, snapshot).await? else {
        return Ok(());
    };
    retire(snapshot, &receipt)?;
    let mut outcome = match receipt.state {
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
    if let Some(items) = outcome.get("items").and_then(Value::as_array) {
        crate::harness::transcript_receipt::restore(
            repo,
            worker,
            card,
            track,
            &receipt.thread_id,
            &receipt.turn_id,
            items,
        )
        .await?;
    }
    if let Some(outcome) = outcome.as_object_mut() {
        outcome.remove("items");
    }
    crate::harness::turn_outcome::record(
        repo,
        worker,
        card,
        track,
        &receipt.thread_id,
        &receipt.turn_id,
        &outcome,
        crate::harness::turn_outcome::Confirmation::ProviderTerminal,
    )
    .await?;
    snapshot.phase = HarnessPhaseTag::TurnCompleted;
    snapshot.last_thread_id = Some(receipt.thread_id);
    snapshot.last_turn_id = Some(receipt.turn_id);
    snapshot.projection_client_id = None;
    snapshot.issued_input_segments = None;
    Ok(())
}

/// Durable receipt fencing belongs to the worker, independent of queue and process liveness.
pub(crate) async fn submission_unresolved(
    repo: &dyn Repo,
    worker: &str,
    live_phase: Option<HarnessPhaseTag>,
) -> Result<bool> {
    // A sending receipt belongs to a healthy live dispatch until that dispatch ends.
    // A checkpoint's phase alone cannot prove liveness after boot or registry removal.
    if matches!(
        live_phase,
        Some(
            HarnessPhaseTag::IssuingTurn
                | HarnessPhaseTag::TurnRunning
                | HarnessPhaseTag::IssuingInterrupt
        )
    ) {
        return Ok(false);
    }
    let pool = repo
        .sqlite_db()
        .ok_or_else(|| CalmError::Internal("ACP recovery requires durable storage".into()))?;
    Ok(crate::db::sqlite::acp_submission_unresolved(pool.read_pool(), worker).await?)
}

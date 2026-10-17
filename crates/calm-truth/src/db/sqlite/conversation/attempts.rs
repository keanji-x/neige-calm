//! Crossing/report persistence in the existing harness checkpoint. The execution
//! adapter validates private native proofs before calling the fact writer; no
//! request DTO, retry enum or error diagnostic can enter this path.
use super::carrier::save_input_carrier_tx;
use super::{CurrentInputCarrier, submission_plan_tx};
use crate::error::{CalmError, Result};
use calm_types::conversation::{InputAttempt, InputAttemptCheckpoint, InputCorrelationFact};
use sqlx::{Sqlite, Transaction};

pub async fn begin_input_attempt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    attempt: InputAttempt,
    correlation: InputCorrelationFact,
) -> Result<()> {
    carrier.require_execution_current()?;
    let owns: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversation_input_submissions WHERE id=?1 AND card_id=?2)",
    )
    .bind(attempt.submission_id.as_str())
    .bind(&carrier.card_id)
    .fetch_one(&mut **tx)
    .await?;
    if !owns
        || carrier
            .checkpoint
            .active
            .as_ref()
            .is_none_or(|active| !active.submission_ids.contains(&attempt.submission_id))
    {
        return Err(CalmError::Conflict(
            "dispatch requires this carrier's active submission",
        ));
    }
    let claimed:i64=sqlx::query_scalar("SELECT count(*) FROM conversation_submission_inputs WHERE submission_id=?1 AND effective=1")
        .bind(attempt.submission_id.as_str()).fetch_one(&mut **tx).await?;
    let plan = submission_plan_tx(tx, &attempt.submission_id).await?;
    if usize::try_from(claimed).ok()
        != Some(plan.plan.input_refs().map_err(CalmError::Conflict)?.len())
    {
        return Err(CalmError::Conflict(
            "dispatch requires every exact input claim to remain effective",
        ));
    }
    let thread = carrier
        .snapshot
        .get("last_thread_id")
        .and_then(serde_json::Value::as_str);
    if thread != Some(correlation.thread_id.as_str()) {
        return Err(CalmError::Conflict(
            "dispatch thread differs from its current carrier",
        ));
    }
    carrier
        .checkpoint
        .begin_attempt(attempt, correlation)
        .map_err(CalmError::Conflict)?;
    save_input_carrier_tx(tx, carrier).await
}

pub async fn record_input_attempt_fact_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    report: InputAttemptCheckpoint,
) -> Result<()> {
    let owns: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversation_input_submissions WHERE id=?1 AND card_id=?2)",
    )
    .bind(report.attempt.submission_id.as_str())
    .bind(&carrier.card_id)
    .fetch_one(&mut **tx)
    .await?;
    if !owns {
        return Err(CalmError::Conflict(
            "input report belongs to another conversation",
        ));
    }
    carrier
        .checkpoint
        .record_attempt_fact(report)
        .map_err(CalmError::Conflict)?;
    save_input_carrier_tx(tx, carrier).await
}

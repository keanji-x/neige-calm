//! Exact confirmed non-delivery release. The caller supplies only attempt
//! identity; delivery proof is re-read from the existing owned checkpoint.
use super::carrier::save_input_carrier_tx;
use super::{
    CurrentInputCarrier, current_input_fact_carrier_tx, message_tx, revision_tx, submission_plan_tx,
};
use crate::error::{CalmError, Result};
use calm_types::conversation::{
    InputAttempt, InputDeliveryFact, PendingInput, PrivateHistoryPayload, SubmissionRenderInput,
    SubmissionRenderSegment,
};
use sqlx::{Row, Sqlite, Transaction};

pub async fn release_submission_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    attempt: &InputAttempt,
) -> Result<serde_json::Value> {
    // Discard a caller's cached facts, including any unpersisted local report or
    // queue view. Only the current fenced SQLite checkpoint authorizes release.
    *carrier =
        current_input_fact_carrier_tx(tx, &carrier.card_id, &carrier.worker_session_id).await?;
    let checkpoint = carrier
        .checkpoint
        .attempt(&attempt.submission_id)
        .ok_or_else(|| CalmError::Conflict("submission has no persisted execution evidence"))?;
    if carrier.checkpoint.is_kept(attempt)
        || checkpoint.attempt != *attempt
        || !matches!(checkpoint.delivery, InputDeliveryFact::NotDispatched { .. })
    {
        return Err(CalmError::Conflict(
            "only exact persisted confirmed non-delivery can release a submission",
        ));
    }
    let submission: Option<String> = sqlx::query_scalar(
        "SELECT logical_turn_id FROM conversation_input_submissions WHERE id=?1 AND card_id=?2",
    )
    .bind(attempt.submission_id.as_str())
    .bind(&carrier.card_id)
    .fetch_optional(&mut **tx)
    .await?;
    let turn_id = submission
        .ok_or_else(|| CalmError::Conflict("submission belongs to another conversation"))?;
    let stored = submission_plan_tx(tx, &attempt.submission_id).await?;
    let rows=sqlx::query("SELECT queue_position,queued_at_ms,envelope_id,system_addressable,effective,released_by_attempt_id
         FROM conversation_submission_inputs
         WHERE submission_id=?1 ORDER BY input_position")
        .bind(attempt.submission_id.as_str()).fetch_all(&mut **tx).await?;
    if rows.iter().all(|row| {
        row.get::<i64, _>("effective") == 0
            && row
                .get::<Option<String>, _>("released_by_attempt_id")
                .as_deref()
                == Some(attempt.attempt_id.as_str())
    }) {
        // A repeated release never restores a message withdrawn/edited afterwards.
        return Ok(carrier.snapshot.clone());
    }
    if rows.iter().any(|row| row.get::<i64, _>("effective") != 1) {
        return Err(CalmError::Conflict(
            "submission claims were already released by another attempt",
        ));
    }
    let active = carrier
        .checkpoint
        .active
        .as_ref()
        .ok_or_else(|| CalmError::Conflict("submission is not owned by this active carrier"))?;
    if active.logical_turn_id.as_str() != turn_id
        || !active.submission_ids.contains(&attempt.submission_id)
        || active.submission_ids.len() != 1
    {
        return Err(CalmError::Conflict(
            "submission release crosses the current logical Turn",
        ));
    }
    let mut restored = Vec::new();
    let inputs = stored
        .plan
        .segments()
        .iter()
        .filter_map(|segment| match segment {
            SubmissionRenderSegment::Input { input, .. } => Some(input),
            _ => None,
        });
    for (input, row) in inputs.zip(&rows) {
        let queue_position = u64::try_from(row.try_get::<i64, _>("queue_position")?)
            .map_err(|_| CalmError::Internal("invalid original queue order".into()))?;
        if queue_position >= carrier.checkpoint.next_queue_position {
            return Err(CalmError::Conflict(
                "carrier lost the original conversation queue allocation counter",
            ));
        }
        let queued_at_ms = row.try_get("queued_at_ms")?;
        let envelope_id = row.try_get("envelope_id")?;
        let entry = match input {
            SubmissionRenderInput::Human { message, .. } => {
                let current = message_tx(tx, &message.message_id).await?;
                if current.card_id != carrier.card_id {
                    return Err(CalmError::Conflict(
                        "release crosses canonical message ownership",
                    ));
                }
                if current.withdrawn_at_ms.is_some()
                    || current.current != *message
                    || !matches!(
                        revision_tx(tx, message).await?.payload,
                        PrivateHistoryPayload::Present { .. }
                    )
                {
                    continue;
                }
                PendingInput::Human {
                    message: message.clone(),
                    queue_position,
                    queued_at_ms,
                    envelope_id,
                }
            }
            SubmissionRenderInput::System {
                observation_id,
                payload: PrivateHistoryPayload::Present { value },
            } => PendingInput::System {
                observation_id: observation_id.clone(),
                observation: value.clone(),
                queue_position,
                queued_at_ms,
                envelope_id,
                addressable: row.try_get::<i64, _>("system_addressable")? == 1,
            },
            SubmissionRenderInput::System {
                payload: PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. },
                ..
            } => continue,
        };
        if carrier.checkpoint.pending.iter().any(|pending| {
            pending.queue_position() == queue_position
                || pending.input_ref().identity() == entry.input_ref().identity()
        }) {
            return Err(CalmError::Conflict(
                "release would duplicate or replace unrelated pending input",
            ));
        }
        restored.push(entry);
    }
    let changed=sqlx::query("UPDATE conversation_submission_inputs SET effective=0,released_by_attempt_id=?1 WHERE submission_id=?2 AND effective=1")
        .bind(attempt.attempt_id.as_str()).bind(attempt.submission_id.as_str()).execute(&mut **tx).await?;
    if usize::try_from(changed.rows_affected()).ok() != Some(rows.len()) {
        return Err(CalmError::Conflict(
            "submission claims changed during release",
        ));
    }
    carrier.checkpoint.pending.extend(restored);
    carrier
        .checkpoint
        .pending
        .sort_by_key(PendingInput::queue_position);
    carrier.checkpoint.active = None;
    // The existing runtime FSM settles phase/retry/rewind separately. A prompt
    // non-dispatch fact is not permission to clear an uncertain native cut.
    save_input_carrier_tx(tx, carrier).await?;
    Ok(carrier.snapshot.clone())
}

//! Retire a stale shared ACP Initial after the boot owner has stopped the prior
//! process generation. A completed receipt still belongs to ordinary END recovery.
use super::carrier::{restore_interrupt_timeout_carrier_tx, save_input_carrier_tx};
use super::*;
use crate::db::sqlite::{AcpSubmissionState, acp_submission_get, acp_submission_unresolved_tx};
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
use calm_types::harness::{HARNESS_INTERRUPT_TIMEOUT_REASON, HarnessPhaseTag};

/// Candidate inventory only. The release transaction rechecks the exact
/// carrier, original Initial, process provenance and native identity.
pub async fn shared_acp_restart_candidates_tx(
    tx: &mut Transaction<'_, Sqlite>,
    provider: &calm_types::runtime::AgentProvider,
) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT c.id,ws.id FROM acp_managed_sessions owned
         JOIN worker_sessions ws ON ws.id=owned.worker_session_id
         JOIN cards c ON c.session_id=ws.id AND c.id=ws.card_id AND c.track_id=ws.track_id
         JOIN tracks t ON t.id=c.track_id
         WHERE ws.provider=?1 AND ws.mode='resumable' AND t.closed_at IS NULL
           AND (ws.completed_at_ms IS NULL OR ws.state='failed')
           AND ws.queue_harvested_at_ms IS NULL
           AND NOT EXISTS(SELECT 1 FROM assistant_conversation_archives a WHERE a.card_id=c.id)
           AND ws.agent_session_id IS NOT NULL AND ws.handle_state_json IS NOT NULL
           AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='object'",
    )
    .bind(provider.wire_name())
    .fetch_all(&mut **tx)
    .await?)
}

/// The caller owns the successful instance-scoped boot sweep and exact receipt
/// validation. Releasing execution records no native result and restores no input.
pub async fn release_shared_acp_restart_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    worker: &str,
    thread: &str,
    expected_client: Option<&str>,
) -> Result<()> {
    let mut carrier = current_input_fact_carrier_tx(tx, card, worker).await?;
    let frame: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
    let header = &frame.header;
    let Some(initial) = original_initial_submission_tx(tx, &carrier).await? else {
        return Ok(());
    };
    if frame
        .canonical_inputs
        .active
        .as_ref()
        .is_none_or(|active| active.submission_ids.as_slice() != std::slice::from_ref(&initial))
        || header.last_thread_id.as_deref() != Some(thread)
        || header.pending_rewind.is_some()
        || super::super::provider_execution::retained(tx, worker).await?
    {
        return Ok(());
    }
    let receipt = acp_submission_get(&mut **tx, worker, initial.as_str()).await?;
    if receipt.as_ref().map(|receipt| receipt.client_id.as_str()) != expected_client {
        return Ok(());
    }
    let is_current = carrier.is_execution_current();
    let restore = !is_current
        && header.phase == HarnessPhaseTag::Wedged
        && header.interruption_intent.is_some()
        && header.wedged_reason.as_deref() == Some(HARNESS_INTERRUPT_TIMEOUT_REASON);
    let normal = is_current
        && header.wedged_reason.is_none()
        && matches!(
            header.phase,
            HarnessPhaseTag::IssuingTurn | HarnessPhaseTag::TurnRunning
        )
        && header.interruption_intent.is_none();
    let stopping = is_current
        && header.wedged_reason.is_none()
        && header.phase == HarnessPhaseTag::IssuingInterrupt
        && header.interruption_intent.is_some();
    if !(normal || stopping || restore) {
        return Ok(());
    }
    if let Some(receipt) = receipt {
        if receipt.thread_id != thread
            || matches!(receipt.state, AcpSubmissionState::Completed)
            || (header.phase != HarnessPhaseTag::IssuingTurn
                && header.last_turn_id.as_deref() != Some(receipt.turn_id.as_str()))
            || ((stopping || restore)
                && !super::runtime_frame::interruption_matches_tx(
                    tx,
                    &carrier,
                    thread,
                    &receipt.turn_id,
                )
                .await?)
            || super::super::acp_submission_other_blocking_tx(tx, worker, &receipt.client_id)
                .await?
        {
            return Ok(());
        }
        let released = sqlx::query(
            "UPDATE acp_submissions SET state='unknown',restart_released_at_ms=?3
             WHERE worker_session_id=?1 AND client_id=?2 AND state IN ('sending','unknown')
               AND shared_restart_managed=1 AND restart_released_at_ms IS NULL AND kept_cleanup_json IS NULL",
        )
        .bind(worker)
        .bind(&receipt.client_id)
        .bind(crate::model::now_ms())
        .execute(&mut **tx)
        .await?;
        if released.rows_affected() != 1 {
            return Ok(());
        }
    } else if header.phase != HarnessPhaseTag::IssuingTurn
        || acp_submission_unresolved_tx(tx, worker).await?
    {
        // ACP always persists a receipt before a prompt can cross.
        return Ok(());
    }
    carrier.checkpoint.active = None;
    carrier.set_phase(HarnessPhaseTag::Idle)?;
    carrier.snapshot["interruption_intent"] = serde_json::Value::Null;
    carrier.snapshot["wedged_reason"] = serde_json::Value::Null;
    save_input_carrier_tx(tx, &mut carrier).await?;
    if restore {
        restore_interrupt_timeout_carrier_tx(tx, &carrier).await?;
    } else {
        super::super::session_set_harness_observation_runtime_tx(
            tx,
            &worker.to_owned(),
            calm_types::worker::WorkerSessionState::Idle,
            Some(thread),
            None,
        )
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    }
    Ok(())
}

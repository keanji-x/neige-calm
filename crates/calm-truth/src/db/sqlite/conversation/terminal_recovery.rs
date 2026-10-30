//! Known terminal failure eligibility grants no native execution authority.
use super::*;
use calm_types::{
    conversation::{
        AbandonConversationRequest, InputDeliveryFact, NativeInputAcceptance, PendingInput,
    },
    harness::{HARNESS_SYSTEM_ERROR_REASON, HarnessPhaseTag, snapshot::CanonicalHarnessSnapshot},
};

pub async fn terminal_recovery_request_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &CurrentInputCarrier,
) -> Result<Option<AbandonConversationRequest>> {
    let frame: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
    if carrier.is_execution_current()
        || frame.header.phase != HarnessPhaseTag::TurnCompleted
        || frame.header.wedged_reason.as_deref() != Some(HARNESS_SYSTEM_ERROR_REASON)
        || frame.canonical_inputs.active.is_some()
        || frame.header.pending_rewind.is_some()
        || frame.header.interruption_intent.is_some()
        || frame
            .canonical_inputs
            .pending
            .iter()
            .any(|p| !matches!(p, PendingInput::Human { .. }))
    {
        return Ok(None);
    }
    let (Some(thread), Some(turn)) = (
        frame.header.last_thread_id.as_deref(),
        frame.header.last_turn_id.as_deref(),
    ) else {
        return Ok(None);
    };
    let attempts = frame.canonical_inputs.attempts.iter().filter(|fact| {
        fact.correlation.provider.protocol() == calm_types::runtime::AgentProtocol::Codex
            && fact.correlation.thread_id == thread
            && fact.correlation.native_turn_id.as_deref() == Some(turn)
            && matches!(&fact.delivery, InputDeliveryFact::Accepted { evidence: NativeInputAcceptance::NativeTurn { native_turn_id }, .. } if native_turn_id == turn)
    });
    let mut original = None;
    for fact in attempts {
        let initial: bool = sqlx::query_scalar(concat!("SELECT EXISTS(SELECT 1 FROM conversation_turns t JOIN conversation_input_submissions s ","ON s.id=t.initial_submission_id AND s.logical_turn_id=t.id AND s.card_id=t.card_id ","WHERE s.id=?1 AND s.kind='initial' AND s.ordinal=0 AND t.card_id=?2 AND t.initiating_runtime_id=?3)"))
            .bind(fact.attempt.submission_id.as_str()).bind(carrier.card_id()).bind(carrier.worker_session_id()).fetch_one(&mut **tx).await?;
        if initial {
            if original.is_some() {
                return Ok(None);
            }
            original = Some(fact);
        }
    }
    let Some(fact) = original else {
        return Ok(None);
    };
    // Released failed ENDs could leave this mirror at the exact ended turn.
    // Canonical closure plus the native owner's new proof remains authoritative;
    // a different mirrored turn never qualifies.
    let valid: bool = sqlx::query_scalar(concat!(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions ws JOIN tracks t ON t.id=ws.track_id ",
        "JOIN harness_items i ON i.worker_session_id=ws.id AND i.card_id=ws.card_id ",
        "WHERE ws.id=?1 AND ws.provider='codex' AND ws.state='failed' ",
        "AND ws.completed_at_ms IS NULL AND ws.queue_harvested_at_ms IS NULL ",
        "AND ws.terminal_run_id IS NULL AND (ws.active_turn_id IS NULL OR ws.active_turn_id=?3) AND ws.thread_id=?2 ",
        "AND t.closed_at IS NULL AND i.thread_id=?2 AND i.turn_id=?3 ",
        "AND i.method='turn/completed' AND json_extract(i.params,'$.id')=?3 ",
        "AND json_extract(i.params,'$.status')='failed')"
    ))
    .bind(carrier.worker_session_id())
    .bind(thread)
    .bind(turn)
    .fetch_one(&mut **tx)
    .await?;
    Ok(valid.then(|| AbandonConversationRequest {
        carrier_id: carrier.worker_session_id().into(),
        submission_id: fact.attempt.submission_id.as_str().into(),
        attempt_id: fact.attempt.attempt_id.as_str().into(),
        revision: fact.revision.to_string(),
    }))
}

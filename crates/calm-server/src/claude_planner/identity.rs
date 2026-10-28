//! Claude owns native identity recovery. A validated exact replay already proves that
//! this thread's session existed, even if its earlier init bind never committed.
use super::session::ClaudePlannerSessionParams;
use crate::db::sqlite::conversation::{current_input_fact_carrier_tx, submission_identity_tx};
use crate::error::{CalmError, Result};
use crate::harness::backend::{ExecutionEndScope, InputAcknowledgementBasis};
use crate::session_projection_repo::{AgentProvider, ThreadAttribution};
use calm_types::conversation::{
    ConversationExecutionProvenance, InputDeliveryFact, NativeInputAcceptance,
};
use calm_types::worker::WorkerSession;
use sqlx::{Sqlite, Transaction};

/// Recovery repairs identity only. It grants no execution authority and leaves
/// every input fact (including a subsequent Unknown attempt) untouched.
pub(super) async fn recover(params: &ClaudePlannerSessionParams) -> Result<bool> {
    let card = params.card_id.clone();
    let worker = params.worker_session_id.clone();
    let track = params.track_id.clone();
    crate::db::write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            let (binding, row) =
                crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
            if binding.provider != AgentProvider::Claude || row.track_id.as_str() != track {
                return Err(CalmError::Conflict(
                    "Claude identity recovery crosses its carrier".into(),
                ));
            }
            let Some(thread) = row.thread_id.as_deref() else {
                return Ok(false);
            };
            if carrier
                .snapshot()
                .get("last_thread_id")
                .and_then(serde_json::Value::as_str)
                != Some(thread)
            {
                return Err(CalmError::Conflict(
                    "Claude identity recovery crosses its thread".into(),
                ));
            }
            if let Some(bound) = row.agent_session_id.as_deref() {
                return if bound == thread {
                    Ok(true)
                } else {
                    Err(CalmError::Conflict(
                        "Claude identity binding changed during recovery".into(),
                    ))
                };
            }
            for checkpoint in &carrier.checkpoint().attempts {
                let InputDeliveryFact::Accepted {
                    basis,
                    evidence: NativeInputAcceptance::Submission { submission_id },
                } = &checkpoint.delivery
                else {
                    continue;
                };
                if basis != InputAcknowledgementBasis::ClaudeUserReplay.as_str()
                    || submission_id != &checkpoint.attempt.submission_id
                    || checkpoint.correlation.provider != AgentProvider::Claude
                    || checkpoint.correlation.thread_id != thread
                    || !matches!(
                        ExecutionEndScope::from_correlation(&checkpoint.correlation),
                        Some(ExecutionEndScope::LocalProcess { .. })
                    )
                {
                    continue;
                }
                let identity = submission_identity_tx(tx, submission_id).await?;
                let ConversationExecutionProvenance::LegacySingleUserDeployment {
                    provider,
                    profile,
                    ..
                } = identity.execution;
                // Replay acknowledgements are recorded only for the original input.
                // Verify ownership against its immutable provenance before binding.
                if identity.card_id != card
                    || identity.ordinal != 0
                    || provider != AgentProvider::Claude
                    || profile != binding.profile
                {
                    return Err(CalmError::Conflict(
                        "Claude replay evidence crosses its original input".into(),
                    ));
                }
                bind_tx(tx, &row, thread).await?;
                return Ok(true);
            }
            Ok(false)
        })
    })
    .await
}

/// One attribution writer for live init and verified recovery. Preserve the
/// harness-owned active turn, row state, checkpoint and provider execution claim.
pub(super) async fn bind_tx(
    tx: &mut Transaction<'_, Sqlite>,
    row: &WorkerSession,
    thread: &str,
) -> Result<()> {
    crate::db::sqlite::session_bind_attribution_tx(
        tx,
        &row.id.0,
        ThreadAttribution {
            worker_session_id: row.id.0.clone(),
            provider: AgentProvider::Claude,
            thread_id: Some(thread.into()),
            session_id: Some(thread.into()),
            active_turn_id: row.active_turn_id.clone(),
        },
    )
    .await?;
    Ok(())
}

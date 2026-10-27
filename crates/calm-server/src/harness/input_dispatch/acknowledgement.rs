//! Record native-owner evidence through the existing input checkpoint transaction.
//! This grants no execution authority and never derives acceptance from a transcript.
use super::*;
use crate::harness::backend::{ExecutionEndScope, ExecutionEndTarget};
use sqlx::{Sqlite, Transaction};

pub(crate) async fn acknowledge(
    repo: &dyn Repo,
    target: ExecutionEndTarget,
    provider: AgentProvider,
    basis: &'static str,
) -> Result<()> {
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { acknowledge_tx(tx, &target, &provider, basis).await })
    })
    .await
}

pub(super) async fn acknowledge_tx(
    tx: &mut Transaction<'_, Sqlite>,
    target: &ExecutionEndTarget,
    provider: &AgentProvider,
    basis: &'static str,
) -> Result<()> {
    let mut carrier =
        current_input_fact_carrier_tx(tx, &target.card_id, &target.worker_session_id).await?;
    let (binding, session) =
        crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
    let (thread, turn) = target.scope.thread_and_turn();
    let original =
        crate::db::sqlite::conversation::original_initial_submission_tx(tx, &carrier).await?;
    let attempt = target.proof.attempt();
    if &binding.provider != provider
        || session.track_id.as_str() != target.track_id
        || session.thread_id.as_deref() != Some(thread.as_str())
        || original.as_ref() != Some(&attempt.submission_id)
        || carrier
            .checkpoint()
            .active
            .as_ref()
            .is_none_or(|active| active.logical_turn_id != target.logical_turn_id)
    {
        return Err(CalmError::Conflict(
            "acknowledgement names another original input".into(),
        ));
    }
    let identity = submission_identity_tx(tx, &attempt.submission_id).await?;
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        provider: recorded_provider,
        profile,
        ..
    } = &identity.execution;
    if recorded_provider != provider || *profile != binding.profile {
        return Err(CalmError::Conflict(
            "acknowledgement crosses its execution provenance".into(),
        ));
    }
    let proof = crate::db::sqlite::conversation::submission_dispatch_proof_tx(tx, attempt).await?;
    if proof != target.proof {
        return Err(CalmError::Conflict(
            "acknowledgement differs from its frozen input proof".into(),
        ));
    }
    let prior = carrier
        .checkpoint()
        .attempt(&attempt.submission_id)
        .ok_or_else(|| CalmError::Conflict("acknowledgement has no input attempt".into()))?;
    let mut correlation = prior.correlation.clone();
    if &correlation.provider != provider || correlation.thread_id != thread {
        return Err(CalmError::Conflict(
            "acknowledgement crosses its provider thread".into(),
        ));
    }
    // The target comes only from its native owner. Existing identities
    // must still agree; the checkpoint writer enforces monotone refinement.
    match &target.scope {
        ExecutionEndScope::LocalProcess { .. } => {
            correlation.local_turn_id = Some(turn);
        }
        ExecutionEndScope::ReceiptProcess {
            native_session_id, ..
        } => {
            correlation.local_turn_id = Some(turn);
            correlation.native_session_id = Some(native_session_id.clone());
        }
        ExecutionEndScope::NativeTurn { .. } => {
            correlation.native_turn_id = Some(turn);
        }
    }
    record_input_attempt_fact_tx(
        tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation,
            delivery: InputDeliveryFact::Accepted {
                basis: basis.into(),
                evidence: NativeInputAcceptance::Submission {
                    submission_id: attempt.submission_id.clone(),
                },
            },
        },
    )
    .await?;
    Ok(())
}

//! Consume one actual immutable END as a pure canonical transition. This owner
//! never starts/wakes a backend or grants the next input. Native acknowledgement
//! evidence, when present, is committed atomically with the original END.
use calm_types::conversation::ConversationExecutionProvenance;
use calm_types::runtime::AgentProvider;
use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::conversation::{
    OriginalInputExecution, RuntimeExecutionFence, RuntimeFrameAfterWrite,
    complete_input_execution_tx, current_input_fact_carrier_tx, original_input_execution_tx,
};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::Result;
use crate::harness::backend::{
    ExecutionEndScope, ExecutionEndTarget, PlannerBackend, QualifiedExecutionEnd,
};
use crate::harness::profile::input_carrier_contract_tx;

#[cfg(all(test, feature = "fixtures"))]
mod tests;

struct Sample {
    target: ExecutionEndTarget,
    fence: RuntimeExecutionFence,
    original: OriginalInputExecution,
}

async fn sample_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    runtime: &str,
    provider: &AgentProvider,
    settled: bool,
) -> Result<Option<Sample>> {
    let carrier = current_input_fact_carrier_tx(tx, card, runtime).await?;
    let original = if settled {
        original_input_execution_tx(tx, &carrier).await?
    } else {
        crate::db::sqlite::conversation::original_input_target_tx(tx, &carrier).await?
    };
    let Some(original) = original else {
        return Ok(None);
    };
    let (binding, current) = input_carrier_contract_tx(tx, &carrier).await?;
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        provider: recorded_provider,
        profile,
        ..
    } = &original.identity.execution;
    if binding.provider != *provider
        || recorded_provider != provider
        || binding.profile != *profile
        || original.correlation.provider != *provider
        || current.thread_id.as_deref() != Some(original.correlation.thread_id.as_str())
    {
        return Ok(None);
    }
    let Some(scope) = ExecutionEndScope::from_correlation(&original.correlation) else {
        return Ok(None);
    };
    Ok(Some(Sample {
        target: ExecutionEndTarget {
            card_id: card.into(),
            track_id: current.track_id.to_string(),
            worker_session_id: runtime.into(),
            logical_turn_id: original.identity.logical_turn_id.clone(),
            proof: original.proof.clone(),
            scope,
        },
        fence: RuntimeExecutionFence::from_carrier(&carrier),
        original,
    }))
}

/// The same original Initial/proof association used for END qualification.
/// A returned value is correlation, never permission to send a control.
pub(crate) async fn execution_target_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    runtime: &str,
    provider: &AgentProvider,
) -> Result<Option<ExecutionEndTarget>> {
    Ok(sample_tx(tx, card, runtime, provider, true)
        .await?
        .map(|sample| sample.target))
}

async fn consume_tx(
    tx: &mut Transaction<'_, Sqlite>,
    provider: &AgentProvider,
    sampled: &Sample,
    observed: &QualifiedExecutionEnd,
) -> Result<Option<RuntimeFrameAfterWrite>> {
    let card = &sampled.target.card_id;
    let runtime = &sampled.target.worker_session_id;
    let Some(current) = sample_tx(tx, card, runtime, provider, true).await? else {
        return Ok(None);
    };
    if current.fence != sampled.fence
        || !observed.matches(&current.target)
        || current.original != sampled.original
    {
        return Ok(None);
    }
    if let Some(basis) = observed.acknowledgement() {
        super::acknowledgement::acknowledge_tx(tx, &current.target, provider, basis.as_str())
            .await?;
        // The factual write changes the checkpoint fence. Re-read our own write
        // under this same transaction; never relax the pre-observation CAS.
        let acknowledged = sample_tx(tx, card, runtime, provider, true)
            .await?
            .ok_or_else(|| {
                crate::error::CalmError::Conflict(
                    "acknowledged original execution disappeared".into(),
                )
            })?;
        return Ok(complete_input_execution_tx(
            tx,
            card,
            runtime,
            &acknowledged.fence,
            &acknowledged.original,
        )
        .await?);
    }
    Ok(complete_input_execution_tx(tx, card, runtime, &sampled.fence, &sampled.original).await?)
}

/// The V2 FSM consumes the returned frame only after this future reports its
/// committed result. The V1 loop/serializer is intentionally not a caller.
pub(crate) async fn complete_once(
    repo: &dyn Repo,
    backend: &PlannerBackend,
    card_id: &str,
    worker_session_id: &str,
) -> Result<Option<RuntimeFrameAfterWrite>> {
    let card = card_id.to_owned();
    let runtime = worker_session_id.to_owned();
    let provider = backend.provider();
    // A single-statement observation can rule out an empty execution without
    // writer admission. It grants nothing: real candidates are sampled again
    // in the original protected transaction, then revalidated by consume_tx.
    let db = repo.sqlite_db().ok_or_else(|| {
        crate::error::CalmError::Internal("canonical END observation requires SQLite".into())
    })?;
    if crate::db::sqlite::conversation::runtime_frame_observation(db.read_pool(), &card, &runtime)
        .await?
        .snapshot
        .canonical_inputs
        .active
        .is_none()
    {
        return Ok(None);
    }
    let sampled = write_in_tx_typed(repo, {
        let card = card.clone();
        let runtime = runtime.clone();
        let provider = provider.clone();
        move |tx| Box::pin(async move { sample_tx(tx, &card, &runtime, &provider, true).await })
    })
    .await?;
    let Some(sampled) = sampled else {
        return Ok(None);
    };
    // Current-source validation and finished-driver/journal observation happen
    // with NO SQLite writer held. An obtained observation remains a past fact.
    let Some(observed) = backend.qualify_execution_end(&sampled.target).await? else {
        return Ok(None);
    };
    if let Some(terminal) = observed.terminal() {
        let mut terminal = terminal.clone();
        crate::harness::turn_outcome::canonical_diagnostic(&mut terminal);
        let (thread_id, turn_id) = sampled.target.scope.thread_and_turn();
        crate::harness::turn_outcome::record(
            repo,
            &sampled.target.worker_session_id,
            &sampled.target.card_id,
            &sampled.target.track_id,
            &thread_id,
            &turn_id,
            &terminal,
            crate::harness::turn_outcome::Confirmation::ProviderTerminal,
        )
        .await?;
    }
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { consume_tx(tx, &provider, &sampled, &observed).await })
    })
    .await
}

/// Accepted original execution, not authority to dispatch or control another turn.
pub(crate) struct AcceptedRecovery {
    sample: Sample,
    backend: String,
}
impl AcceptedRecovery {
    pub(crate) fn target(&self) -> &ExecutionEndTarget {
        &self.sample.target
    }
    pub(crate) fn backend_id(&self) -> &str {
        &self.backend
    }
    pub(crate) async fn validate_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        let scope = crate::db::sqlite::session_backend::codex_backend_scope_tx(
            tx,
            crate::db::sqlite::session_backend::CodexScopeKey::Session(
                &self.sample.target.worker_session_id,
            ),
        )
        .await?;
        if scope != crate::db::sqlite::session_backend::CodexScope::Bound(self.backend.clone()) {
            return Err(crate::error::CalmError::Conflict(
                "original native backend scope changed".into(),
            ));
        }
        let current = sample_tx(
            tx,
            &self.sample.target.card_id,
            &self.sample.target.worker_session_id,
            &AgentProvider::Codex,
            true,
        )
        .await?;
        if current.as_ref().is_none_or(|current| {
            current.target != self.sample.target
                || current.fence != self.sample.fence
                || current.original != self.sample.original
        }) {
            return Err(crate::error::CalmError::Conflict(
                "original accepted recovery execution changed".into(),
            ));
        }
        Ok(())
    }
}
pub(crate) async fn accepted_recovery_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    worker: &str,
    owner: &str,
) -> Result<Option<AcceptedRecovery>> {
    use calm_types::conversation::{InputDeliveryFact, NativeInputAcceptance};
    let Some(sample) = sample_tx(tx, card, worker, &AgentProvider::Codex, true).await? else {
        return Ok(None);
    };
    let carrier = current_input_fact_carrier_tx(tx, card, worker).await?;
    let Some(fact) = carrier
        .checkpoint()
        .attempt(&sample.original.proof.attempt().submission_id)
    else {
        return Ok(None);
    };
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id,
        ..
    } = &sample.original.identity.execution;
    let accepted = matches!((&sample.target.scope,&fact.delivery),
        (ExecutionEndScope::NativeTurn{native_turn_id,..},InputDeliveryFact::Accepted{evidence:NativeInputAcceptance::NativeTurn{native_turn_id:accepted},..}) if native_turn_id==accepted);
    let scope = crate::db::sqlite::session_backend::codex_backend_scope_tx(
        tx,
        crate::db::sqlite::session_backend::CodexScopeKey::Session(worker),
    )
    .await?;
    let crate::db::sqlite::session_backend::CodexScope::Bound(backend) = scope else {
        return Ok(None);
    };
    Ok((accepted && deployment_owner_user_id == owner)
        .then_some(AcceptedRecovery { sample, backend }))
}

/// Current original target independent of supplemental report settlement. No END authority.
pub(crate) async fn retained_execution_target_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    runtime: &str,
    provider: &AgentProvider,
) -> Result<Option<ExecutionEndTarget>> {
    Ok(sample_tx(tx, card, runtime, provider, false)
        .await?
        .map(|sample| sample.target))
}

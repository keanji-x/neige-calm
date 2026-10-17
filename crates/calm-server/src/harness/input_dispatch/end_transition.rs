//! Consume one actual immutable END as a pure canonical transition. This owner
//! never starts/wakes a backend, alters delivery or grants the next input.
use calm_types::conversation::ConversationExecutionProvenance;
use calm_types::runtime::AgentProvider;
use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::conversation::{
    OriginalInputExecution, RuntimeExecutionFence, RuntimeFrameAfterWrite,
    complete_input_execution_tx, current_input_carrier_tx, original_input_execution_tx,
};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::Result;
use crate::harness::backend::{
    ExecutionEndScope, ExecutionEndTarget, PlannerBackend, QualifiedExecutionEnd,
};
use crate::harness::profile::input_carrier_binding_tx;

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
) -> Result<Option<Sample>> {
    let carrier = current_input_carrier_tx(tx, card, runtime).await?;
    let Some(original) = original_input_execution_tx(tx, &carrier).await? else {
        return Ok(None);
    };
    let (binding, current) = input_carrier_binding_tx(tx, &carrier).await?;
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
    let correlation = &original.correlation;
    let scope = match (
        &correlation.native_turn_id,
        &correlation.native_session_id,
        &correlation.local_turn_id,
    ) {
        (Some(turn), _, None) => ExecutionEndScope::NativeTurn {
            thread_id: correlation.thread_id.clone(),
            native_turn_id: turn.clone(),
        },
        (None, Some(session), Some(turn))
            if current.agent_session_id.as_deref() == Some(session.as_str()) =>
        {
            ExecutionEndScope::LocalExecution {
                thread_id: correlation.thread_id.clone(),
                native_session_id: session.clone(),
                local_turn_id: turn.clone(),
            }
        }
        _ => return Ok(None),
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

async fn consume_tx(
    tx: &mut Transaction<'_, Sqlite>,
    provider: &AgentProvider,
    sampled: &Sample,
    observed: &QualifiedExecutionEnd,
) -> Result<Option<RuntimeFrameAfterWrite>> {
    let card = &sampled.target.card_id;
    let runtime = &sampled.target.worker_session_id;
    let Some(current) = sample_tx(tx, card, runtime, provider).await? else {
        return Ok(None);
    };
    if current.fence != sampled.fence
        || !observed.matches(&current.target)
        || current.original != sampled.original
    {
        return Ok(None);
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
    let sampled = write_in_tx_typed(repo, {
        let card = card.clone();
        let runtime = runtime.clone();
        let provider = provider.clone();
        move |tx| Box::pin(async move { sample_tx(tx, &card, &runtime, &provider).await })
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
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { consume_tx(tx, &provider, &sampled, &observed).await })
    })
    .await
}

//! The runtime owns its whole header; conversation input owns the queue and
//! claims. A stale cache may preserve newer accepted input, but must never roll
//! back a same-phase runtime fact such as a budget reminder or native rewind.
use super::carrier::save_input_carrier_tx;
use super::{CurrentInputCarrier, current_input_carrier_tx};
use crate::error::{CalmError, Result};
use calm_types::conversation::{
    ActiveInputSubmissions, CONVERSATION_INPUT_CHECKPOINT_KEY, InputAttemptCheckpoint,
};
use calm_types::harness::HarnessPhaseTag;
use serde_json::Value;
use sqlx::{Sqlite, Transaction};

/// An in-memory comparison value, not a grant, retry driver, or second ledger.
/// Comparing the whole existing header automatically covers new runtime fields.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeExecutionFence {
    header: Value,
    active: Option<ActiveInputSubmissions>,
    attempts: Vec<InputAttemptCheckpoint>,
}

impl RuntimeExecutionFence {
    pub fn from_carrier(carrier: &CurrentInputCarrier) -> Self {
        let mut header = carrier.snapshot().clone();
        header
            .as_object_mut()
            .expect("a validated V2 carrier is an object")
            .remove(CONVERSATION_INPUT_CHECKPOINT_KEY);
        Self {
            header,
            active: carrier.checkpoint().active.clone(),
            attempts: carrier.checkpoint().attempts.clone(),
        }
    }

    /// The last committed runtime header, without any pending/input authority.
    /// The FSM owner builds its proposed next frame from its declared state.
    pub fn header(&self) -> &Value {
        &self.header
    }
}

/// Publish this snapshot/fence to caches only after the surrounding transaction
/// commits. The snapshot contains the current queue, including newer acceptance.
pub struct RuntimeFrameAfterWrite {
    pub snapshot: Value,
    pub execution_fence: RuntimeExecutionFence,
}

pub async fn persist_runtime_frame_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
    expected: &RuntimeExecutionFence,
    mut next_header: Value,
) -> Result<RuntimeFrameAfterWrite> {
    let header = next_header
        .as_object_mut()
        .ok_or_else(|| CalmError::Conflict("runtime header must be an object"))?;
    if header.contains_key(CONVERSATION_INPUT_CHECKPOINT_KEY) {
        return Err(CalmError::Conflict(
            "runtime header cannot replace canonical input authority",
        ));
    }
    let mut current = current_input_carrier_tx(tx, card_id, worker_session_id).await?;
    if RuntimeExecutionFence::from_carrier(&current) != *expected {
        return Err(CalmError::Conflict(
            "runtime frame or input execution changed since its last committed observation",
        ));
    }
    header.insert(
        CONVERSATION_INPUT_CHECKPOINT_KEY.into(),
        serde_json::to_value(current.checkpoint())?,
    );
    current.snapshot = next_header;
    current.phase()?;
    save_input_carrier_tx(tx, &mut current).await?;
    Ok(RuntimeFrameAfterWrite {
        snapshot: current.snapshot().clone(),
        execution_fence: RuntimeExecutionFence::from_carrier(&current),
    })
}

/// The kernel has observed an exact native END outside this transaction. This
/// dedicated fact mutation closes only its unchanged logical execution; it
/// never changes delivery/claims or grants a new native/Workspace operation.
pub async fn complete_input_execution_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
    expected: &RuntimeExecutionFence,
    original: &super::plans::OriginalInputExecution,
) -> Result<Option<RuntimeFrameAfterWrite>> {
    let mut current = current_input_carrier_tx(tx, card_id, worker_session_id).await?;
    if RuntimeExecutionFence::from_carrier(&current) != *expected
        || super::plans::original_input_execution_tx(tx, &current)
            .await?
            .as_ref()
            != Some(original)
    {
        return Ok(None);
    }
    let Some(turn) = original.turn_id() else {
        return Ok(None);
    };
    let phase = current.phase()?;
    if !matches!(
        phase,
        HarnessPhaseTag::IssuingTurn | HarnessPhaseTag::TurnRunning
    ) || (phase == HarnessPhaseTag::TurnRunning
        && current
            .snapshot()
            .get("last_turn_id")
            .and_then(Value::as_str)
            != Some(turn))
        || current
            .snapshot()
            .get("last_thread_id")
            .and_then(Value::as_str)
            != Some(original.correlation.thread_id.as_str())
        || ["pending_rewind", "interruption_intent", "wedged_reason"]
            .iter()
            .any(|key| {
                current
                    .snapshot()
                    .get(*key)
                    .is_some_and(|value| !value.is_null())
            })
    {
        return Ok(None);
    }
    current.checkpoint.active = None;
    current.set_phase(HarnessPhaseTag::TurnCompleted)?;
    current.snapshot["last_turn_id"] = Value::String(turn.to_owned());
    save_input_carrier_tx(tx, &mut current).await?;
    super::super::session_set_harness_observation_runtime_tx(
        tx,
        &worker_session_id.to_owned(),
        calm_types::worker::WorkerSessionState::Idle,
        Some(original.correlation.thread_id.as_str()),
        None,
    )
    .await
    .map_err(|error| CalmError::Internal(error.to_string()))?;
    Ok(Some(RuntimeFrameAfterWrite {
        snapshot: current.snapshot().clone(),
        execution_fence: RuntimeExecutionFence::from_carrier(&current),
    }))
}

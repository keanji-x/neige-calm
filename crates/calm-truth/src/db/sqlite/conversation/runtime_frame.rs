//! The runtime owns its whole header; conversation input owns the queue and
//! claims. A stale cache may preserve newer accepted input, but must never roll
//! back a same-phase runtime fact such as a budget reminder or native rewind.
use super::carrier::{restore_interrupt_timeout_carrier_tx, save_input_carrier_tx};
use super::{CurrentInputCarrier, current_input_fact_carrier_tx};
use crate::error::{CalmError, Result};
use calm_types::conversation::{
    ActiveInputSubmissions, CONVERSATION_INPUT_CHECKPOINT_KEY, HumanInputResolutionReceipt,
    InputAttemptCheckpoint,
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
    receipts: Vec<HumanInputResolutionReceipt>,
}

impl RuntimeExecutionFence {
    pub fn from_carrier(carrier: &CurrentInputCarrier) -> Self {
        let mut header = carrier.snapshot().clone();
        header
            .as_object_mut()
            .expect("a validated V2 carrier is an object")
            .remove(CONVERSATION_INPUT_CHECKPOINT_KEY);
        header
            .as_object_mut()
            .expect("validated header")
            .remove("push_watermark");
        Self {
            header,
            active: carrier.checkpoint().active.clone(),
            attempts: carrier.checkpoint().attempts.clone(),
            receipts: carrier.checkpoint().receipts.clone(),
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
    let mut current = current_input_fact_carrier_tx(tx, card_id, worker_session_id).await?;
    if RuntimeExecutionFence::from_carrier(&current) != *expected {
        return Err(CalmError::Conflict(
            "runtime frame or input execution changed since its last committed observation",
        ));
    }
    // Event acceptance owns this monotonic metadata; a runtime cache cannot
    // advance it without accepting, or overwrite a newer accepted prefix.
    header.insert(
        "push_watermark".into(),
        current.snapshot()["push_watermark"].clone(),
    );
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

/// Existing control ownership check shared by END and factual Keep settlement.
pub(super) async fn interruption_matches_tx(
    tx: &mut Transaction<'_, Sqlite>,
    current: &super::CurrentInputCarrier,
    thread_id: &str,
    turn_id: &str,
) -> Result<bool> {
    let card_id = current.card_id();
    let worker_session_id = current.worker_session_id();
    if let Some(intent) = current
        .snapshot()
        .get("interruption_intent")
        .filter(|value| !value.is_null())
    {
        use calm_types::harness::{
            HarnessControlIntent, HarnessInterruptionIntent, HarnessTimeoutIntent,
        };
        let intent: HarnessInterruptionIntent = serde_json::from_value(intent.clone())?;
        let track: String = sqlx::query_scalar("SELECT track_id FROM worker_sessions WHERE id=?1")
            .bind(worker_session_id)
            .fetch_one(&mut **tx)
            .await?;
        let matches = match &intent {
            HarnessInterruptionIntent::Versioned(HarnessTimeoutIntent::V1(request)) => {
                request.session_id == worker_session_id
                    && request.card_id == card_id
                    && request.track_id == track
                    && request.thread_id == thread_id
                    && request.turn_id == turn_id
            }
            HarnessInterruptionIntent::Control(HarnessControlIntent::UserStopV1(request)) => {
                request.session_id == worker_session_id
                    && request.card_id == card_id
                    && request.track_id == track
                    && request.thread_id == thread_id
                    && request.turn_id == turn_id
            }
            HarnessInterruptionIntent::Legacy(intent) => intent.turn_id == turn_id,
        };
        return Ok(matches);
    }
    Ok(true)
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
    let mut current = current_input_fact_carrier_tx(tx, card_id, worker_session_id).await?;
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
    if !interruption_matches_tx(tx, &current, &original.correlation.thread_id, turn).await? {
        return Ok(None);
    }
    let phase = current.phase()?;
    let has_intent = current
        .snapshot()
        .get("interruption_intent")
        .is_some_and(|value| !value.is_null());
    let wedge = current
        .snapshot()
        .get("wedged_reason")
        .and_then(Value::as_str);
    let ordinary = matches!(
        phase,
        HarnessPhaseTag::IssuingTurn | HarnessPhaseTag::TurnRunning
    ) && !has_intent
        && wedge.is_none();
    let stopping = phase == HarnessPhaseTag::IssuingInterrupt && has_intent && wedge.is_none();
    let failed_end = phase == HarnessPhaseTag::Wedged
        && !current.is_execution_current()
        && matches!(
            wedge,
            Some(
                calm_types::harness::HARNESS_SYSTEM_ERROR_REASON
                    | calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON
                    | calm_types::harness::HARNESS_RUNTIME_FAILED_REASON
            )
        );
    if !(ordinary || stopping || failed_end)
        || (phase != HarnessPhaseTag::IssuingTurn
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
        || ["pending_rewind"].iter().any(|key| {
            current
                .snapshot()
                .get(*key)
                .is_some_and(|value| !value.is_null())
        })
    {
        return Ok(None);
    }
    // The original's own END, matched above against the retained Stop intent,
    // resolves what interrupt_timeout stood for: whether it was still live.
    // system_error and runtime_failed are not resolved by an END.
    let restore = failed_end
        && has_intent
        && wedge == Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON);
    current.checkpoint.active = None;
    current.set_phase(HarnessPhaseTag::TurnCompleted)?;
    current.snapshot["last_turn_id"] = Value::String(turn.to_owned());
    current.snapshot["interruption_intent"] = Value::Null;
    let execution_current = current.is_execution_current();
    if execution_current || restore {
        current.snapshot["wedged_reason"] = Value::Null;
    }
    save_input_carrier_tx(tx, &mut current).await?;
    if restore {
        restore_interrupt_timeout_carrier_tx(tx, &current).await?;
    } else {
        super::super::session_set_harness_observation_runtime_tx(
            tx,
            &worker_session_id.to_owned(),
            if execution_current {
                calm_types::worker::WorkerSessionState::Idle
            } else {
                calm_types::worker::WorkerSessionState::Failed
            },
            Some(original.correlation.thread_id.as_str()),
            None,
        )
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    }
    Ok(Some(RuntimeFrameAfterWrite {
        snapshot: current.snapshot().clone(),
        execution_fence: RuntimeExecutionFence::from_carrier(&current),
    }))
}

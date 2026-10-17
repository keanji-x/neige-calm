//! Canonical inputs are read and claimed from the current carrier. This runtime
//! stores only transient exact-input sources and a committed read projection.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::{
    RuntimeExecutionFence, current_input_carrier_tx,
};
use crate::harness::snapshot::CanonicalHarnessSnapshot;
use crate::harness::profile::input_carrier_binding_tx;
use calm_types::conversation::MessageRevisionRef;
use std::collections::HashMap;

#[derive(Clone)]
pub(super) struct CommittedFrame {
    pub snapshot: CanonicalHarnessSnapshot,
    pub fence: RuntimeExecutionFence,
}

pub(super) struct CanonicalInputs {
    // This is an identity index, not an ordered queue or an admission cache.
    // Every claim/crossing still revalidates its actual issuer and current scope.
    sources: Mutex<HashMap<MessageRevisionRef, HumanInputSource>>,
    projection: Mutex<CommittedFrame>,
}

impl CanonicalInputs {
    pub fn new(frame: CommittedFrame) -> Self {
        Self {
            sources: Mutex::new(HashMap::new()),
            projection: Mutex::new(frame),
        }
    }

    pub async fn observe_source(
        &self,
        input: MessageRevisionRef,
        source: HumanInputSource,
    ) {
        self.sources.lock().await.insert(input, source);
    }

    pub async fn source(&self, input: &MessageRevisionRef) -> Option<HumanInputSource> {
        self.sources.lock().await.get(input).cloned()
    }

    pub async fn committed(&self) -> CommittedFrame {
        self.projection.lock().await.clone()
    }

    pub async fn publish_committed(&self, frame: CommittedFrame) {
        *self.projection.lock().await = frame;
    }
}

/// The supplied snapshot selected a contract, not a reusable execution fence.
/// Read the actual current carrier before the loop consumes any commands or
/// native events. No authorization, native readiness or source is fabricated.
pub(super) async fn load_current(params: &PlannerHarnessParams) -> Result<CommittedFrame> {
    let card = params.card_id.to_string();
    let track = params.track_id.to_string();
    let runtime = params.worker_session_id.clone();
    let supplied_thread = params.thread_id.clone();
    let provider = params.backend.provider();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
            let snapshot: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone()).map_err(|_| {
                CalmError::Conflict("current canonical carrier has an invalid frame".into())
            })?;
            let (binding, current) = input_carrier_binding_tx(tx, &carrier).await?;
            if current.track_id.as_str() != track
                || binding.provider != provider
                || current.thread_id != snapshot.header.last_thread_id
                || supplied_thread.as_ref().is_some_and(|thread| {
                    current.thread_id.as_ref() != Some(thread)
                })
            {
                return Err(CalmError::Conflict(
                    "canonical runtime parameters differ from its current binding".into(),
                ));
            }
            if snapshot.header.phase == HarnessPhaseTag::TurnCompleted
                && snapshot.header.last_turn_id.as_deref().is_none_or(str::is_empty)
            {
                return Err(CalmError::Conflict(
                    "completed canonical frame has no exact original turn".into(),
                ));
            }
            Ok(CommittedFrame {
                snapshot,
                fence: RuntimeExecutionFence::from_carrier(&carrier),
            })
        })
    })
    .await
}

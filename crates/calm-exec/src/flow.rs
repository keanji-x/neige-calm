//! Passive worker→read-model normalization contracts.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::worker::{WorkerProviderKind, WorkerSession};
use calm_types::worker_flow::WorkerFlowItem;

/// Identifiers stamped onto each captured item.
pub struct FlowRowCtx {
    pub session_id: calm_types::worker::WorkerSessionId,
    pub wave_id: Option<String>,
    pub card_id: Option<String>,
}

/// Read-model writer for normalized flow items.
#[async_trait]
pub trait WorkerFlowItemSink: Send + Sync {
    /// A saturated sink returns `CoreError::ServiceUnavailable`, never a drop.
    async fn record(&self, ctx: &FlowRowCtx, item: WorkerFlowItem) -> Result<(), CoreError>;
}

/// A provider's passive drain of its own worker wire into a sink.
#[async_trait]
pub trait WorkerFlowSource: Send + Sync {
    fn provider(&self) -> WorkerProviderKind;

    /// Passive: drain the worker's wire, normalize, push to `sink` until the
    /// session ends. Opens no model connection, sends no turn, advances no
    /// FSM.
    async fn capture(
        &self,
        session: &WorkerSession,
        sink: &dyn WorkerFlowItemSink,
    ) -> Result<(), CoreError>;
}

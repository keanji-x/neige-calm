//! The worker-flow capture seam — passive worker→read-model normalization
//! (#695 PR1: traits only).
//!
//! Two traits mirror the [`crate::observation`] / [`crate::reaction`] seams,
//! but for the *capture* direction:
//!
//! * [`WorkerFlowItemSink`] — where normalized
//!   [`WorkerFlowItem`](calm_types::worker_flow::WorkerFlowItem)s go (the
//!   read-model writer). Backpressure is an error, not a drop —
//!   `CoreError::ServiceUnavailable`, matching [`crate::observation::ObservationSink`].
//! * [`WorkerFlowSource`] — a provider's *passive* drain of its own wire:
//!   read bytes/records, normalize, push to the sink until the session ends.
//!   A source opens no model connection, sends no turn, advances no FSM — it
//!   is read-only over an already-running worker. The real Codex/Claude
//!   sources land in a later PR; PR1 defines only the contract.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::worker::{WorkerProviderKind, WorkerSession};
use calm_types::worker_flow::WorkerFlowItem;

/// Row-context the sink stamps onto every captured item. Carries the
/// identifiers the read-model keys flow rows by; the [`WorkerFlowItem`]'s own
/// [`FlowEnvelope`](calm_types::worker_flow::FlowEnvelope) carries sequencing.
pub struct FlowRowCtx {
    /// `worker_sessions(id)` FK; after #695 PR5 this is the same value as
    /// the backing runtime id.
    pub session_id: calm_types::worker::WorkerSessionId,
    pub wave_id: Option<String>,
    pub card_id: Option<String>,
}

/// Read-model writer for normalized flow items.
#[async_trait]
pub trait WorkerFlowItemSink: Send + Sync {
    /// Record one normalized `item` under `ctx`.
    ///
    /// A saturated sink returns `CoreError::ServiceUnavailable` (backpressure,
    /// not a fault) so the caller can retry or park — mirror
    /// [`crate::observation::ObservationSink`].
    async fn record(&self, ctx: &FlowRowCtx, item: WorkerFlowItem) -> Result<(), CoreError>;
}

/// A provider's passive drain of its own worker wire into a sink.
#[async_trait]
pub trait WorkerFlowSource: Send + Sync {
    /// The provider this source captures for.
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

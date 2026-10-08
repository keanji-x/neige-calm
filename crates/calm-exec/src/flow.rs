//! Passive worker→read-model normalization contracts.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::worker::{WorkerProviderKind, WorkerSession};
use calm_types::worker_flow::WorkerFlowItem;

/// Identifiers stamped onto each captured item.
pub struct FlowRowCtx {
    pub session_id: calm_types::worker::WorkerSessionId,
    pub track_id: Option<String>,
    pub card_id: Option<String>,
}

/// Durable source position. Reset/rewrite may legitimately lower its index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturePosition {
    pub source_path: String,
    pub record_index: i64,
    pub byte_offset: i64,
    pub last_source_uuid: Option<String>,
    pub last_line_hash: Option<String>,
}

/// Complete durable compare value, including the stored activity timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureCheckpoint {
    Missing,
    Present {
        position: CapturePosition,
        updated_at_ms: i64,
    },
}

/// All items from one source record, including zero-item records.
pub struct CaptureBatch {
    pub card_id: String,
    pub source_kind: String,
    pub expected: CaptureCheckpoint,
    pub next: CapturePosition,
    pub items: Vec<WorkerFlowItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
    Applied(CaptureCheckpoint),
    /// Caller must stop and reconstruct from durable state with a new source.
    Stale,
}

/// Authoritative read-model writer for one record and its checkpoint.
#[async_trait]
pub trait WorkerFlowItemSink: Send + Sync {
    /// Only known-rolled-back contention returns `ServiceUnavailable`.
    /// Other failures must stop capture and recover from the durable checkpoint.
    async fn capture_batch(
        &self,
        ctx: &FlowRowCtx,
        batch: &CaptureBatch,
    ) -> Result<CaptureOutcome, CoreError>;
}

/// A provider's passive drain of its own worker wire into a sink.
#[async_trait]
pub trait WorkerFlowSource: Send + Sync {
    fn provider(&self) -> WorkerProviderKind;

    /// Passive: drain the worker's wire into `sink` until the session ends; opens no model connection, sends no turn, advances no FSM.
    async fn capture(
        &self,
        session: &WorkerSession,
        sink: &dyn WorkerFlowItemSink,
    ) -> Result<(), CoreError>;
}

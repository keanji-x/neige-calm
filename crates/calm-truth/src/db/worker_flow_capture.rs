//! Serialized internal capture contract; not a database or wire schema.
use calm_exec::flow::{CaptureCheckpoint, CapturePosition};
use calm_types::worker::WorkerSessionId;

pub struct CaptureItem {
    pub kind: String,
    pub payload: String,
}

pub struct WorkerFlowCapture {
    pub card_id: String,
    pub source_kind: String,
    pub session_id: WorkerSessionId,
    pub track_id: Option<String>,
    pub expected: CaptureCheckpoint,
    pub next: CapturePosition,
    pub items: Vec<CaptureItem>,
}

impl From<&super::rows::WorkerFlowCursor> for CaptureCheckpoint {
    fn from(row: &super::rows::WorkerFlowCursor) -> Self {
        Self::Present {
            position: CapturePosition {
                source_path: row.source_path.clone(),
                record_index: row.record_index,
                byte_offset: row.byte_offset,
                last_source_uuid: row.last_source_uuid.clone(),
                last_line_hash: row.last_line_hash.clone(),
            },
            updated_at_ms: row.updated_at_ms,
        }
    }
}

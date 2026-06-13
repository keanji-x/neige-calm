//! Typed DTOs for the read-only wave file JSON projections.

use crate::ids::CardId;
use crate::model::CardRole;
use serde::Serialize;
use serde_json::Value;
use std::fmt;
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsCardMeta {
    pub created_at: i64,
    pub deletable: bool,
    #[schema(value_type = String)]
    pub id: CardId,
    pub kind: String,
    pub role: CardRole,
    pub sort: f64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum WaveFsRunStatus {
    Completed,
    Failed,
    Running,
    Requested,
    Unknown,
}

impl WaveFsRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Running => "running",
            Self::Requested => "requested",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for WaveFsRunStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsRunVerdictSummary {
    pub at: i64,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsRunVerdict {
    pub at: i64,
    #[schema(nullable = true, required = true)]
    pub reason: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsRunIndexEntry {
    #[schema(nullable = true, required = true)]
    pub finished_at: Option<i64>,
    pub idempotency_key: String,
    pub kind: String,
    #[schema(nullable = true, required = true)]
    pub requested_at: Option<i64>,
    pub status: WaveFsRunStatus,
    #[schema(nullable = true, required = true)]
    pub verdict: Option<WaveFsRunVerdictSummary>,
    #[schema(value_type = Option<String>, nullable = true, required = true)]
    pub worker_card_id: Option<CardId>,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsRunEventRef {
    pub created_at: i64,
    pub event_id: i64,
    pub kind: String,
    #[schema(value_type = Value)]
    #[ts(type = "unknown")]
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsRunEvents {
    #[schema(nullable = true, required = true)]
    pub completed: Option<WaveFsRunEventRef>,
    #[schema(nullable = true, required = true)]
    pub failed: Option<WaveFsRunEventRef>,
    #[schema(nullable = true, required = true)]
    pub requested: Option<WaveFsRunEventRef>,
    #[schema(nullable = true, required = true)]
    pub verdict: Option<WaveFsRunEventRef>,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsRunDetail {
    pub events: WaveFsRunEvents,
    #[schema(nullable = true, required = true)]
    pub finished_at: Option<i64>,
    pub idempotency_key: String,
    pub kind: String,
    #[schema(nullable = true, required = true)]
    pub requested_at: Option<i64>,
    pub status: WaveFsRunStatus,
    #[schema(nullable = true, required = true)]
    pub verdict: Option<WaveFsRunVerdict>,
    #[schema(value_type = Option<String>, nullable = true, required = true)]
    pub worker_card_id: Option<CardId>,
    #[schema(value_type = Option<Value>, nullable = true, required = true)]
    #[ts(type = "unknown | null")]
    pub worker_card_payload: Option<Value>,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveFsHookEvent {
    pub created_at: i64,
    pub event_id: i64,
    pub hook_kind: String,
    pub kind: String,
    #[schema(value_type = Value)]
    #[ts(type = "unknown")]
    pub payload: Value,
}

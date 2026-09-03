//! Typed DTOs for the read-only track file JSON projections.

use crate::ids::CardId;
use crate::model::CardRole;
use serde::Serialize;
use serde_json::Value;
use std::fmt;
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsCardMeta {
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
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum TrackFsRunStatus {
    Completed,
    Failed,
    Running,
    Requested,
    Unknown,
}

impl TrackFsRunStatus {
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

impl fmt::Display for TrackFsRunStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsRunVerdictSummary {
    pub at: i64,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsRunVerdict {
    pub at: i64,
    #[schema(nullable = true, required = true)]
    pub reason: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsRunIndexEntry {
    #[schema(nullable = true, required = true)]
    pub finished_at: Option<i64>,
    pub idempotency_key: String,
    pub kind: String,
    #[schema(nullable = true, required = true)]
    pub requested_at: Option<i64>,
    pub status: TrackFsRunStatus,
    #[schema(nullable = true, required = true)]
    pub verdict: Option<TrackFsRunVerdictSummary>,
    #[schema(value_type = Option<String>, nullable = true, required = true)]
    pub worker_card_id: Option<CardId>,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsRunEventRef {
    pub created_at: i64,
    pub event_id: i64,
    pub kind: String,
    #[schema(value_type = Value)]
    #[ts(type = "unknown")]
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsRunEvents {
    #[schema(nullable = true, required = true)]
    pub completed: Option<TrackFsRunEventRef>,
    #[schema(nullable = true, required = true)]
    pub failed: Option<TrackFsRunEventRef>,
    #[schema(nullable = true, required = true)]
    pub requested: Option<TrackFsRunEventRef>,
    #[schema(nullable = true, required = true)]
    pub verdict: Option<TrackFsRunEventRef>,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsRunDetail {
    pub events: TrackFsRunEvents,
    #[schema(nullable = true, required = true)]
    pub finished_at: Option<i64>,
    pub idempotency_key: String,
    pub kind: String,
    #[schema(nullable = true, required = true)]
    pub requested_at: Option<i64>,
    pub status: TrackFsRunStatus,
    #[schema(nullable = true, required = true)]
    pub verdict: Option<TrackFsRunVerdict>,
    #[schema(value_type = Option<String>, nullable = true, required = true)]
    pub worker_card_id: Option<CardId>,
    #[schema(value_type = Option<Value>, nullable = true, required = true)]
    #[ts(type = "unknown | null")]
    pub worker_card_payload: Option<Value>,
}

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackFsHookEvent {
    pub created_at: i64,
    pub event_id: i64,
    pub hook_kind: String,
    pub kind: String,
    #[schema(value_type = Value)]
    #[ts(type = "unknown")]
    pub payload: Value,
}

//! Runtime projection vocabulary: the TS-exported wire types of calm-server's session projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

use crate::worker::WorkerSessionState;

pub type TimestampMs = i64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerSessionKind {
    #[serde(rename = "terminal")]
    Terminal,
    #[serde(rename = "codex")]
    CodexCard,
    #[serde(rename = "claude")]
    ClaudeCard,
    #[serde(rename = "shared-spec")]
    SharedPlanner,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum AgentProvider {
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "claude")]
    Claude,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct WorkerSessionProjection {
    pub id: String,
    pub card_id: String,
    pub kind: WorkerSessionKind,
    pub agent_provider: Option<AgentProvider>,
    pub status: WorkerSessionState,
    pub terminal_run_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub active_turn_id: Option<String>,
    #[ts(type = "unknown | null")]
    pub handle_state_json: Option<Value>,
    pub created_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
    pub completed_at_ms: Option<TimestampMs>,
    /// When the card's last non-interrupted turn ended; `None` when the card has no completed turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_turn_completed_ms: Option<TimestampMs>,
}

//! Runtime projection vocabulary — the data half of calm-server's
//! `session_projection_repo` module (#679 PR1).
//!
//! `WorkerSessionKind` / `AgentProvider` / `TerminalRunRef` /
//! `WorkerSessionProjection` are TS-exported wire types, so they live here in the
//! vocabulary crate. The `WorkerSessionProjectionRepo` trait, its error type and the sqlx
//! `Tx` alias stay in calm-server (IO). The whole `runtimes` family is
//! scheduled to be superseded by `worker_sessions`
//! ([`crate::worker::WorkerSession`]) across #679 PR2–PR9; this module is
//! the dual-write-window vocabulary, not the destination.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

use crate::worker::WorkerSessionState;

pub type RuntimeId = String;
pub type TimestampMs = i64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum WorkerSessionKind {
    #[serde(rename = "terminal")]
    Terminal,
    #[serde(rename = "codex")]
    CodexCard,
    #[serde(rename = "claude")]
    ClaudeCard,
    #[serde(rename = "shared-spec")]
    SharedSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum AgentProvider {
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "claude")]
    Claude,
}

/// Legacy joined-terminal projection vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct TerminalRunRef {
    pub terminal_id: String,
    pub program: String,
    pub cwd: Option<String>,
    pub pid: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WorkerSessionProjection {
    pub id: RuntimeId,
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
}

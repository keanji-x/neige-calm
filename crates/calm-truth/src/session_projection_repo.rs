use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Sqlite, Transaction};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

// Source definitions live in calm-types; do NOT re-declare them here.
pub use calm_types::runtime::{
    AgentProvider, TimestampMs, WorkerSessionKind, WorkerSessionProjection,
};
pub use calm_types::worker::WorkerSessionState;

pub type CardId = String;
pub type Tx<'a> = Transaction<'a, Sqlite>;
pub type Result<T> = std::result::Result<T, WorkerSessionProjectionRepoError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSessionProjectionRepoError {
    Message {
        message: String,
    },
    IllegalStatusTransition {
        id: String,
        attempted: WorkerSessionState,
    },
}

impl fmt::Display for WorkerSessionProjectionRepoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message { message } => formatter.write_str(message),
            Self::IllegalStatusTransition { id, attempted } => {
                write!(
                    formatter,
                    "illegal runtime status transition for {id}: {attempted:?}"
                )
            }
        }
    }
}

impl Error for WorkerSessionProjectionRepoError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadAttribution {
    pub worker_session_id: String,
    pub provider: AgentProvider,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub active_turn_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerSessionInit {
    pub id: String,
    pub card_id: CardId,
    pub kind: WorkerSessionKind,
    pub agent_provider: Option<AgentProvider>,
    pub status: WorkerSessionState,
    pub terminal_run_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub handle_state_json: Option<Value>,
    pub spawn_op_id: Option<String>,
    pub now_ms: TimestampMs,
}

impl WorkerSessionInit {
    /// A Planner harness runtime. The provider is required: the mirror persists it as the
    /// session row's provider (`resumable`, `planner`), which the kind alone cannot name.
    pub fn shared_planner(
        id: String,
        card_id: CardId,
        provider: AgentProvider,
        status: WorkerSessionState,
        thread_id: Option<String>,
        handle_state_json: Value,
        now_ms: TimestampMs,
    ) -> Self {
        Self {
            id,
            card_id,
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(provider),
            status,
            terminal_run_id: None,
            thread_id,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(handle_state_json),
            spawn_op_id: None,
            now_ms,
        }
    }
}

#[async_trait]
pub trait WorkerSessionProjectionRepo {
    /// Active = starting/running/idle/turn_pending, matching the active-per-card
    /// partial unique constraint.
    async fn session_projection_active_by_thread(
        &self,
        provider: AgentProvider,
        thread_id: &str,
    ) -> Result<Option<WorkerSessionProjection>>;

    async fn session_projection_active_by_session(
        &self,
        provider: AgentProvider,
        session_id: &str,
    ) -> Result<Option<WorkerSessionProjection>>;

    async fn session_projection_active_for_card(
        &self,
        card_id: &CardId,
    ) -> Result<Option<WorkerSessionProjection>>;

    /// The active runtime, or else a latest failed no-thread runtime so the legacy
    /// `failed_to_spawn` state still surfaces.
    async fn session_projection_projectable_for_card(
        &self,
        card_id: &CardId,
    ) -> Result<Option<WorkerSessionProjection>>;

    /// Same persisted eligibility as the explicit failed-conversation restore.
    async fn session_projection_system_error_recovery_matches(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
    ) -> Result<bool>;

    async fn session_projection_projectable_for_cards(
        &self,
        card_ids: &[CardId],
    ) -> Result<HashMap<CardId, WorkerSessionProjection>>;

    /// Active = starting/running/idle/turn_pending, matching the active-per-card
    /// partial unique constraint.
    async fn session_projection_active_shared_thread_attribution(
        &self,
    ) -> Result<Vec<(String, String)>>;

    /// Active = starting/running/idle/turn_pending, matching the active-per-card
    /// partial unique constraint.
    async fn session_projection_active_for_kind(
        &self,
        kind: WorkerSessionKind,
    ) -> Result<Vec<WorkerSessionProjection>>;

    async fn session_projection_by_id(&self, id: &str) -> Result<Option<WorkerSessionProjection>>;

    /// A runtime's own `state`, by id, independent of which runtime the card
    /// points at: `session_projection_by_id` is card-backed, so a row the card
    /// moved off answers `None` there, indistinguishable from "no such row".
    async fn session_projection_state_by_id(&self, id: &str) -> Result<Option<WorkerSessionState>>;

    /// Join-free and keyed on the id: the caller asks precisely when the card may
    /// already point somewhere else.
    async fn session_projection_handle_state_by_id(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>>;

    /// Idempotent: no active runtime for this card returns `Ok(())` without writing.
    async fn session_projection_set_status_for_card(
        &self,
        card_id: &str,
        status: WorkerSessionState,
    ) -> Result<()>;

    /// Idempotent: no active runtime for this card returns `Ok(())` without writing.
    async fn session_projection_complete_for_card(
        &self,
        card_id: &str,
        terminal_status: WorkerSessionState,
    ) -> Result<()>;

    async fn session_projection_complete_for_terminal(
        &self,
        terminal_id: &str,
        terminal_status: WorkerSessionState,
    ) -> Result<()>;

    /// Shared-spec runtimes whose `handle_state_json` carries a harness snapshot
    /// (`$.mode == 'harness'`), for the boot-time harness rebuild.
    async fn session_projection_recover_harnesses_on_boot(
        &self,
    ) -> Result<Vec<WorkerSessionProjection>>;
}

impl From<sqlx::Error> for WorkerSessionProjectionRepoError {
    fn from(err: sqlx::Error) -> Self {
        Self::Message {
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for WorkerSessionProjectionRepoError {
    fn from(err: serde_json::Error) -> Self {
        Self::Message {
            message: err.to_string(),
        }
    }
}

/// Lets the sqlite impl use `begin_immediate_tx` (which returns `CalmError`) with plain `?`.
impl From<crate::error::CalmError> for WorkerSessionProjectionRepoError {
    fn from(err: crate::error::CalmError) -> Self {
        Self::Message {
            message: err.to_string(),
        }
    }
}

impl From<WorkerSessionProjectionRepoError> for crate::error::CalmError {
    fn from(err: WorkerSessionProjectionRepoError) -> Self {
        crate::error::CalmError::Internal(err.to_string())
    }
}

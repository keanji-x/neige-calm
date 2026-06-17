use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Sqlite, Transaction};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

// #679 PR1 — moved vocabulary (TS-exported runtime projection types),
// re-exported at the old paths. Source definitions live in calm-types;
// do NOT re-declare them here.
pub use calm_types::runtime::{
    AgentProvider, RunStatus, RuntimeId, RuntimeKind, TimestampMs, WorkerSessionProjection,
};

pub type CardId = String;
pub type Tx<'a> = Transaction<'a, Sqlite>;
pub type Result<T> = std::result::Result<T, RuntimeRepoError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRepoError {
    Message { message: String },
    IllegalStatusTransition { id: RuntimeId, attempted: RunStatus },
}

impl fmt::Display for RuntimeRepoError {
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

impl Error for RuntimeRepoError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadAttribution {
    pub runtime_id: RuntimeId,
    pub provider: AgentProvider,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub active_turn_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeInit {
    pub id: RuntimeId,
    pub card_id: CardId,
    pub kind: RuntimeKind,
    pub agent_provider: Option<AgentProvider>,
    pub status: RunStatus,
    pub terminal_run_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub handle_state_json: Option<Value>,
    pub lease_owner: Option<String>,
    pub lease_until_ms: Option<TimestampMs>,
    pub spawn_op_id: Option<String>,
    pub now_ms: TimestampMs,
}

#[async_trait]
pub trait RuntimeRepo {
    /// Active = starting/running/idle/turn_pending, matching the
    /// active-per-card partial unique constraint. Looks up a worker session by
    /// provider-owned thread id for bridge/app-server attribution.
    async fn runtime_get_active_by_thread(
        &self,
        provider: AgentProvider,
        thread_id: &str,
    ) -> Result<Option<WorkerSessionProjection>>;

    async fn runtime_get_active_by_session(
        &self,
        provider: AgentProvider,
        session_id: &str,
    ) -> Result<Option<WorkerSessionProjection>>;

    async fn runtime_get_active_for_card(
        &self,
        card_id: &CardId,
    ) -> Result<Option<WorkerSessionProjection>>;

    /// Runtime row used by read-time payload projection. This preserves the
    /// active-runtime lookup as the primary source, but also allows a latest
    /// failed no-thread runtime to surface the legacy `failed_to_spawn` state.
    async fn runtime_get_projectable_for_card(
        &self,
        card_id: &CardId,
    ) -> Result<Option<WorkerSessionProjection>>;

    async fn runtime_get_projectable_for_cards(
        &self,
        card_ids: &[CardId],
    ) -> Result<HashMap<CardId, WorkerSessionProjection>>;

    /// Active = starting/running/idle/turn_pending, matching the
    /// active-per-card partial unique constraint. Returns codex-owned
    /// thread attributions used to rebuild the shared app-server cache.
    async fn runtime_active_shared_thread_attribution(&self) -> Result<Vec<(String, String)>>;

    /// Active = starting/running/idle/turn_pending, matching the
    /// active-per-card partial unique constraint. Batch scan for boot
    /// takeover flows that need all live runtimes of a specific kind.
    async fn runtimes_active_for_kind(
        &self,
        kind: RuntimeKind,
    ) -> Result<Vec<WorkerSessionProjection>>;

    async fn runtime_get_by_id(&self, id: &RuntimeId) -> Result<Option<WorkerSessionProjection>>;

    /// Idempotent: if no active runtime exists for this card, returns
    /// `Ok(())` without writing. This handles fast-exit races and
    /// pre-#488-backfilled-but-already-completed cards.
    async fn runtime_set_status_for_card(&self, card_id: &str, status: RunStatus) -> Result<()>;

    /// Idempotent: if no active runtime exists for this card, returns
    /// `Ok(())` without writing. This handles fast-exit races and
    /// pre-#488-backfilled-but-already-completed cards.
    async fn runtime_complete_for_card(
        &self,
        card_id: &str,
        terminal_status: RunStatus,
    ) -> Result<()>;

    async fn runtime_complete_for_terminal(
        &self,
        terminal_id: &str,
        terminal_status: RunStatus,
    ) -> Result<()>;

    /// Returns shared-spec runtimes whose `handle_state_json` carries a harness
    /// snapshot (`$.mode == 'harness'`) so the spec harness boot path can
    /// rebuild their in-memory task + replay pending observations.
    async fn runtimes_recover_harnesses_on_boot(&self) -> Result<Vec<WorkerSessionProjection>>;
}

impl From<sqlx::Error> for RuntimeRepoError {
    fn from(err: sqlx::Error) -> Self {
        Self::Message {
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for RuntimeRepoError {
    fn from(err: serde_json::Error) -> Self {
        Self::Message {
            message: err.to_string(),
        }
    }
}

impl From<RuntimeRepoError> for crate::error::CalmError {
    fn from(err: RuntimeRepoError) -> Self {
        crate::error::CalmError::Internal(err.to_string())
    }
}

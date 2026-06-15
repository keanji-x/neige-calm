use async_trait::async_trait;
use calm_types::worker::{Liveness, WorkerSession, WorkerSessionId, WorkerSessionState};
use sqlx::{Sqlite, Transaction};

use crate::error::Result;
use crate::ids::WaveId;

pub type Tx<'a> = Transaction<'a, Sqlite>;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum CommitExitOutcome {
    Committed(WorkerSession),
    Absorbed,
}

#[async_trait]
pub trait SessionRepo: Send + Sync {
    async fn session_insert_tx(
        &self,
        tx: &mut Tx<'_>,
        session: WorkerSession,
    ) -> Result<WorkerSession>;

    async fn session_get(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>>;

    async fn sessions_nonterminal(&self) -> Result<Vec<WorkerSession>>;

    async fn session_set_liveness(
        &self,
        id: &WorkerSessionId,
        liveness: &Liveness,
        probed_at_ms: i64,
    ) -> Result<Option<WorkerSession>>;

    async fn session_state_transition_tx(
        &self,
        tx: &mut Tx<'_>,
        id: &WorkerSessionId,
        to: WorkerSessionState,
    ) -> Result<WorkerSession>;

    async fn session_commit_exit(
        &self,
        id: &WorkerSessionId,
        to: WorkerSessionState,
        liveness_probed_at_ms: i64,
        exit_code: Option<i32>,
        exit_interpretation: &str,
    ) -> Result<CommitExitOutcome>;

    async fn session_list_by_wave(&self, wave_id: &WaveId) -> Result<Vec<WorkerSession>>;
}

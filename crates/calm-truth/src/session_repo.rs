use async_trait::async_trait;
use calm_types::worker::{WorkerSession, WorkerSessionId, WorkerSessionState};
use sqlx::{Sqlite, Transaction};

use crate::error::Result;
use crate::ids::WaveId;

pub type Tx<'a> = Transaction<'a, Sqlite>;

#[async_trait]
pub trait SessionRepo: Send + Sync {
    async fn session_insert_tx(
        &self,
        tx: &mut Tx<'_>,
        session: WorkerSession,
    ) -> Result<WorkerSession>;

    async fn session_get(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>>;

    async fn session_state_transition_tx(
        &self,
        tx: &mut Tx<'_>,
        id: &WorkerSessionId,
        to: WorkerSessionState,
    ) -> Result<WorkerSession>;

    async fn session_list_by_wave(&self, wave_id: &WaveId) -> Result<Vec<WorkerSession>>;
}

//! Execution ownership: consumers submit requests; only this module creates launch capabilities.
mod backend;
mod storage;

use crate::error::{CalmError, Result};
use crate::model::now_ms;
use sqlx::SqlitePool;
use backend::{Backend, LaunchOutcome, Observation};
use storage::Reservation;

pub(crate) struct ExecutionManager {
    pool: SqlitePool,
}
impl ExecutionManager {
    pub(crate) fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub(super) async fn submit<B: Backend>(
        &self, backend: &B, owner: &Owner, request: B::Request, preferred_nonce: Option<&str>,
    ) -> Result<Receipt> {
        let reservation = Reservation::acquire(&self.pool, owner).await?;
        let nonce = match reservation.nonce(preferred_nonce).await {
            Ok(nonce) => nonce,
            Err(error) => { reservation.reject().await?; return Err(error); }
        };
        let record = storage::load(&self.pool, reservation.id()).await?
            .ok_or_else(|| CalmError::Conflict("execution disappeared before launch".into()))?;
        let permit = match record.access {
            calm_types::workspace_access::WorkspaceAccess::ReadOnly => LaunchPermit::Read(ReadPermit { record, nonce }),
            calm_types::workspace_access::WorkspaceAccess::ReadWrite => LaunchPermit::Write(WritePermit { record, nonce }),
        };
        match backend.launch(permit, request).await {
            LaunchOutcome::Started(identity) => {
                let id = reservation.id().to_owned();
                let stopped = reservation.started(&self.pool, &identity).await?;
                Ok(Receipt { execution_id: id, identity, stopped })
            }
            LaunchOutcome::NotIssued(error) => { reservation.reject().await?; Err(error) }
            LaunchOutcome::Uncertain(error) => Err(error),
        }
    }

    pub(super) async fn recover<B: Backend>(&self, backend: &B, execution: &str) -> Result<bool> {
        let Some(record) = storage::load(&self.pool, execution).await? else { return Ok(true); };
        let observation = backend.recover(&record).await?;
        self.apply_observation(&record, observation).await
    }

    pub(super) async fn cancel<B: Backend>(&self, backend: &B, execution: &str) -> Result<bool> {
        storage::request_stop(&self.pool, execution).await?;
        let Some(record) = storage::load(&self.pool, execution).await? else { return Ok(true); };
        let observation = backend.stop(&record).await?;
        self.apply_observation(&record, observation).await
    }

    async fn apply_observation(&self, record: &Record, observation: Observation) -> Result<bool> {
        if observation.execution != record.id {
            return Err(CalmError::Conflict("stop evidence belongs to another execution generation".into()));
        }
        if let Some(identity) = observation.identity.as_deref() {
            storage::observe(&self.pool, record, identity).await?;
        }
        if !observation.stopped { return Ok(false); }
        let identity = observation.identity.ok_or_else(|| CalmError::Conflict("stopped execution has no observed identity".into()))?;
        storage::release_confirmed(&self.pool, record, &identity, now_ms()).await
    }
}

pub(crate) struct ReadPermit { record: Record, nonce: String }
pub(crate) struct WritePermit { record: Record, nonce: String }
pub(super) enum LaunchPermit { Read(ReadPermit), Write(WritePermit) }
impl LaunchPermit {
    pub(super) fn record(&self) -> &Record {
        match self { Self::Read(p) => &p.record, Self::Write(p) => &p.record }
    }
    pub(super) fn nonce(&self) -> &str {
        match self { Self::Read(p) => &p.nonce, Self::Write(p) => &p.nonce }
    }
}

pub(super) struct Owner { pub card: String, pub holder: String }
pub(super) struct Receipt { pub execution_id: String, pub identity: String, pub stopped: bool }
pub(super) struct Record {
    pub id: String, pub holder: String, pub phase: String, pub nonce: Option<String>,
    pub observed: Option<String>, pub cwd: String, pub access: calm_types::workspace_access::WorkspaceAccess,
}

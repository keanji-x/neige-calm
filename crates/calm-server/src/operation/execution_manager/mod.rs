//! Execution ownership: consumers submit requests; only this module creates launch capabilities.
mod native;
pub use native::*;
pub(crate) use native::{DeletionThreadSeals, redact_thread_start_config};
mod backend;
mod storage;

use crate::error::{CalmError, Result};
use crate::model::now_ms;
use backend::{Backend, LaunchOutcome, Observation};
use sqlx::SqlitePool;
use storage::Reservation;

pub(crate) struct ExecutionManager {
    pool: SqlitePool,
}
impl ExecutionManager {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn submit<B: Backend>(
        &self,
        backend: &B,
        owner: &Owner,
        request: B::Request,
        preferred_nonce: Option<&str>,
    ) -> Result<Receipt> {
        self.submit_with_policy(backend, owner, request, preferred_nonce, None)
            .await
    }
    async fn submit_with_policy<B: Backend>(
        &self,
        backend: &B,
        owner: &Owner,
        request: B::Request,
        preferred_nonce: Option<&str>,
        policy: Option<PermissionsChoice>,
    ) -> Result<Receipt> {
        let reservation = Reservation::acquire(&self.pool, owner).await?;
        let nonce = match reservation.nonce(preferred_nonce).await {
            Ok(nonce) => nonce,
            Err(error) => {
                reservation.reject().await?;
                return Err(error);
            }
        };
        let record = storage::load(&self.pool, reservation.id())
            .await?
            .ok_or_else(|| CalmError::Conflict("execution disappeared before launch".into()))?;
        let permit = match record.access {
            calm_types::workspace_access::WorkspaceAccess::ReadOnly => {
                LaunchPermit::Read(ReadPermit {
                    record,
                    nonce,
                    policy,
                })
            }
            calm_types::workspace_access::WorkspaceAccess::ReadWrite => {
                LaunchPermit::Write(WritePermit {
                    record,
                    nonce,
                    policy,
                })
            }
        };
        match backend.launch(permit, request).await {
            LaunchOutcome::Started(identity) => {
                let id = reservation.id().to_owned();
                let stopped = reservation.started(&self.pool, &identity).await?;
                Ok(Receipt {
                    execution_id: id,
                    identity,
                    stopped,
                })
            }
            LaunchOutcome::NotIssued(error) => {
                reservation.reject().await?;
                Err(error)
            }
            LaunchOutcome::Uncertain(error) => Err(error),
        }
    }

    async fn launch_reserved<B: Backend>(
        &self,
        backend: &B,
        execution: &str,
        request: B::Request,
    ) -> Result<Receipt> {
        let record = storage::load(&self.pool, execution)
            .await?
            .ok_or_else(|| CalmError::Conflict("prepared execution is not held".into()))?;
        if record.backend != backend.kind()
            || record.backend != BackendKind::NativeSession
            || record.phase != "issuing"
            || record.access != calm_types::workspace_access::WorkspaceAccess::ReadWrite
        {
            return Err(CalmError::Conflict(
                "prepared session cannot be launched again".into(),
            ));
        }
        storage::claim_session(&self.pool, execution).await?;
        let permit = LaunchPermit::Write(WritePermit {
            nonce: record.id.clone(),
            record,
            policy: None,
        });
        match backend.launch(permit, request).await {
            LaunchOutcome::Started(identity) => {
                let stopped = storage::session_started(&self.pool, execution, &identity).await?;
                Ok(Receipt {
                    execution_id: execution.to_owned(),
                    identity,
                    stopped,
                })
            }
            LaunchOutcome::NotIssued(error) => {
                storage::reject_session(&self.pool, execution).await?;
                Err(error)
            }
            LaunchOutcome::Uncertain(error) => Err(error),
        }
    }

    async fn recover<B: Backend>(&self, backend: &B, execution: &str) -> Result<bool> {
        let Some(record) = storage::load(&self.pool, execution).await? else {
            return Ok(true);
        };
        if backend.kind() != record.backend {
            return Err(CalmError::Conflict(
                "execution belongs to another backend".into(),
            ));
        }
        let observation = backend.recover(&record).await?;
        self.apply_observation(&record, observation).await
    }

    async fn cancel<B: Backend>(&self, backend: &B, execution: &str) -> Result<bool> {
        let Some(record) = storage::load(&self.pool, execution).await? else {
            return Ok(true);
        };
        if backend.kind() != record.backend {
            return Err(CalmError::Conflict(
                "execution belongs to another backend".into(),
            ));
        }
        storage::request_stop(&self.pool, execution).await?;
        let observation = backend.stop(&record).await?;
        self.apply_observation(&record, observation).await
    }

    async fn apply_observation(&self, record: &Record, observation: Observation) -> Result<bool> {
        if observation.execution != record.id {
            return Err(CalmError::Conflict(
                "stop evidence belongs to another execution generation".into(),
            ));
        }
        if let Some(identity) = observation.identity.as_deref() {
            storage::observe(&self.pool, record, identity).await?;
        }
        if !observation.stopped {
            return Ok(false);
        }
        let identity = observation.identity.ok_or_else(|| {
            CalmError::Conflict("stopped execution has no observed identity".into())
        })?;
        storage::release_confirmed(&self.pool, record, &identity, now_ms()).await
    }
}

pub(crate) struct ReadPermit {
    record: Record,
    nonce: String,
    policy: Option<PermissionsChoice>,
}
pub(crate) struct WritePermit {
    record: Record,
    nonce: String,
    policy: Option<PermissionsChoice>,
}
pub(super) enum LaunchPermit {
    Read(ReadPermit),
    Write(WritePermit),
}
impl LaunchPermit {
    fn policy(&self) -> Option<&PermissionsChoice> {
        match self {
            Self::Read(p) => p.policy.as_ref(),
            Self::Write(p) => p.policy.as_ref(),
        }
    }
    pub(super) fn record(&self) -> &Record {
        match self {
            Self::Read(p) => &p.record,
            Self::Write(p) => &p.record,
        }
    }
    pub(super) fn nonce(&self) -> &str {
        match self {
            Self::Read(p) => &p.nonce,
            Self::Write(p) => &p.nonce,
        }
    }
}

pub(super) struct Owner {
    pub card: String,
    pub holder: String,
}
pub(super) struct Receipt {
    pub execution_id: String,
    pub identity: String,
    pub stopped: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BackendKind {
    NativeTurn,
    NativeSession,
}

pub(super) struct Record {
    pub backend: BackendKind,
    pub id: String,
    pub holder: String,
    pub phase: String,
    pub nonce: Option<String>,
    pub observed: Option<String>,
    pub cwd: String,
    pub access: calm_types::workspace_access::WorkspaceAccess,
}

#[cfg(test)]
mod tests;

/// Called from the existing business-state transaction, without granting release authority.
pub(crate) async fn task_ended_tx(
    tx: &mut crate::operation::Tx<'_>,
    card: &str,
    delivery: crate::operation::workspace_lease::ReleaseDelivery,
) -> Result<
    Vec<(
        crate::ids::ActorId,
        crate::event::EventScope,
        crate::event::Event,
    )>,
> {
    storage::task_ended_tx(tx, card, delivery).await
}

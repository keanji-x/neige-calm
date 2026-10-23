//! Provider-owned admission and completion. Engine alone establishes process settlement.
use crate::{
    db::sqlite::SqliteDb,
    error::{CalmError, Result},
    execution_boot::ExecutionEngine,
};
use async_trait::async_trait;
use calm_truth::db::sqlite::{provider_execution as sql, workspace_execution as executions};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use workspace_runtime::execution::*;

pub(crate) mod launch;
pub mod release;
pub mod stderr;
pub mod stdio;
pub mod sweep;
pub use sql::{Claim, Decision, Purpose, Record, StopReason};

pub enum CompletionEvidence {
    Settled(Settlement),
    Unconfirmed,
}
/// Protocol results remain provider-owned; rewind has no durable outcome.
pub enum KnownEnd {
    RewindCheck,
    ClaudeTurn(Box<crate::claude_planner::execution::End>),
    ClaudeRefused,
    AcpTurn(Box<crate::acp_planner::execution::End>),
    AcpRefused,
}
#[async_trait]
pub trait ProviderExecutionOwner: Send + Sync {
    fn consumer(&self) -> &'static str;
    /// A captured record may no longer own the session. Guard session-wide
    /// writes by exact execution ownership inside their writer transaction;
    /// scope other effects to this execution or its turn.
    async fn complete(
        &self,
        session: &str,
        record: &Record,
        evidence: CompletionEvidence,
        known: Option<KnownEnd>,
    ) -> Result<()>;
}

#[derive(Clone, Default, Debug)]
pub struct ProviderDrivers(Arc<Mutex<HashSet<ExecutionId>>>);
impl ProviderDrivers {
    pub fn contains(&self, id: &ExecutionId) -> bool {
        self.0.lock().unwrap().contains(id)
    }
    fn claim(&self, id: ExecutionId) -> DriverGuard {
        assert!(self.0.lock().unwrap().insert(id.clone()));
        DriverGuard {
            drivers: self.clone(),
            id,
        }
    }
}
pub struct DriverGuard {
    drivers: ProviderDrivers,
    id: ExecutionId,
}
impl Drop for DriverGuard {
    fn drop(&mut self) {
        self.drivers.0.lock().unwrap().remove(&self.id);
        #[cfg(feature = "fixtures")]
        calm_truth::test_seam::task_drop_pause_point("c4-driver-dropped", self.id.as_str());
    }
}

#[derive(Clone)]
pub struct ProviderExecution {
    pub owner: Arc<dyn ProviderExecutionOwner>,
    pub drivers: ProviderDrivers,
}
impl std::fmt::Debug for ProviderExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderExecution")
            .field("consumer", &self.owner.consumer())
            .finish()
    }
}
pub fn conflict(error: impl std::fmt::Debug) -> CalmError {
    CalmError::Conflict(format!("provider execution: {error:?}"))
}
pub fn engine(engine: Option<&ExecutionEngine>) -> Result<&ExecutionEngine> {
    engine
        .filter(|engine| engine.available())
        .ok_or(CalmError::ExecutionUnavailable)
}
pub fn execution_id(record: &Record) -> Result<ExecutionId> {
    record.execution_id.clone().try_into().map_err(conflict)
}
pub async fn settlement(pool: &SqliteDb, id: &ExecutionId) -> Result<Option<Settlement>> {
    let row = executions::load(pool.read_pool(), id.as_str())
        .await?
        .ok_or_else(|| conflict("execution disappeared"))?;
    row.settlement_json
        .map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}
/// Wait within the owner's settlement budget. Engine's individual wait deadline
/// bounds each observation; its timeout is not evidence that Stop failed.
pub async fn wait_settled(
    engine: &ExecutionEngine,
    id: &ExecutionId,
    budget: Duration,
) -> Result<()> {
    tokio::time::timeout(budget, async {
        loop {
            match engine.wait_settled(id).await {
                Ok(()) => return Ok(()),
                Err(DriveError::Timeout) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) => return Err(conflict(error)),
            }
        }
    })
    .await
    .map_err(conflict)?
}

/// Caller holds the session issue lock. Native consumers have no completion port yet:
/// they must retain any outstanding claim rather than complete another provider's work.
pub(crate) async fn admission_gate(
    pool: &SqliteDb,
    engine: Option<&ExecutionEngine>,
    session: &str,
    owner: Option<&ProviderExecution>,
) -> Result<()> {
    let Some(claim) = sql::load(pool.read_pool(), session).await? else {
        return Ok(());
    };
    if let Some(owner) = owner {
        let id = execution_id(claim.record())?;
        if !owner.drivers.contains(&id) && settlement(pool, &id).await?.is_some() {
            owner.finish(pool, engine, session, &claim, None).await?;
            if sql::load(pool.read_pool(), session).await?.is_none() {
                return Ok(());
            }
        }
    }
    Err(conflict("the previous provider execution is not complete"))
}
/// Compose an exact owner-claim Stop with the caller's writer transaction. This
/// performs no owner I/O; batch lifecycle callers commit once before driving.
pub(crate) async fn request_stop_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: &str,
    claimed: &Claim,
    reason: StopReason,
) -> Result<Option<ExecutionId>> {
    let id = execution_id(claimed.record())?;
    let current = sql::load(&mut **tx, session).await?;
    let Some(current) =
        current.filter(|c| c.record().execution_id == claimed.record().execution_id)
    else {
        return Ok(None);
    };
    if current.record().decision == Decision::Observing {
        let sql::Cas::Applied(_) =
            sql::stop(tx, session, &current, reason, crate::model::now_ms()).await?
        else {
            return Ok(None);
        };
    }
    executions::request(tx, id.as_str(), "stop").await?;
    Ok(Some(id))
}

impl ProviderExecution {
    /// Caller holds its session issue lock. SQL errors are never absence.
    pub async fn admission_gate(
        &self,
        pool: &SqliteDb,
        engine: Option<&ExecutionEngine>,
        session: &str,
    ) -> Result<()> {
        admission_gate(pool, engine, session, Some(self)).await
    }
    /// ① Driver registration precedes COMMIT; RAII covers cancellation during commit.
    pub async fn admit(
        &self,
        pool: &SqliteDb,
        engine: &ExecutionEngine,
        session: &str,
        intent: ExecutionIntent,
        purpose: Purpose,
    ) -> Result<(Claim, DriverGuard)> {
        let engine = self::engine(Some(engine))?;
        let id = intent.metadata.id.clone();
        let guard = self.drivers.claim(id.clone());
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(pool).await?;
        let sql::Cas::Applied(claim) = sql::admit(
            &mut tx,
            session,
            id.as_str().into(),
            purpose,
            sql::InitialRelease::Held,
        )
        .await?
        else {
            return Err(conflict("admission lost"));
        };
        let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
        tx.commit().await?;
        engine.drive(ticket.committed());
        Ok((claim, guard))
    }
    /// ③ Re-read on CAS loss; never adopt another execution or overwrite Stopping.
    pub async fn stop(
        &self,
        pool: &SqliteDb,
        engine: Option<&ExecutionEngine>,
        session: &str,
        claimed: &Claim,
        reason: StopReason,
    ) -> Result<()> {
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(pool).await?;
        let Some(id) = request_stop_tx(&mut tx, session, claimed, reason).await? else {
            return Ok(());
        };
        tx.commit().await?;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c4-stop-committed", id.as_str()).await;
        crate::execution_quiesce::drive(engine, [id]);
        Ok(())
    }
    /// Owner confirmation precedes ④. Re-check settlement and exact claim in its transaction.
    pub async fn finish(
        &self,
        pool: &SqliteDb,
        engine: Option<&ExecutionEngine>,
        session: &str,
        claimed: &Claim,
        known: Option<KnownEnd>,
    ) -> Result<()> {
        let id = execution_id(claimed.record())?;
        let Some(settled) = settlement(pool, &id).await? else {
            return Err(conflict("not settled"));
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            self.owner.complete(
                session,
                claimed.record(),
                CompletionEvidence::Settled(settled),
                known,
            ),
        )
        .await
        .map_err(conflict)??;
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(pool).await?;
        let Some(current) = sql::load(&mut **tx, session)
            .await?
            .filter(|c| c.record().execution_id == id.as_str())
        else {
            return Ok(());
        };
        let settled = executions::settlement(&mut tx, id.as_str()).await?;
        if settled.is_none() {
            return Err(conflict("not settled"));
        }
        let sql::Cas::Applied(()) = sql::clear_on_completion(&mut tx, session, &current).await?
        else {
            return Ok(());
        };
        executions::request(&mut tx, id.as_str(), "retire").await?;
        tx.commit().await?;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c4-retire-committed", id.as_str()).await;
        crate::execution_quiesce::drive(engine, [id]);
        Ok(())
    }
}

/// Explicit projection registration: provider owners complete outside store transactions.
pub struct Projection;
#[async_trait]
impl crate::execution_store::ExecutionProjection for Projection {
    async fn spawned(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        _: &str,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        _: &str,
        _: &Settlement,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
}

pub(crate) struct Cancellation {
    pub(crate) owner: ProviderExecution,
    pub(crate) pool: crate::db::sqlite::SqliteDb,
    pub(crate) engine: crate::execution_boot::ExecutionEngine,
    pub(crate) session: String,
    pub(crate) claim: Claim,
    pub(crate) armed: bool,
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let owner = self.owner.clone();
        let pool = self.pool.clone();
        let engine = self.engine.clone();
        let session = self.session.clone();
        let claim = self.claim.clone();
        tokio::spawn(async move {
            if let Err(error) = owner
                .stop(
                    &pool,
                    Some(&engine),
                    &session,
                    &claim,
                    StopReason::DriverLost,
                )
                .await
            {
                tracing::warn!(%error, "provider cancellation Stop failed; durable sweep will retry");
            }
        });
    }
}

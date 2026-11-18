//! Engine ownership for one resident ACP generation. No card owns its process.
use super::config::{AcpPlannerHost, Registration};
use super::profile::{AcpAgentProfile, ResidentRestart};
use crate::{
    db::sqlite::SqliteDb,
    error::{CalmError, Result},
    execution_boot::ExecutionEngine,
    provider_execution::{
        self, conflict, launch::LaunchOwner, release, stderr, stdio::EngineStdio,
    },
};
use calm_truth::db::sqlite::acp_resident;
use calm_truth::db::sqlite::workspace_execution as sql;
use calm_types::runtime::AgentProvider;
use std::{
    future::Future,
    path::{Path, PathBuf},
};
use workspace_runtime::execution::*;

pub(crate) const CONSUMER: &str = "acp-resident";
#[cfg(test)]
mod tests;

fn generation_alias(backend: &str, id: &ExecutionId) -> String {
    format!("{CONSUMER}:{backend}:{}", id.as_str())
}

/// The admitted workspace binding of one resident. Values contain no environment secrets.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ResidentKey {
    provider: &'static str,
    track: String,
    cwd: PathBuf,
}
impl ResidentKey {
    pub(crate) fn resolve(provider: &AgentProvider, track: &str, cwd: &Path) -> Result<Self> {
        Ok(Self {
            provider: provider.wire_name(),
            track: track.into(),
            cwd: std::fs::canonicalize(cwd)?,
        })
    }

    fn owner(&self) -> OperationRef {
        let mut hash = blake3::Hasher::new();
        for part in [
            self.provider.as_bytes(),
            self.track.as_bytes(),
            self.cwd.as_os_str().as_encoded_bytes(),
        ] {
            hash.update(&(part.len() as u64).to_le_bytes());
            hash.update(part);
        }
        OperationRef(format!(
            "{CONSUMER}:{}:workspace:{}",
            self.provider,
            hash.finalize().to_hex()
        ))
    }

    fn alias(&self, id: &ExecutionId) -> ProcessAlias {
        ProcessAlias(format!("{}:{}", self.owner().0, id.as_str()))
    }
}

pub(crate) struct ResidentExecution {
    id: ExecutionId,
    engine: ExecutionEngine,
    pool: SqliteDb,
    drained: bool,
}

impl ResidentExecution {
    pub(crate) async fn start<F>(
        host: &AcpPlannerHost,
        registration: Registration<'_>,
        track_id: &str,
        cwd: &Path,
        revalidate: impl FnOnce() -> F + Send,
    ) -> Result<(
        Self,
        EngineStdio,
        Option<super::environment::ShellEnvironment>,
    )>
    where
        F: Future<Output = Result<()>> + Send,
    {
        let engine = provider_execution::engine(host.execution.engine())?.clone();
        let pool = host
            .completion
            .repo()?
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        let backend = registration.config.provider.wire_name();
        let key = ResidentKey::resolve(&registration.config.provider, track_id, cwd)?;
        let (environment, hook) = super::environment::track_values(
            registration,
            host.completion.repo()?.as_ref(),
            track_id,
            cwd,
            &host.agent_logs,
            revalidate,
        )
        .await?;
        let launch = registration
            .profile
            .resident
            .ok_or_else(|| conflict("ACP resident launch required"))?
            .launch;
        let (args, payload) =
            super::execution::process_launch(registration, launch, [], environment, hook.as_ref())?;
        // A cancelled initialize can leave a committed generation without an
        // in-memory server. Settle that exact owner before starting another.
        stop_inventory(&pool, Some(&engine), Some(backend), Some(&key.owner().0)).await?;
        let id = ExecutionId::fresh()?;
        let dir = stderr::create(&host.agent_logs, id.as_str())?;
        let intent = provider_execution::launch::wrapped_intent(
            LaunchOwner {
                consumer: CONSUMER.into(),
                owner: key.owner(),
                alias: key.alias(&id),
                scope: ExecutionOwnership::Track(TrackId(track_id.into())),
            },
            cwd,
            &id,
            &dir,
            &registration.config.command,
            args,
            payload.public,
            &payload.read_prefix,
            AmbientEnvironment::None,
        )
        .await?;
        // Arm before the commit await. A lost answer is reconciled from the
        // same ID by Drop, or from durable inventory on the next start.
        let mut owned = Self {
            id: id.clone(),
            engine: engine.clone(),
            pool: pool.clone(),
            drained: false,
        };
        let admit_engine = engine.clone();
        let admitted = crate::db::write_in_tx_typed(host.completion.repo()?.as_ref(), move |tx| {
            Box::pin(async move { admit_engine.admit(tx, intent).await.map_err(conflict) })
        })
        .await;
        let ticket = match admitted {
            Ok(ticket) => ticket,
            Err(error) => {
                if sql::load(pool.read_pool(), id.as_str()).await?.is_none() {
                    owned.drained = true;
                }
                return Err(error);
            }
        };
        engine.drive(ticket.committed());
        release::wait_spawned(&engine, &id).await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            release::write_payload(&engine, &id, &payload.payload),
        )
        .await
        .map_err(conflict)??;
        let io = EngineStdio::new(engine, id);
        Ok((owned, io, hook))
    }

    pub(crate) fn id(&self) -> &ExecutionId {
        &self.id
    }

    pub(crate) async fn stop(&mut self) -> Result<()> {
        if !self.drained {
            drain(&self.pool, &self.engine, &self.id, false).await?;
            self.drained = true;
        }
        Ok(())
    }
}

impl Drop for ResidentExecution {
    fn drop(&mut self) {
        if self.drained {
            return;
        }
        let (pool, engine, id) = (self.pool.clone(), self.engine.clone(), self.id.clone());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = drain(&pool, &engine, &id, false).await {
                    tracing::warn!(execution = id.as_str(), %error, "ACP generation retained for recovery");
                }
            });
        }
    }
}

pub(crate) async fn stop_previous(
    pool: &SqliteDb,
    engine: Option<&ExecutionEngine>,
    backend: Option<&str>,
) -> Result<()> {
    stop_inventory(pool, engine, backend, None).await
}

async fn stop_inventory(
    pool: &SqliteDb,
    engine: Option<&ExecutionEngine>,
    backend: Option<&str>,
    owner: Option<&str>,
) -> Result<()> {
    // Fence every previous generation in one writer transaction before boot
    // recovery may drive any committed Run intent.
    let backend = backend.map(str::to_owned);
    let owner = owner.map(str::to_owned);
    let boot = backend.is_none();
    let ids = pool
        .writer()
        .with_writer(|writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await?;
                let mut ids = Vec::new();
                for execution_id in sql::resident_inventory_tx(
                    &mut tx,
                    CONSUMER,
                    backend.as_deref(),
                    owner.as_deref(),
                )
                .await?
                {
                    let id = ExecutionId::try_from(execution_id).map_err(conflict)?;
                    sql::request(&mut tx, id.as_str(), "stop").await?;
                    ids.push(id);
                }
                tx.commit().await?;
                Ok::<_, CalmError>(ids)
            })
        })
        .await
        .map_err(calm_truth::TruthError::from)??;
    for id in ids {
        let result =
            async { drain(pool, provider_execution::engine(engine)?, &id, true).await }.await;
        if !boot {
            result?;
        } else if let Err(error) = result {
            tracing::warn!(execution = id.as_str(), %error, "ACP resident workspace fence retained after restart");
        }
    }
    Ok(())
}

pub(crate) async fn workspace_generations(
    host: &AcpPlannerHost,
    workers: &[&str],
) -> Result<(SqliteDb, Vec<String>)> {
    let pool = host
        .completion
        .repo()?
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let mut ids = Vec::new();
    for worker in workers {
        ids.extend(acp_resident::for_worker(pool.read_pool(), worker).await?);
    }
    ids.sort_unstable();
    ids.dedup();
    Ok((pool, ids))
}

/// The caller snapshots the generations before taking the live slot's locks,
/// then checks those slots for routed siblings. A later mount cannot enter this stop.
pub(crate) async fn stop_detached_workspace(
    host: &AcpPlannerHost,
    pool: &SqliteDb,
    previous: Vec<String>,
) -> Result<()> {
    for id in previous {
        let id = ExecutionId::try_from(id).map_err(conflict)?;
        drain(
            pool,
            provider_execution::engine(host.execution.engine())?,
            &id,
            false,
        )
        .await?;
    }
    Ok(())
}

async fn request(
    pool: &SqliteDb,
    engine: &ExecutionEngine,
    id: &ExecutionId,
    desired: Desired,
) -> Result<()> {
    let engine_for_write = engine.clone();
    let id = id.clone();
    let ticket = pool
        .writer()
        .with_writer(|writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await?;
                let ticket = engine_for_write
                    .request(&mut tx, &id, desired)
                    .await
                    .map_err(conflict)?;
                if desired == Desired::Retire {
                    acp_resident::clear_generation(&mut tx, id.as_str()).await?;
                }
                tx.commit().await?;
                Ok::<_, CalmError>(ticket)
            })
        })
        .await
        .map_err(calm_truth::TruthError::from)??;
    engine.drive(ticket.committed());
    Ok(())
}

async fn drain(
    pool: &SqliteDb,
    engine: &ExecutionEngine,
    id: &ExecutionId,
    restarting: bool,
) -> Result<()> {
    request(pool, engine, id, Desired::Stop).await?;
    provider_execution::wait_settled(engine, id, QUIESCE_DEADLINE).await?;
    let settlement = provider_execution::settlement(pool, id).await?;
    let restart_proof = if restarting
        && matches!(
            settlement,
            Some(Settlement::Journal(
                ExecutionState::InterruptedQuiescent { .. }
            ))
        ) {
        interrupted_restart_allowed(pool, id).await?
    } else {
        false
    };
    if !restart_proof
        && !settlement.is_some_and(|settlement| {
            settlement.descendants_complete()
                || matches!(
                    settlement,
                    Settlement::Journal(ExecutionState::NoChild { .. } | ExecutionState::Revoked)
                )
                || matches!(
                    settlement,
                    Settlement::Retired(tombstone)
                        if tombstone.execution_id == *id
                            && tombstone.outcome == TombstoneOutcome::Revoked
                )
        })
    {
        return Err(conflict("resident ACP descendants have not drained"));
    }
    request(pool, engine, id, Desired::Retire).await?;
    engine.wait_retired(id).await.map_err(conflict)
}

fn accepts_interrupted_restart(provider: &AgentProvider) -> bool {
    AcpAgentProfile::of(provider)
        .and_then(|profile| profile.resident)
        .is_some_and(|resident| resident.restart == ResidentRestart::ResumeAfterOwnerExit)
}

async fn interrupted_restart_allowed(pool: &SqliteDb, id: &ExecutionId) -> Result<bool> {
    let Some(row) = sql::load(pool.read_pool(), id.as_str()).await? else {
        return Ok(false);
    };
    let intent: ExecutionMetadata = serde_json::from_str(&row.intent_json)?;
    Ok(AgentProvider::ALL.iter().any(|provider| {
        resident_owner_matches(&intent, provider, id) && accepts_interrupted_restart(provider)
    }))
}

fn resident_owner_matches(
    intent: &ExecutionMetadata,
    provider: &AgentProvider,
    id: &ExecutionId,
) -> bool {
    let backend = provider.wire_name();
    intent.id == *id
        && intent.consumer.0 == CONSUMER
        && match &intent.scope {
            ExecutionOwnership::Host { backend: owner } => {
                owner == backend && intent.alias.0 == generation_alias(backend, id)
            }
            ExecutionOwnership::Track(track) => {
                let key = ResidentKey {
                    provider: backend,
                    track: track.0.clone(),
                    cwd: intent.source.root.clone(),
                };
                intent.owner == key.owner() && intent.alias == key.alias(id)
            }
        }
}

async fn owner_settlement_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    provider: &AgentProvider,
    worker: &str,
    execution_id: &ExecutionId,
) -> Result<Option<Settlement>> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT alias,intent_json,settlement_json FROM workspace_executions
         WHERE execution_id=?1 AND settlement_json IS NOT NULL",
    )
    .bind(execution_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some((alias, intent, settlement)) = row else {
        return Ok(None);
    };
    let intent: ExecutionMetadata = serde_json::from_str(&intent)?;
    if intent.alias.0 != alias || !resident_owner_matches(&intent, provider, execution_id) {
        return Ok(None);
    }
    if let ExecutionOwnership::Track(track) = &intent.scope {
        let Some(session) = crate::db::sqlite::session_get_tx(
            tx,
            &calm_types::worker::WorkerSessionId::from(worker),
        )
        .await?
        else {
            return Ok(None);
        };
        if session.track_id.as_str() != track.0
            || session.provider != calm_types::worker::WorkerProviderKind::from(provider)
        {
            return Ok(None);
        }
        let Some(card) = session.card_id.as_ref() else {
            return Ok(None);
        };
        let track = crate::db::sqlite::track_get_tx(tx, &session.track_id).await?;
        let cwd =
            crate::harness::wiring::session_cwd_tx(tx, card.as_str(), &session, &track).await?;
        if std::fs::canonicalize(cwd)? != intent.source.root {
            return Ok(None);
        }
    }
    Ok(Some(serde_json::from_str(&settlement)?))
}

pub(super) async fn has_drained_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    provider: &AgentProvider,
    worker: &str,
    execution_id: &ExecutionId,
) -> Result<bool> {
    Ok(owner_settlement_tx(tx, provider, worker, execution_id)
        .await?
        .is_some_and(|settlement| settlement.descendants_complete()))
}

pub(super) async fn restart_owner_exited_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    provider: &AgentProvider,
    worker: &str,
    execution_id: &ExecutionId,
) -> Result<bool> {
    Ok(owner_settlement_tx(tx, provider, worker, execution_id)
        .await?
        .is_some_and(|settlement| {
            settlement.descendants_complete()
                || (accepts_interrupted_restart(provider)
                    && matches!(
                        settlement,
                        Settlement::Journal(ExecutionState::InterruptedQuiescent { .. })
                    ))
        }))
}

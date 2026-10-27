//! New-worker selection only. Existing sessions always use the sticky route.
use super::{CodexBackends, CodexRouting};
use crate::agent_backend_pool::{policy, signals::*};
use crate::error::{CalmError, Result};
use crate::shared_codex_appserver::SharedCodexAppServer;
use calm_truth::db::sqlite::session_backend;
use sqlx::SqliteConnection;
use std::{path::Path, sync::Arc, time::SystemTime};

pub(crate) enum WorkerSelection<'a> {
    /// Legacy Single has no binding query or write.
    Single,
    Bound(&'a Arc<SharedCodexAppServer>),
}

impl WorkerSelection<'_> {
    pub(crate) fn home<'a>(&'a self, legacy: &'a Path) -> &'a Path {
        match self {
            Self::Single => legacy,
            Self::Bound(instance) => instance.shared_home().path(),
        }
    }

    pub(crate) async fn persist_tx(&self, tx: &mut SqliteConnection, session: &str) -> Result<()> {
        if let Self::Bound(instance) = self {
            session_backend::insert_tx(tx, session, "codex", instance.backend_id().as_str())
                .await?;
        }
        Ok(())
    }
}

impl CodexBackends {
    pub(crate) async fn select_new_worker_tx(
        &self,
        tx: &mut SqliteConnection,
    ) -> Result<WorkerSelection<'_>> {
        let CodexRouting::Pooled { isolation, .. } = &self.routing else {
            return Ok(WorkerSelection::Single);
        };
        let counts = session_backend::live_bound_counts_tx(tx, "codex").await?;
        let now = SystemTime::now();
        let observations = self
            .instances()
            .iter()
            .map(|instance| {
                let health = if !instance.is_running() {
                    BackendHealth::Unavailable(UnavailableReason::NotRunning)
                } else if instance.authentication_hold().is_some() {
                    BackendHealth::Unavailable(UnavailableReason::AuthenticationHeld)
                } else {
                    BackendHealth::Ready
                };
                let count = counts
                    .iter()
                    .find(|(id, _)| id == instance.backend_id().as_str())
                    .map_or(0, |(_, count)| *count);
                Ok(BackendObservation {
                    backend: crate::agent_backend_pool::BackendRef {
                        provider: calm_types::runtime::AgentProvider::Codex,
                        id: instance.backend_id().clone(),
                    },
                    connection_generation: 0,
                    observed_at: now,
                    signals: BackendSignals {
                        health: Observed::Known(health),
                        inflight: Observed::Known(u32::try_from(count).map_err(|_| {
                            CalmError::Internal("Codex inflight count out of range".into())
                        })?),
                        // MVP has no cooldown/quota feeder; do not infer either from RPC errors.
                        cooldown: Observed::Known(None),
                        quota: Observed::Known(QuotaStatus::Available),
                    },
                })
            })
            .collect::<Result<Vec<_>>>()?;
        match policy::select(
            isolation.profiles(),
            &observations,
            isolation.selection_policy(),
            now,
        ) {
            policy::Decision::Candidate(backend) => Ok(WorkerSelection::Bound(
                self.instance(&backend.id)
                    .ok_or_else(|| CalmError::Internal("selected Codex backend missing".into()))?,
            )),
            // Same error class as SharedCodexAppServer::not_running_error: the
            // operation driver's existing internal-error recovery owns the failure.
            policy::Decision::NoEligibleBackend(rejections) => Err(CalmError::Internal(format!(
                "no eligible Codex backend: {rejections:?}"
            ))),
        }
    }
}

//! Session routing and broadcast. Each instance retains its original supervisor.
use std::sync::Arc;

use super::{BackendId, identity::BackendIsolation};
use crate::{
    config::Config, db::Repo, pending_codex_threads::PendingThreadStartRegistry,
    shared_codex_appserver::SharedCodexAppServer, thread_seals::ThreadSeals,
};

/// Routing mode is fixed at construction; only a pool needs persistent scope.
#[derive(Clone)]
enum CodexRouting {
    Single,
    Pooled {
        db: calm_truth::db::sqlite::SqliteDb,
        isolation: Arc<BackendIsolation>,
    },
}

pub struct CodexBackends {
    instances: Vec<Arc<SharedCodexAppServer>>,
    routing: CodexRouting,
}

impl CodexBackends {
    pub fn build(
        cfg: &Config,
        repo: Arc<dyn Repo>,
        pending: Arc<PendingThreadStartRegistry>,
        isolation: Arc<BackendIsolation>,
    ) -> Arc<Self> {
        let routing = if isolation.is_multi_backend() {
            CodexRouting::Pooled {
                db: repo
                    .sqlite_db()
                    .expect("multi-backend isolation preparation requires SQLite"),
                isolation: isolation.clone(),
            }
        } else {
            CodexRouting::Single
        };
        let seals = Arc::new(ThreadSeals::new());
        let instances = isolation
            .profiles()
            .iter()
            .map(|profile| {
                SharedCodexAppServer::new_for_backend(
                    cfg,
                    repo.clone(),
                    profile.id.is_default().then(|| pending.clone()),
                    isolation.clone(),
                    profile.id.clone(),
                    seals.clone(),
                )
            })
            .collect();
        Arc::new(Self { instances, routing })
    }

    pub fn single(instance: Arc<SharedCodexAppServer>) -> Arc<Self> {
        Arc::new(Self {
            instances: vec![instance],
            routing: CodexRouting::Single,
        })
    }

    pub(crate) fn is_pooled(&self) -> bool {
        matches!(self.routing, CodexRouting::Pooled { .. })
    }

    pub fn default_only(&self) -> &Arc<SharedCodexAppServer> {
        self.instances
            .iter()
            .find(|instance| instance.backend_id().is_default())
            .expect("configuration requires default backend")
    }

    pub fn instances(&self) -> &[Arc<SharedCodexAppServer>] {
        &self.instances
    }

    pub fn additional(&self) -> impl Iterator<Item = &Arc<SharedCodexAppServer>> {
        self.instances
            .iter()
            .filter(|instance| !instance.backend_id().is_default())
    }

    pub fn additional_product_liveness_enabled(&self) -> bool {
        match &self.routing {
            CodexRouting::Single => false,
            CodexRouting::Pooled { isolation, .. } => isolation.is_multi_backend(),
        }
    }

    pub fn instance(&self, id: &BackendId) -> Option<&Arc<SharedCodexAppServer>> {
        self.instances
            .iter()
            .find(|instance| instance.backend_id() == id)
    }

    pub async fn for_session(&self, id: &str) -> crate::error::Result<&Arc<SharedCodexAppServer>> {
        self.resolve(calm_truth::db::sqlite::session_backend::CodexScopeKey::Session(id))
            .await
    }
    pub async fn for_thread(&self, id: &str) -> crate::error::Result<&Arc<SharedCodexAppServer>> {
        self.resolve(calm_truth::db::sqlite::session_backend::CodexScopeKey::Thread(id))
            .await
    }
    async fn resolve(
        &self,
        key: calm_truth::db::sqlite::session_backend::CodexScopeKey<'_>,
    ) -> crate::error::Result<&Arc<SharedCodexAppServer>> {
        use crate::error::CalmError;
        use calm_truth::db::sqlite::session_backend::{CodexScope, codex_backend_scope};
        let db = match &self.routing {
            CodexRouting::Single => return Ok(self.default_only()),
            CodexRouting::Pooled { db, .. } => db,
        };
        let CodexScope::Bound(id) = codex_backend_scope(db, key).await? else {
            return Err(CalmError::NotFound("Codex backend scope".into()));
        };
        let backend = BackendId::try_from(id.clone()).map_err(|error| {
            CalmError::Internal(format!("invalid persisted Codex backend: {error}"))
        })?;
        self.instance(&backend)
            .ok_or_else(|| CalmError::NotFound(format!("Codex backend {id}")))
    }

    pub fn boot_additional(&self) {
        for instance in self.additional() {
            let instance = instance.clone();
            tokio::spawn(async move {
                if let Err(error) = instance.boot().await {
                    tracing::warn!(backend = instance.backend_id().as_str(), %error, "additional Codex daemon boot failed");
                }
            });
        }
    }

    pub fn mark_needs_respawn(&self) {
        for instance in &self.instances {
            instance.mark_needs_respawn();
        }
    }

    pub async fn retry_hibernating_releases(&self) {
        for instance in &self.instances {
            instance.retry_hibernating_releases().await;
        }
    }

    pub async fn release_ended_threads(&self) -> crate::error::Result<usize> {
        let mut count = 0;
        let mut error = None;
        for instance in &self.instances {
            match instance.release_ended_threads().await {
                Ok(released) => count += released,
                Err(err) => {
                    error.get_or_insert(err);
                }
            }
        }
        match error {
            Some(err) => Err(err),
            None => Ok(count),
        }
    }

    pub async fn forget_threads_for_deleted_cards(
        &self,
        cards: &std::collections::HashSet<String>,
    ) {
        for instance in &self.instances {
            instance.forget_threads_for_deleted_cards(cards).await;
        }
    }

    pub fn forget_turn_state_for_deleted_threads(&self, threads: &[String]) {
        for instance in &self.instances {
            instance.forget_turn_state_for_deleted_threads(threads);
        }
    }
}

#[path = "codex_selection.rs"]
mod selection;

#[cfg(test)]
pub(crate) mod codex_route_tests;

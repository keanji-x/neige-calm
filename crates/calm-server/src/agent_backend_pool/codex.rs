//! Composition and broadcast only. Each instance retains its original supervisor.
use std::sync::Arc;

use super::{BackendId, identity::BackendIsolation};
use crate::{
    config::Config, db::Repo, pending_codex_threads::PendingThreadStartRegistry,
    shared_codex_appserver::SharedCodexAppServer, thread_seals::ThreadSeals,
};

/// Temporary rollout policy lives here, rather than in generic providers or callers.
#[derive(Clone, Copy)]
pub enum CodexRollout {
    InfrastructureOnly,
}

pub struct CodexBackends {
    instances: Vec<Arc<SharedCodexAppServer>>,
    rollout: CodexRollout,
}

impl CodexBackends {
    pub fn build(
        cfg: &Config,
        repo: Arc<dyn Repo>,
        pending: Arc<PendingThreadStartRegistry>,
        isolation: Arc<BackendIsolation>,
    ) -> Arc<Self> {
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
        Arc::new(Self {
            instances,
            rollout: CodexRollout::InfrastructureOnly,
        })
    }

    pub fn single(instance: Arc<SharedCodexAppServer>) -> Arc<Self> {
        Arc::new(Self {
            instances: vec![instance],
            rollout: CodexRollout::InfrastructureOnly,
        })
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

    /// PR1 has no additional product bindings. PR2 must add ownership filtering
    /// before enabling these subscriptions; a native thread ID alone is not an owner.
    pub fn additional_product_liveness_enabled(&self) -> bool {
        match self.rollout {
            CodexRollout::InfrastructureOnly => false,
        }
    }

    pub fn instance(&self, id: &BackendId) -> Option<&Arc<SharedCodexAppServer>> {
        self.instances
            .iter()
            .find(|instance| instance.backend_id() == id)
    }

    /// PR2 will resolve a session-owned persisted binding here. PR1 never routes.
    pub fn for_session(&self, _worker_session_id: &str) -> &Arc<SharedCodexAppServer> {
        match self.rollout {
            CodexRollout::InfrastructureOnly => self.default_only(),
        }
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

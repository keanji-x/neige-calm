//! Lifecycle of compiled plugin backends, sharing the host's admission and configuration gates.
use super::{
    AdmissionGuard, ConnectorClient, HostError, LifecycleGuard, Manifest, PluginHost,
    PluginRuntimeStatus, RunningPlugin, lifecycle,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

impl<E: crate::ports::ErrorFactory> PluginHost<E> {
    /// Reconcile only compiled declarations; preserve installed enable/configuration state.
    pub async fn reconcile_builtins(&self) -> Result<(), E> {
        if let Some(component) = crate::builtin::catalog().iter().find(|component| {
            !component.can_disable() && self.plugins_disabled.contains(&component.manifest().id)
        }) {
            return Err(E::bad_request(format!(
                "always-enabled component `{}` cannot be listed in plugins_disabled",
                component.manifest().id
            )));
        }
        for component in crate::builtin::catalog() {
            let manifest = component.manifest();
            let _guard = self
                .try_lock_lifecycle(&manifest.id)
                .map_err(lifecycle::spawn_error_to_calm::<E>)?;
            let prior = self.repo.plugin_get_by_id(&manifest.id).await?;
            self.repo.plugin_token_delete(&manifest.id).await?;
            self.repo
                .plugin_install(super::ports::NewPlugin {
                    id: manifest.id.clone(),
                    version: manifest.version.clone(),
                    install_path: format!("builtin:{}", manifest.id),
                    manifest: manifest.to_json(),
                    enabled: !component.can_disable() || prior.as_ref().is_some_and(|p| p.enabled),
                    user_config: serde_json::json!({}),
                })
                .await?;
        }
        Ok(())
    }

    pub(super) async fn spawn_builtin(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard<E>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let component = self
            .backends
            .get(id)
            .ok_or_else(|| HostError::BadState("unknown compiled component".into()))?;
        if manifest.to_json() != component.manifest().to_json() {
            return Err(HostError::BadState("compiled manifest was replaced".into()));
        }
        let (_, guard) = self
            .config_for_spawn_or_unavailable(lifecycle, manifest, guard)
            .await?;
        {
            let mut table = self.lock_table();
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: None,
                    mcp: Some(ConnectorClient::Builtin(component)),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window: 0,
                    window_started: Instant::now(),
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
        }
        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
        Ok(())
    }
}

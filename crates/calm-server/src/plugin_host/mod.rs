//! Kernel composition and port adapters for the owning plugin host.
pub use plugin::host::{connector, events, managed, registry, resources};
pub use plugin::{
    auth, cli_query, config, error, forge_caller, http_headers, http_mcp, manifest, mcp, perms,
    process,
};
mod adapter;
pub mod callbacks;
pub mod child_process;
pub mod lifecycle;
pub mod mcp_setup;
pub mod template_input;
pub mod version;
use crate::{db::RouteRepo, error::CalmError, event::EventBus, model::Plugin, state::WriteContext};
pub use calm_types::boot_budget::*;
pub use plugin::auth::{PluginToken, hash_token, verify_token};
pub use plugin::cli_query::{CLI_QUERY_BRINGUP_BUDGET, CliQueryRuntime};
pub use plugin::config::{effective_config, missing_required};
pub use plugin::connector::{SecretsError, read_secrets};
pub use plugin::error::{HostError, McpError, ProcessError};
pub use plugin::host::connector::ConnectorClient;
pub use plugin::host::managed::ConnectorInstall;
pub use plugin::host::registry::{PluginRegistry, PluginRegistryBuilder};
pub use plugin::host::resources::{ResourceError, read_ui_resource};
pub use plugin::host::{
    BackoffConfig, ConnectorSpawnOrder, LifecycleGuard, PluginHostStatus, PluginRuntimeStatus,
    PluginSocket, SocketUnavailable, connector_bringup_budget,
};
pub use plugin::http_mcp::{HttpCredential, HttpMcpClient};
pub use plugin::manifest::{CONFIG_SCHEMA_KEY, ConnectorKind, Manifest};
pub use plugin::mcp::{
    CallToolResult, ContentBlock, InboundNotification, InboundRequest, InitializeMeta, McpClient,
    RequestId, ResourceContent, ResourceContents, RpcError,
};
pub use plugin::process::PluginProcess;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};
pub use version::{KERNEL_VERSION, KernelTooOld, check_min_kernel_version};
#[async_trait::async_trait]
pub trait PluginListDb: Send + Sync {
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>, CalmError>;
}
struct ListBridge(Arc<dyn PluginListDb>);
#[async_trait::async_trait]
impl plugin::host::PluginListDb<CalmError> for ListBridge {
    async fn plugins_list_all(&self) -> Result<Vec<plugin::host::ports::PluginRecord>, CalmError> {
        self.0
            .plugins_list_all()
            .await
            .map(|rows| rows.into_iter().map(adapter::record).collect())
    }
}
pub struct PluginHost {
    inner: Arc<plugin::host::PluginHost<CalmError>>,
    write: WriteContext,
}
impl std::ops::Deref for PluginHost {
    type Target = plugin::host::PluginHost<CalmError>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl std::ops::DerefMut for PluginHost {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::get_mut(&mut self.inner).expect("host configuration must precede sharing")
    }
}
impl PluginHost {
    pub fn new_full(
        registry: Arc<PluginRegistry>,
        repo: Arc<dyn RouteRepo>,
        plugins_dir: PathBuf,
        plugins_data_dir: PathBuf,
        plugins_disabled: Vec<String>,
        events: EventBus,
        write: WriteContext,
    ) -> Self {
        let adapter = Arc::new(adapter::Adapter {
            repo,
            events,
            write: write.clone(),
        });
        let inner = plugin::host::PluginHost::new_full(plugin::host::HostInputs {
            registry,
            store: adapter.clone(),
            plugins_dir: plugins_dir.clone(),
            plugins_data_dir: plugins_data_dir.clone(),
            plugins_disabled,
            callbacks: adapter.clone(),
            state_sink: adapter,
            backends: Arc::new(adapter::Builtins),
            kernel_version: KERNEL_VERSION.clone(),
        });
        Self {
            inner: Arc::new(inner),
            write,
        }
    }
    pub fn write(&self) -> &WriteContext {
        &self.write
    }
    pub fn with_lifecycle_db(mut self, db: Arc<dyn lifecycle::LifecycleDb>) -> Self {
        let inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("host configuration must precede sharing"));
        self.inner = Arc::new(inner.with_lifecycle_db(Arc::new(lifecycle::Bridge(db))));
        self
    }
    pub fn with_plugin_list_db(mut self, db: Arc<dyn PluginListDb>) -> Self {
        let inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("host configuration must precede sharing"));
        self.inner = Arc::new(inner.with_plugin_list_db(Arc::new(ListBridge(db))));
        self
    }
    pub fn with_plugin_list_wall(mut self, wall: Duration) -> Self {
        let inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("host configuration must precede sharing"));
        self.inner = Arc::new(inner.with_plugin_list_wall(wall));
        self
    }
    pub fn with_backoff_schedule(
        mut self,
        schedule_ms: Vec<u64>,
        crash_window: Duration,
        crash_window_limit: u32,
    ) -> Self {
        let inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("host configuration must precede sharing"));
        self.inner =
            Arc::new(inner.with_backoff_schedule(schedule_ms, crash_window, crash_window_limit));
        self
    }
    pub fn with_app_autospawn_wall(mut self, wall: Duration) -> Self {
        let inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("host configuration must precede sharing"));
        self.inner = Arc::new(inner.with_app_autospawn_wall(wall));
        self
    }
    pub fn registry(&self) -> &Arc<PluginRegistry> {
        self.inner.registry()
    }
    pub fn try_lock_lifecycle(&self, id: &str) -> Result<LifecycleGuard, HostError> {
        self.inner.try_lock_lifecycle(id)
    }
    pub fn registry_insert(
        &self,
        guard: &LifecycleGuard,
        manifest: manifest::Manifest,
        install_path: Option<PathBuf>,
    ) {
        self.inner.registry_insert(guard, manifest, install_path)
    }
    pub fn registry_remove(&self, guard: &LifecycleGuard) -> Option<manifest::Manifest> {
        self.inner.registry_remove(guard)
    }
    pub async fn ensure_plugin_token(&self, guard: &LifecycleGuard) -> Result<String, HostError> {
        self.inner.ensure_plugin_token(guard).await
    }
    pub async fn rotate_plugin_token(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        self.inner.rotate_plugin_token(id).await
    }
    pub async fn autospawn_enabled(self: &Arc<Self>) {
        self.inner.autospawn_enabled().await
    }
    pub async fn autospawn_enabled_within(self: &Arc<Self>, connector_budget: Duration) {
        self.inner.autospawn_enabled_within(connector_budget).await
    }
    pub async fn spawn(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        self.inner.spawn(id).await
    }
    pub fn assert_config_gate_ran(&self, id: &str, kind: ConnectorKind) {
        self.inner.assert_config_gate_ran(id, kind)
    }
    pub fn config_gate_ran(&self, id: &str) -> bool {
        self.inner.config_gate_ran(id)
    }
    pub fn config_gate_breaches(&self, id: &str) -> u64 {
        self.inner.config_gate_breaches(id)
    }
    pub fn connector_spawn_order(&self, id: &str) -> Option<ConnectorSpawnOrder> {
        self.inner.connector_spawn_order(id)
    }
    pub async fn reaffirm_running(self: &Arc<Self>, id: &str) -> bool {
        self.inner.reaffirm_running(id).await
    }
    pub async fn stop(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        self.inner.stop(id).await
    }
    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        self.inner.restart(id).await
    }
    pub async fn status(&self, id: &str) -> Option<PluginHostStatus> {
        self.inner.status(id).await
    }
    pub async fn list_running(&self) -> Vec<PluginHostStatus> {
        self.inner.list_running().await
    }
    pub async fn running_plugin_ids(&self) -> BTreeSet<String> {
        self.inner.running_plugin_ids().await
    }
    pub async fn stderr_tail(&self, id: &str, n: usize) -> Option<Vec<String>> {
        self.inner.stderr_tail(id, n).await
    }
    pub async fn mcp_client(&self, id: &str) -> Option<Arc<McpClient>> {
        self.inner.mcp_client(id).await
    }
    pub async fn connector_client(&self, id: &str) -> Option<ConnectorClient> {
        self.inner.connector_client(id).await
    }
    pub async fn dispatch_neige_callback(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
        call_id: Option<&str>,
    ) -> Result<serde_json::Value, RpcError> {
        self.inner
            .dispatch_neige_callback(plugin_id, method, params, call_id)
            .await
    }
    pub async fn enable(self: &Arc<Self>, id: &str) -> Result<Plugin, CalmError> {
        self.inner.enable(id).await.map(adapter::row)
    }
    pub async fn disable(self: &Arc<Self>, id: &str) -> Result<Plugin, CalmError> {
        self.inner.disable(id).await.map(adapter::row)
    }
    pub async fn reload(self: &Arc<Self>, id: &str) -> Result<Plugin, CalmError> {
        self.inner.reload(id).await.map(adapter::row)
    }
    pub async fn install(
        &self,
        manifest: Manifest,
        path: &std::path::Path,
    ) -> Result<Plugin, CalmError> {
        self.inner.install(manifest, path).await.map(adapter::row)
    }
    pub async fn install_managed_connector(
        &self,
        connector: &ConnectorInstall,
    ) -> Result<Plugin, CalmError> {
        self.inner
            .install_managed_connector(connector)
            .await
            .map(adapter::row)
    }
    pub async fn uninstall(self: &Arc<Self>, id: &str) -> Result<(), CalmError> {
        self.inner.uninstall(id).await
    }
    pub async fn reconcile_builtins(&self) -> Result<(), CalmError> {
        self.inner.reconcile_builtins().await
    }
}
#[cfg(test)]
mod manifest_tests;

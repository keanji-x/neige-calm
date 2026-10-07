//! Kernel adapters for plugin host ports. All database and event writes remain here.
use super::*;
use crate::{
    db::RouteRepo,
    error::{CalmError, Result},
    event::{Event, EventBus, EventScope},
    ids::ActorId,
    model::{NewPlugin, Plugin},
    state::WriteContext,
};
use async_trait::async_trait;
use plugin::host::ports::{
    Backends, BuiltinBackend, CallbackInvocation, Callbacks, NewPlugin as Install, PluginRecord,
    StateSink, Store,
};
use std::sync::Arc;
pub struct Adapter {
    pub repo: Arc<dyn RouteRepo>,
    pub events: EventBus,
    pub write: WriteContext,
}
pub(super) fn record(row: Plugin) -> PluginRecord {
    PluginRecord {
        id: row.id,
        version: row.version,
        install_path: row.install_path,
        manifest: row.manifest,
        enabled: row.enabled,
        user_config: row.user_config,
        installed_at: row.installed_at,
        updated_at: row.updated_at,
    }
}
pub fn row(record: PluginRecord) -> Plugin {
    Plugin {
        id: record.id,
        version: record.version,
        install_path: record.install_path,
        manifest: record.manifest,
        enabled: record.enabled,
        user_config: record.user_config,
        installed_at: record.installed_at,
        updated_at: record.updated_at,
    }
}
#[async_trait]
impl Store<CalmError> for Adapter {
    async fn plugins_list_all(&self) -> Result<Vec<PluginRecord>> {
        Ok(self
            .repo
            .plugins_list_all()
            .await?
            .into_iter()
            .map(record)
            .collect())
    }
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<PluginRecord>> {
        Ok(self.repo.plugin_get_by_id(id).await?.map(record))
    }
    async fn plugin_install(&self, p: Install) -> Result<PluginRecord> {
        Ok(record(
            self.repo
                .plugin_install(NewPlugin {
                    id: p.id,
                    version: p.version,
                    install_path: p.install_path,
                    manifest: p.manifest,
                    enabled: p.enabled,
                    user_config: p.user_config,
                })
                .await?,
        ))
    }
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<PluginRecord> {
        Ok(record(self.repo.plugin_update_enabled(id, enabled).await?))
    }
    async fn plugin_update_manifest(
        &self,
        id: &str,
        manifest: serde_json::Value,
    ) -> Result<PluginRecord> {
        Ok(record(
            self.repo.plugin_update_manifest(id, manifest).await?,
        ))
    }
    async fn plugin_delete(&self, id: &str) -> Result<()> {
        Ok(self.repo.plugin_delete(id).await?)
    }
    async fn plugin_token_delete(&self, id: &str) -> Result<()> {
        Ok(self.repo.plugin_token_delete(id).await?)
    }
    async fn plugin_kv_clear(&self, id: &str) -> Result<()> {
        Ok(self.repo.plugin_kv_clear(id).await?)
    }
    async fn overlays_clear_by_plugin(&self, id: &str) -> Result<()> {
        Ok(self.repo.overlays_clear_by_plugin(id).await?)
    }
    async fn plugin_token_set(&self, id: &str, hash: &str, expires: i64) -> Result<()> {
        Ok(self.repo.plugin_token_set(id, hash, expires).await?)
    }
}
#[async_trait]
impl Callbacks for Adapter {
    async fn dispatch(
        &self,
        invocation: CallbackInvocation,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let ctx = callbacks::CallbackCtx {
            plugin_id: &invocation.plugin_id,
            repo: self.repo.clone(),
            event_bus: Arc::new(self.events.clone()),
            registry: invocation.registry,
            mcp: invocation.mcp,
            subscriptions: invocation.subscriptions,
            call_id: invocation.call_id.as_deref(),
            write: self.write.clone(),
        };
        callbacks::dispatch(&ctx, method, params).await
    }
}
#[async_trait]
#[allow(deprecated)] // Existing typed event writer retains its legacy cache arguments.
impl StateSink for Adapter {
    async fn emit(&self, id: &str, status: &PluginRuntimeStatus) {
        let event = Event::PluginState {
            id: id.into(),
            state: status.wire_name().into(),
            last_error: status.last_error().map(str::to_owned),
        };
        if let Err(e) = self
            .repo
            .log_pure_event(
                ActorId::Plugin(id.into()),
                EventScope::System,
                None,
                &self.events,
                self.write.role_cache(),
                self.write.area_cache(),
                event,
            )
            .await
        {
            tracing::warn!(plugin_id=%id,error=%e,"plugin_state event log failed");
        }
    }
}
pub struct Builtins;
impl Backends for Builtins {
    fn get(&self, id: &str) -> Option<&'static dyn BuiltinBackend> {
        crate::builtin_plugins::get(id).map(|backend| backend as &dyn BuiltinBackend)
    }
    fn trusted_forge(&self, id: &str) -> bool {
        crate::forge_trust::trusted_forge_plugin(id)
    }
}
impl BuiltinBackend for crate::builtin_plugins::BuiltinPlugin {
    fn manifest(&self) -> &Manifest {
        self.manifest()
    }
    fn tools_call(&self, tool: &str, args: &serde_json::Value) -> Result<CallToolResult, RpcError> {
        self.tools_call(tool, args)
    }
    fn forge_tools_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        caller: &forge_caller::ForgeCallerScope,
    ) -> Result<CallToolResult, RpcError> {
        self.forge_tools_call(tool, args, caller)
    }
}

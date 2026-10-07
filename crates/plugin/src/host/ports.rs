//! Narrow host storage, event and callback ports supplied by the kernel.
use super::{PluginRuntimeStatus, registry::PluginRegistry};
use crate::{
    forge_caller::ForgeCallerScope,
    manifest::Manifest,
    mcp::{CallToolResult, McpClient, RpcError},
    ports::ErrorFactory,
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
#[derive(Clone, Debug)]
pub struct PluginRecord {
    pub id: String,
    pub version: String,
    pub install_path: String,
    pub manifest: Value,
    pub enabled: bool,
    pub user_config: Value,
    pub installed_at: i64,
    pub updated_at: i64,
}
pub struct NewPlugin {
    pub id: String,
    pub version: String,
    pub install_path: String,
    pub manifest: Value,
    pub enabled: bool,
    pub user_config: Value,
}
#[async_trait]
pub trait Store<E: ErrorFactory>: Send + Sync {
    async fn plugins_list_all(&self) -> Result<Vec<PluginRecord>, E>;
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<PluginRecord>, E>;
    async fn plugin_install(&self, input: NewPlugin) -> Result<PluginRecord, E>;
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<PluginRecord, E>;
    async fn plugin_update_manifest(&self, id: &str, manifest: Value) -> Result<PluginRecord, E>;
    async fn plugin_delete(&self, id: &str) -> Result<(), E>;
    async fn plugin_token_set(&self, id: &str, hash: &str, expires_at: i64) -> Result<(), E>;
    async fn plugin_token_delete(&self, id: &str) -> Result<(), E>;
    async fn plugin_kv_clear(&self, id: &str) -> Result<(), E>;
    async fn overlays_clear_by_plugin(&self, id: &str) -> Result<(), E>;
}
pub struct SubscriptionRecord {
    pub plugin_id: String,
    pub task: tokio::task::JoinHandle<()>,
}
pub struct CallbackInvocation {
    pub plugin_id: String,
    pub registry: Arc<PluginRegistry>,
    pub mcp: Arc<McpClient>,
    pub subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
    pub call_id: Option<String>,
}
#[async_trait]
pub trait Callbacks: Send + Sync {
    async fn dispatch(
        &self,
        invocation: CallbackInvocation,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError>;
}
#[async_trait]
pub trait StateSink: Send + Sync {
    async fn emit(&self, id: &str, state: &PluginRuntimeStatus);
}
pub trait BuiltinBackend: Send + Sync {
    fn manifest(&self) -> &Manifest;
    fn tools_call(&self, tool: &str, args: &Value) -> Result<CallToolResult, RpcError>;
    fn forge_tools_call(
        &self,
        tool: &str,
        args: &Value,
        caller: &ForgeCallerScope,
    ) -> Result<CallToolResult, RpcError>;
}
pub trait Backends: Send + Sync {
    fn get(&self, id: &str) -> Option<&'static dyn BuiltinBackend>;
    fn trusted_forge(&self, id: &str) -> bool;
}

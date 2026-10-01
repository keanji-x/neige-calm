//! Compiled plugin backends. Filesystem manifests cannot provide or replace their code.

pub mod calendar;
pub mod dev;

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{AppContext, ToolCallIdentity, ToolRegistry};
use crate::mcp_server::tool_visibility::plugin_scope_for_track;
use crate::plugin_host::forge_caller::ForgeCallerScope;
use crate::plugin_host::{CallToolResult, Manifest};
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock};

#[derive(Clone, Copy, PartialEq, Eq)]
enum LifecyclePolicy {
    Optional,
    Always,
}

pub struct BuiltinPlugin {
    lifecycle: LifecyclePolicy,
    manifest: Manifest,
    native: ToolRegistry,
    lower: fn(&str, &Value) -> Result<Value, String>,
    forge_lower: fn(&str, &Value, &ForgeCallerScope) -> Result<Value, String>,
    instructions: &'static str,
    router: fn() -> axum::Router<crate::state::AppState>,
}

impl BuiltinPlugin {
    fn new(
        manifest: &str,
        native: ToolRegistry,
        lower: fn(&str, &Value) -> Result<Value, String>,
        forge_lower: fn(&str, &Value, &ForgeCallerScope) -> Result<Value, String>,
        instructions: &'static str,
    ) -> Self {
        Self {
            lifecycle: LifecyclePolicy::Optional,
            manifest: Manifest::parse(manifest).expect("compiled manifest"),
            native,
            lower,
            forge_lower,
            instructions,
            router: axum::Router::new,
        }
    }
    pub(super) fn always_enabled(mut self) -> Self {
        self.lifecycle = LifecyclePolicy::Always;
        self
    }
    pub fn can_disable(&self) -> bool {
        self.lifecycle == LifecyclePolicy::Optional
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn instructions(&self) -> &'static str {
        self.instructions
    }
    pub fn tools_call(&self, tool: &str, args: &Value) -> Result<CallToolResult, RpcError> {
        Self::encode_result((self.lower)(tool, args))
    }
    pub fn forge_tools_call(
        &self,
        tool: &str,
        args: &Value,
        caller: &ForgeCallerScope,
    ) -> Result<CallToolResult, RpcError> {
        Self::encode_result((self.forge_lower)(tool, args, caller))
    }
    fn encode_result(lowered: Result<Value, String>) -> Result<CallToolResult, RpcError> {
        let result = match lowered {
            Ok(value) => json!({ "content": [], "isError": false, "structuredContent": value }),
            Err(error) => {
                json!({ "content": [{"type":"text", "text":error}], "isError":true, "structuredContent":{"error":error} })
            }
        };
        serde_json::from_value(result)
            .map_err(|e| RpcError::internal(format!("built-in tool result: {e}")))
    }
}

static CATALOG: LazyLock<Vec<BuiltinPlugin>> =
    LazyLock::new(|| vec![dev::component(), calendar::component()]);
pub fn catalog() -> &'static [BuiltinPlugin] {
    &CATALOG
}
pub fn get(id: &str) -> Option<&'static BuiltinPlugin> {
    catalog().iter().find(|p| p.manifest.id == id)
}
pub fn is_reserved(id: &str) -> bool {
    get(id).is_some()
}
pub fn owner(tool: &str) -> Option<&'static BuiltinPlugin> {
    catalog().iter().find(|p| p.native.lookup(tool).is_some())
}
pub(crate) async fn require_bound(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    id: &str,
    name: &str,
) -> Result<(), RpcError> {
    let unknown = || RpcError::method_not_found(&format!("tools/call: {name}"));
    let host = ctx.plugin_host.get().ok_or_else(unknown)?;
    if !host.running_plugin_ids().await.contains(id) {
        if let Some(error) = crate::mcp_server::tool_visibility::disabled_plugin_error(
            ctx,
            identity,
            get(id).expect("registered native owner").manifest(),
        )
        .await
        {
            return Err(error);
        }
        return Err(unknown());
    }
    if !plugin_scope_for_track(ctx, identity.track_id.as_deref())
        .await
        .allows_manifest(get(id).expect("registered native owner").manifest())
    {
        return Err(unknown());
    }
    Ok(())
}
pub fn register_native_tools(registry: &mut ToolRegistry) {
    for plugin in catalog() {
        for descriptor in plugin.native.descriptors() {
            let id = plugin.manifest.id.clone();
            let name = descriptor.name.clone();
            let handler = plugin.native.lookup(&name).expect("compiled handler");
            assert!(
                registry.lookup(&descriptor.name).is_none(),
                "a compiled tool must not shadow another handler"
            );
            registry.register(
                descriptor,
                Arc::new(move |ctx, identity, args| {
                    let id = id.clone();
                    let name = name.clone();
                    let handler = handler.clone();
                    Box::pin(async move {
                        require_bound(&ctx, &identity, &id, &name).await?;
                        handler(ctx, identity, args).await
                    })
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn required_owner(template_id: &str) -> Option<&'static str> {
    catalog()
        .iter()
        .find(|p| p.manifest.templates.iter().any(|t| t.id == template_id))
        .map(|p| p.manifest.id.as_str())
}

pub fn router() -> axum::Router<crate::state::AppState> {
    catalog()
        .iter()
        .fold(axum::Router::new(), |router, plugin| {
            router.merge((plugin.router)())
        })
}

//! Compiled plugin backends. Filesystem manifests cannot provide or replace their code.

pub mod calendar;
pub mod dev;

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{AppContext, ToolCallIdentity, ToolRegistry};
use crate::mcp_server::tool_visibility::plugin_scope_for_track;
use crate::plugin_host::forge_caller::ForgeCallerScope;
use crate::plugin_host::{CallToolResult, Manifest};
use crate::plugin_results::registry_name;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock};

pub struct BuiltinPlugin {
    definition: &'static plugin::builtin::Definition,
    /// The compiled tools under their minted names (`plugin_<id>_<tool>`).
    native: ToolRegistry,
    lower: fn(&str, &Value) -> Result<Value, String>,
    forge_lower: fn(&str, &Value, &ForgeCallerScope) -> Result<Value, String>,
    instructions: &'static str,
    router: fn() -> axum::Router<crate::state::AppState>,
    /// Optional background task started once at boot; it must check its own lifecycle.
    background: Option<fn(Arc<AppContext>)>,
}

impl BuiltinPlugin {
    /// `local` holds the compiled tools under their local names (`publish`).
    fn new(
        definition: &'static plugin::builtin::Definition,
        local: ToolRegistry,
        lower: fn(&str, &Value) -> Result<Value, String>,
        forge_lower: fn(&str, &Value, &ForgeCallerScope) -> Result<Value, String>,
        instructions: &'static str,
    ) -> Self {
        let manifest = definition.manifest();
        let native = minted(&manifest.id, &local);
        Self {
            definition,
            native,
            lower,
            forge_lower,
            instructions,
            router: axum::Router::new,
            background: None,
        }
    }
    pub fn can_disable(&self) -> bool {
        self.definition.can_disable()
    }
    pub fn manifest(&self) -> &Manifest {
        self.definition.manifest()
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

/// A tool that comes and goes with its plugin is a plugin tool, compiled or not (#2227, §6 of
/// `docs/conventions/agent-commands.md`): each local name is served as `registry_name(id, local)`.
fn minted(id: &str, local: &ToolRegistry) -> ToolRegistry {
    let mut native = ToolRegistry::new();
    for mut descriptor in local.descriptors() {
        let handler = local.unguarded(&descriptor.name).expect("compiled handler");
        descriptor.name = registry_name(id, &descriptor.name);
        native.register(descriptor, handler);
    }
    native
}

/// Every built-in manifest tool's minted name. Manifest tools route after the kernel registry, so
/// a compiled tool minting one of these would silently shadow it.
fn manifest_tool_names() -> BTreeSet<String> {
    catalog()
        .iter()
        .flat_map(|plugin| {
            let id = &plugin.manifest().id;
            plugin
                .manifest()
                .exposes_tools
                .iter()
                .map(move |tool| registry_name(id, &tool.name))
        })
        .collect()
}

static CATALOG: LazyLock<Vec<BuiltinPlugin>> = LazyLock::new(|| {
    plugin::builtin::catalog()
        .iter()
        .map(|definition| match definition.binding {
            plugin::builtin::Binding::Calendar => calendar::component(definition),
            plugin::builtin::Binding::Gitforge => dev::component(definition),
        })
        .collect()
});
pub fn catalog() -> &'static [BuiltinPlugin] {
    &CATALOG
}
pub fn get(id: &str) -> Option<&'static BuiltinPlugin> {
    catalog().iter().find(|p| p.manifest().id == id)
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
    let manifest_tools = manifest_tool_names();
    for plugin in catalog() {
        for descriptor in plugin.native.descriptors() {
            let id = plugin.manifest().id.clone();
            let name = descriptor.name.clone();
            let handler = plugin.native.unguarded(&name).expect("compiled handler");
            assert!(
                registry.lookup(&descriptor.name).is_none(),
                "a compiled tool must not shadow another handler"
            );
            assert!(
                !manifest_tools.contains(&name),
                "compiled tool `{name}` mints a built-in manifest tool's name"
            );
            let fence: crate::mcp_server::registry::ToolFence = Arc::new(move |ctx, identity| {
                let (id, name) = (id.clone(), name.clone());
                Box::pin(async move { require_bound(&ctx, &identity, &id, &name).await })
            });
            registry.register_fenced(descriptor, Some(fence), handler);
        }
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn required_owner(template_id: &str) -> Option<&'static str> {
    catalog()
        .iter()
        .find(|p| p.manifest().templates.iter().any(|t| t.id == template_id))
        .map(|p| p.manifest().id.as_str())
}

/// Start every compiled component's background task once at boot.
pub fn spawn_background(ctx: &Arc<AppContext>) {
    for spawn in catalog().iter().filter_map(|plugin| plugin.background) {
        spawn(ctx.clone());
    }
}

pub fn router() -> axum::Router<crate::state::AppState> {
    catalog()
        .iter()
        .fold(axum::Router::new(), |router, plugin| {
            router.merge((plugin.router)())
        })
}

pub(super) fn descriptor(
    declaration: plugin::builtin::tools::NativeToolSpec,
) -> crate::mcp_server::registry::ToolDescriptor {
    crate::mcp_server::registry::ToolDescriptor {
        name: declaration.name,
        description: declaration.description,
        input_schema: declaration.input_schema,
        annotations: declaration.annotations,
        roles: declaration.roles,
        listed_for: declaration.listed_for,
    }
}

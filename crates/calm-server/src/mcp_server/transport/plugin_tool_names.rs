//! Which plugin tools a Track can see, in any spelling the model may send.
use super::*;
use crate::codex_appserver::tool_names::model_tool_key;

/// The plugin ids `scope` allows, whatever their running state.
fn in_scope_plugin_ids(
    registry: &crate::plugin_host::PluginRegistry,
    scope: &TrackPluginScope,
) -> BTreeSet<String> {
    registry
        .list()
        .into_iter()
        .map(|m| m.id)
        .filter(|id| scope.allows(id))
        .collect()
}

/// Every tool of every plugin `scope` allows, whatever its running state or kind — never the whole registry.
fn visible_plugin_tools_from(
    registry: &crate::plugin_host::PluginRegistry,
    in_scope: &BTreeSet<String>,
    scope: &TrackPluginScope,
) -> BTreeSet<String> {
    plugin_tool_descriptors_from(
        registry.list(),
        in_scope,
        &super::ToolDiscoveryScope::Track(scope),
    )
    .into_iter()
    .map(|d| d.name)
    .collect()
}

/// A tool outside the Track's scope is unknown here, and no plugin host means no plugin tools.
pub(crate) async fn names_track_visible_plugin_tool(
    ctx: &Arc<AppContext>,
    track_id: Option<&str>,
    requested: &str,
) -> bool {
    let Some(host) = ctx.plugin_host.get().cloned() else {
        return false;
    };
    let scope = plugin_scope_for_track(ctx, track_id).await;
    let registry = host.registry();
    let visible =
        visible_plugin_tools_from(registry, &in_scope_plugin_ids(registry, &scope), &scope);
    let key = model_tool_key(requested);
    visible.iter().any(|name| model_tool_key(name) == key)
}

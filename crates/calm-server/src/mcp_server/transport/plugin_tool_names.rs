//! How a plugin tool name reads to the model, and which plugin tools a Track can see.
use super::*;

/// Codex's sanitizing of a tool name: every char outside `[A-Za-z0-9_]` becomes `_` (codex-mcp `sanitize_responses_api_tool_name`).
/// KNOWN GAP: a name over codex-rs's 128-char cap is truncated and hash-suffixed there and cannot be reduced.
pub(crate) fn codex_sanitized(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// codex-mcp `LEGACY_MCP_TOOL_NAME_PREFIX` and `MCP_TOOL_NAME_DELIMITER`: the model reads `mcp__<server>__<sanitized tool>`.
const CODEX_MCP_PREFIX: &str = "mcp__";
const CODEX_MCP_DELIMITER: &str = "__";

/// The one key every spelling of a registry tool reduces to. A registry name starts with `plugin.`, never `mcp__`, so stripping cannot mis-read one.
pub(crate) fn model_tool_key(name: &str) -> String {
    codex_sanitized(strip_codex_qualifier(name))
}

fn strip_codex_qualifier(name: &str) -> &str {
    match name
        .strip_prefix(CODEX_MCP_PREFIX)
        .and_then(|rest| rest.split_once(CODEX_MCP_DELIMITER))
    {
        Some((server, tool)) if !server.is_empty() => tool,
        _ => name,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const TRUSTED: &str = "plugin.dev.neige.git-forge_wf.tool";

    #[test]
    fn codex_sanitized_matches_the_responses_api_alphabet() {
        assert_eq!(
            codex_sanitized(TRUSTED),
            "plugin_dev_neige_git_forge_wf_tool"
        );
        assert_eq!(codex_sanitized("a_b9Z"), "a_b9Z");
        assert_eq!(codex_sanitized("é-x"), "__x");
    }

    #[test]
    fn model_tool_key_strips_only_a_delimited_non_empty_server_segment() {
        assert_eq!(model_tool_key("mcp__calm__plugin_a_b"), "plugin_a_b");
        assert_eq!(model_tool_key("mcp__calm__plugin.a-b_c"), "plugin_a_b_c");
        assert_eq!(model_tool_key("mcp__plugin_a_b"), "mcp__plugin_a_b");
        assert_eq!(model_tool_key("mcp____plugin_a_b"), "mcp____plugin_a_b");
        assert_eq!(model_tool_key(TRUSTED), codex_sanitized(TRUSTED));
    }
}

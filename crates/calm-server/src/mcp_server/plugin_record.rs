//! Shared successful-result/failure recording for manifest and compiled native plugin calls.
use super::registry::{AppContext, ToolCallIdentity};
use crate::{model::CardRole, plugin_host::CallToolResult};
use serde_json::Value;
pub(crate) fn record(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    plugin: &str,
    tool: &str,
    args: &Value,
    result: Option<&CallToolResult>,
) {
    if identity.role != CardRole::Planner {
        return;
    }
    let Some(track) = identity.track_id.as_deref() else {
        return;
    };
    match result {
        Some(result) => ctx.plugin_results.record(track, plugin, tool, args, result),
        None => ctx.plugin_results.record_failure(track, plugin, tool, args),
    }
}

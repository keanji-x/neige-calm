//! Shared catalog production; connection identity and plugin admission remain authoritative.
use super::*;

/// One discovery owner for MCP listing and authenticated CLI lookup.
/// Bootstrap remains limited to the daemon's initial MCP discovery; CLI accepts card-bound callers only.
pub(crate) async fn tool_descriptors_for_connection(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    connection_identity: &ConnectionIdentity,
    thread_id: Option<&str>,
) -> Result<Vec<ToolDescriptor>, RpcError> {
    let descriptors = match connection_identity {
        ConnectionIdentity::DaemonTrust => match thread_id {
            Some(tid) => match resolve_thread_identity(ctx, Some(tid), "tools/list")
                .await
                .ok()
            {
                Some(identity) => tool_descriptors_for_identity(ctx, registry, &identity).await,
                None => bootstrap_tool_descriptors(ctx, registry).await,
            },
            // Initial discovery may precede thread attribution. The catalog covers all
            // running plugins; tools/call still requires a live role and Track binding.
            None => bootstrap_tool_descriptors(ctx, registry).await,
        },
        ConnectionIdentity::CardBound(bound) => match thread_id {
            Some(tid) => match resolve_thread_identity(ctx, Some(tid), "tools/list")
                .await
                .ok()
            {
                Some(identity) if same_bound_session(&identity, bound) => {
                    tool_descriptors_for_identity(ctx, registry, &identity).await
                }
                Some(identity) => {
                    warn_cross_session_reject(tid, &identity, bound);
                    Vec::new()
                }
                _ => Vec::new(),
            },
            None => {
                let card = ensure_card_bound_session_active(ctx, bound, "tools/list").await?;
                let scope = plugin_scope_for_track(ctx, Some(card.track_id.as_str())).await;
                let mut descriptors = registry.descriptors_for_role(bound.role);
                extend_plugin_tool_descriptors_for_role(ctx, &mut descriptors, card.role, &scope)
                    .await;
                descriptors
            }
        },
    };
    Ok(descriptors)
}

/// What `tools/list` shows a resolved caller: its role's kernel tools plus the plugin tools of
/// its track's scope.
async fn tool_descriptors_for_identity(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: &ToolCallIdentity,
) -> Vec<ToolDescriptor> {
    let scope = plugin_scope_for_track(ctx, identity.track_id.as_deref()).await;
    let mut descriptors = registry.descriptors_for_role(identity.role);
    extend_plugin_tool_descriptors_for_role(ctx, &mut descriptors, identity.role, &scope).await;
    descriptors
}

/// The transport's `-32601` for a `tools/call` name this caller cannot reach. It lists the names
/// its `tools/list` shows, so the error is the same for an unknown name and an out-of-scope one:
/// it is not an existence oracle. A built-in plugin native refused by `require_bound` keeps its
/// bare `-32601` (#2003 KNOWN GAP K7). Hidden tools (callable, not in `tools/list`) are not listed.
pub(super) async fn unknown_tool_error(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: &ToolCallIdentity,
    name: &str,
) -> RpcError {
    let mut visible: Vec<String> = tool_descriptors_for_identity(ctx, registry, identity)
        .await
        .into_iter()
        .map(|descriptor| descriptor.name)
        .collect();
    visible.sort();
    RpcError::method_not_found(&format!(
        "tools/call: {name}; tools visible to this session: {}",
        visible.join(", ")
    ))
}

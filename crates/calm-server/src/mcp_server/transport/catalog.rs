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
                Some(identity) => {
                    // Plugin tools are scoped to the resolved thread's track.
                    let scope = plugin_scope_for_track(ctx, identity.track_id.as_deref()).await;
                    let mut descriptors = registry.descriptors_for_role(identity.role);
                    extend_plugin_tool_descriptors_for_role(
                        ctx,
                        &mut descriptors,
                        identity.role,
                        &scope,
                    )
                    .await;
                    descriptors
                }
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
                    let scope = plugin_scope_for_track(ctx, identity.track_id.as_deref()).await;
                    let mut descriptors = registry.descriptors_for_role(identity.role);
                    extend_plugin_tool_descriptors_for_role(
                        ctx,
                        &mut descriptors,
                        identity.role,
                        &scope,
                    )
                    .await;
                    descriptors
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

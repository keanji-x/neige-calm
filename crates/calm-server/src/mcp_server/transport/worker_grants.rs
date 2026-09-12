//! Frozen per-task plugin grants, intersected with the live platform admission.
use super::*;

/// None means a legacy Worker; an isolated task with no grants is Some(empty).
async fn isolated_grants(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<Option<Vec<String>>, RpcError> {
    if identity.role != CardRole::Worker {
        return Ok(None);
    }
    let track_id = identity
        .track_id
        .as_deref()
        .ok_or_else(|| RpcError::method_not_found("worker tool grants"))?;
    crate::isolated_codex::lookup::delegated_plugin_tools(
        ctx.repo.as_ref(),
        &identity.card_id,
        &identity.session_id,
        track_id,
    )
    .await
    .map_err(|e| RpcError::internal(format!("isolated tool grant binding: {e}")))
}

fn native_tool(name: &str) -> bool {
    crate::dedicated_codex::MCP_TOOL_ALLOWLIST.contains(&name)
}

/// Exact ordinary tools in the current Track. Never infer grants from annotations.
pub(crate) async fn eligible_plugin_tools(
    ctx: &Arc<AppContext>,
    track_id: Option<&str>,
) -> Result<BTreeSet<String>, RpcError> {
    let Some(host) = ctx.plugin_host.get().cloned() else {
        return Ok(BTreeSet::new());
    };
    let running = host.running_plugin_ids().await;
    let scope = plugin_scope_for_track(ctx, track_id).await;
    let mut names = BTreeSet::new();
    for descriptor in plugin_tool_descriptors_from(host.registry().list(), &running, &scope) {
        if let Some((_, _, None)) = plugin_tool_route(host.registry(), &descriptor.name, &running)?
        {
            names.insert(descriptor.name);
        }
    }
    Ok(names)
}

pub(super) async fn filter(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    descriptors: &mut Vec<ToolDescriptor>,
) -> Result<(), RpcError> {
    if let Some(grants) = isolated_grants(ctx, identity).await? {
        let eligible = eligible_plugin_tools(ctx, identity.track_id.as_deref()).await?;
        descriptors.retain(|d| {
            native_tool(&d.name) || (grants.contains(&d.name) && eligible.contains(&d.name))
        });
    }
    Ok(())
}

pub(super) async fn require(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    name: &str,
) -> Result<(), RpcError> {
    if let Some(grants) = isolated_grants(ctx, identity).await? {
        if native_tool(name) {
            return Ok(());
        }
        let eligible = eligible_plugin_tools(ctx, identity.track_id.as_deref()).await?;
        if !grants.iter().any(|g| g == name) || !eligible.contains(name) {
            return Err(RpcError::method_not_found(&format!("tools/call: {name}")));
        }
    }
    Ok(())
}

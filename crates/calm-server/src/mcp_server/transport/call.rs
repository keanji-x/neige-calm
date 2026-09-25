//! The one execution path for a registered kernel tool: `tools/call` and `neige/cli` (#1801) both
//! resolve the caller, enforce worker grants and run the handler through here.
use super::*;
use crate::mcp_server::result::ToolResult;

/// `thread_id` is `tools/call`'s `_meta.threadId`; `neige/cli` passes `None`, so a card-bound
/// connection acts as its bound card. A name the registry does not hold is a `-32601`.
pub(crate) async fn call_registered_tool(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    connection_identity: &ConnectionIdentity,
    thread_id: Option<&str>,
    name: &str,
    arguments: Value,
) -> Result<ToolResult, RpcError> {
    let handler = registry
        .lookup(name)
        .ok_or_else(|| RpcError::method_not_found(&format!("tools/call: {name}")))?;
    let identity = resolve_tools_call_identity(ctx, thread_id, name, connection_identity).await?;
    worker_grants::require(ctx, &identity, name).await?;
    handler(ctx.clone(), identity, arguments).await
}

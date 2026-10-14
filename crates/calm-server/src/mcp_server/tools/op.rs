//! `neige_op_show` (#2600): read one forge action's recorded outcome by op_id, from any card of
//! the caller's Track, in the envelope the original call and its replay return. Read-only: it
//! never drives, waits on, or re-emits anything for the operation.

use crate::mcp_server::forge_receipt;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolRegistry, read_only_annotations,
};
use crate::mcp_server::result::ToolResult;
use crate::model::CardRole;
use crate::operation::forge_action_adapter::FORGE_ACTION_KIND;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_OP_SHOW: &str = "neige_op_show";

pub fn register_into(registry: &mut ToolRegistry) {
    let handler: ToolHandler =
        Arc::new(|ctx, identity, args| Box::pin(async move { op_show(ctx, identity, args).await }));
    registry.register(op_show_descriptor(), handler);
}

fn op_show_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_OP_SHOW.into(),
        description: include_str!("../../../prompts/tools/neige_op_show.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["op_id"],
            "properties": {
                "op_id": { "type": "string", "minLength": 1 }
            }
        }),
        annotations: Some(read_only_annotations()),
        roles: &[CardRole::Planner, CardRole::Worker],
        listed_for: &[],
    }
}

async fn op_show(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<ToolResult, RpcError> {
    let op_id = args
        .get("op_id")
        .and_then(Value::as_str)
        .filter(|op_id| !op_id.is_empty())
        .ok_or_else(|| RpcError::invalid_params("op_id must be a non-empty string"))?;
    let track_id = identity
        .track_id
        .as_deref()
        .ok_or_else(|| RpcError::forbidden("forge action requires a track-scoped caller"))?;
    let Some(runtime) = ctx.operation_runtime.get() else {
        return Err(RpcError::internal("operation runtime not bound"));
    };
    // One refusal for an unknown id, another Track's operation and a non-forge operation alike.
    let op = runtime
        .operation_on_track(op_id, FORGE_ACTION_KIND, track_id)
        .await
        .map_err(|e| RpcError::internal(format!("operation lookup: {e}")))?
        .ok_or_else(|| RpcError::not_found(format!("no operation {op_id} on this track")))?;
    forge_receipt::show(&op).map_err(|e| RpcError::internal(format!("operation result: {e}")))
}

//! One named independent task through the Planner report writer.
use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::mcp_server::{framing::RpcError, registry::*};
use crate::model::CardRole;
use crate::track_report::dispatch::DispatchArgs;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TASK_DISPATCH: &str = "calm.task.dispatch";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ToolDescriptor {
        name: TOOL_TASK_DISPATCH.into(),
        description: "Declare one independent Codex task in an empty isolated workspace. name is a Track-local business identity for Dispatch-created tasks: only surrounding whitespace is trimmed; case and Unicode are exact. Same name and exact typed contract replays the original task key and block, even after report changes or session replacement; a different contract conflicts. Use a new meaningful name for new work, existing recovery for repair. goal and acceptance are required. Semantic acceptance is reviewed from the completion report, not a machine gate or file candidate qualification. Receipt creation does not mean running: current diagnostics preserve User release, lifecycle and budget controls. No dependencies or other options. Normal result receipts arrive through the existing Planner result path.".into(),
        input_schema: json!({
            "type":"object", "additionalProperties":false,
            "required":["name","goal","acceptance","executor","workspace"],
            "properties": {
                "name":{"type":"string","minLength":1,"description":"Nonempty after trim, at most 200 UTF-8 bytes, no control characters; immutable Track-local Dispatch identity."},
                "goal":{"type":"string","minLength":1},
                "acceptance":{"type":"string","minLength":1},
                "executor":{"type":"string","enum":["codex"]},
                "workspace":{"type":"string","enum":["empty"]}
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }, Arc::new(|ctx, identity, args| Box::pin(async move {
        dispatch(ctx, identity, args).await.map(crate::mcp_server::result::ToolResult::structured)
    })));
}

async fn dispatch(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let args: DispatchArgs =
        serde_json::from_value(args).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let args = args.normalize().map_err(map_error)?;
    let (track, _, card, payload) =
        super::track_report::resolve_report_for_caller(&ctx, &identity).await?;
    CardDecisionSink::from_app_context(&ctx)
        .commit_task_dispatch(
            &identity,
            track,
            card,
            payload,
            args,
            ctx.task_budget_default,
        )
        .await
        .map_err(map_error)
}

fn map_error(error: CalmError) -> RpcError {
    match error {
        CalmError::BadRequest(m) => RpcError::invalid_params(m),
        CalmError::Forbidden(m) => RpcError::custom(-32403, m),
        CalmError::Conflict(m) => RpcError::custom(-32409, m),
        other => RpcError::internal(other.to_string()),
    }
}

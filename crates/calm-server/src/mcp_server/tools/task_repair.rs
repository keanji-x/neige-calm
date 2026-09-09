//! Explicit Planner choice to repair one rejected candidate.
use crate::{
    decision_sink::CardDecisionSink,
    error::CalmError,
    file_delivery::repair::RepairArgs,
    mcp_server::{framing::RpcError, registry::*},
    model::CardRole,
};
use serde_json::json;
use std::sync::Arc;
pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ToolDescriptor {
        name:"calm.task.repair".into(),
        description:"Request one linked repair of an original empty-workspace candidate producer after its machine checks passed and its designated Reviewer reported blocking findings and successfully stopped. producer is the original task key; reason is the explicit Planner choice. C1 and R1 remain unchanged. Returns stable repair_key and review_key with exact original findings and current admission; same request replays, different reason conflicts. The new producer inherits original goal, acceptance, files and checks and gets C1 at /workspace/inputs/source. C2 requires fresh checks, its new complete re-review and explicit Planner acceptance. Consumers must explicitly name repair_key. No machine-failed or recursive repairs. User release, budget and lifecycle controls still apply.".into(),
        input_schema:json!({"type":"object","additionalProperties":false,"required":["producer","reason"],"properties":{"producer":{"type":"string","minLength":1},"reason":{"type":"string","minLength":1}}}),
        annotations:Some(role_gated_write_annotations()), visible_to_roles:&[CardRole::Planner],
    }, Arc::new(|ctx, identity, args|Box::pin(async move {
        require_role(&identity, CardRole::Planner)?;
        let args: RepairArgs = serde_json::from_value(args).map_err(|e|RpcError::invalid_params(e.to_string()))?;
        args.validate().map_err(map_error)?;
        let (track, _, card, payload) = super::track_report::resolve_report_for_caller(&ctx, &identity).await?;
        let result = CardDecisionSink::from_app_context(&ctx).commit_task_repair(&identity, track, card, payload, args, ctx.task_budget_default).await.map_err(map_error)?;
        Ok(crate::mcp_server::result::ToolResult::structured(result))
    })));
}
fn map_error(error: CalmError) -> RpcError {
    match error {
        CalmError::BadRequest(m) => RpcError::invalid_params(m),
        CalmError::Forbidden(m) => RpcError::custom(-32403, m),
        CalmError::Conflict(m) => RpcError::custom(-32409, m),
        other => RpcError::internal(other.to_string()),
    }
}

//! `calm.task.replace` (#1785 S2): the Planner's next round of an attached codex/claude task.
use crate::{
    decision_sink::CardDecisionSink,
    error::CalmError,
    mcp_server::{framing::RpcError, registry::*},
    model::CardRole,
    task_replace::ReplaceArgs,
};
use serde_json::json;
use std::sync::Arc;

pub const TOOL_TASK_REPLACE: &str = "calm.task.replace";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(
        ToolDescriptor {
            name: TOOL_TASK_REPLACE.into(),
            description: include_str!("../../../prompts/tools/calm.task.replace.md")
                .trim_end()
                .to_string(),
            input_schema: json!({"type": "object", "additionalProperties": false,
                "required": ["key", "expected_attempt_id", "idempotency_key", "reason", "goal", "acceptance"],
                "properties": {
                    "key": {"type": "string", "minLength": 1},
                    "expected_attempt_id": {"type": "string", "minLength": 1},
                    "idempotency_key": {"type": "string", "minLength": 1, "maxLength": 200},
                    "reason": {"type": "string", "minLength": 1, "maxLength": 4096},
                    "goal": {"type": "string", "minLength": 1},
                    "acceptance": {"type": "string", "minLength": 1},
                    "context": {"type": "object"},
                    "carry": {"type": "string", "enum": ["candidate", "none"]}
                }}),
            annotations: Some(role_gated_write_annotations()),
            visible_to_roles: &[CardRole::Planner],
        },
        Arc::new(|ctx, identity, args| {
            Box::pin(async move {
                require_role(&identity, CardRole::Planner)?;
                let args: ReplaceArgs = serde_json::from_value(args)
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                args.validate().map_err(map_error)?;
                let (track, _, card, payload) =
                    super::track_report::resolve_report_for_caller(&ctx, &identity).await?;
                let result = CardDecisionSink::from_app_context(&ctx)
                    .commit_task_replace(&identity, track, card, payload, args)
                    .await
                    .map_err(map_error)?;
                // A stop committed a cleanup marker: reap now rather than on the next sweep.
                if result["replayed"] == false
                    && result["predecessor"]["stop"] == "canceled_now"
                    && let Some(poke) = ctx.scheduler_poke.get()
                {
                    poke.poke_worker_cleanups();
                }
                Ok(crate::mcp_server::result::ToolResult::structured(result))
            })
        }),
    );
}

fn map_error(error: CalmError) -> RpcError {
    match error {
        CalmError::BadRequest(m) => RpcError::invalid_params(m),
        CalmError::Forbidden(m) => RpcError::custom(-32403, m),
        CalmError::Conflict(m) => RpcError::custom(-32409, m),
        other => RpcError::internal(other.to_string()),
    }
}

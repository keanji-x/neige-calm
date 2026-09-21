//! `calm.task.delivery` (Planner-only): retry or abandon a failed Git delivery
//! (#1727 S4 slice 3). The logic is `git_candidate::action::apply_delivery_action`; this file is
//! the descriptor, the argument parsing and the error mapping: a malformed argument (a missing or
//! empty required string, an unknown `action`, a present `reason` that is not a string) is
//! `-32602`; a state refusal (`CalmError::Conflict`, text starting with `refused:`, 5.1.11) is
//! `-32409` with the text verbatim — the repository's Conflict convention (`task_dispatch`,
//! `task_repair`, `emit`); `Forbidden` is `-32403`.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::CalmError;
use crate::git_candidate::action::{DeliveryAction, DeliveryActionArgs, apply_delivery_action};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::CardRole;

pub const TOOL_TASK_DELIVERY: &str = "calm.task.delivery";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(task_delivery_descriptor(), wrap(task_delivery));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move {
            result
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
        })
    })
}

fn task_delivery_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_DELIVERY.into(),
        description: include_str!("../../../prompts/tools/calm.task.delivery.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["key", "expected_attempt_id", "expected_delivery_id", "idempotency_key", "action"],
            "properties": {
                "key": { "type": "string" },
                "expected_attempt_id": { "type": "string" },
                "expected_delivery_id": { "type": "string" },
                "idempotency_key": { "type": "string" },
                "action": { "type": "string", "enum": ["retry", "abandon"] },
                "reason": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

fn required_string(args: &Value, name: &str) -> Result<String, RpcError> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            RpcError::invalid_params(format!("task_delivery: missing `{name}` (non-empty)"))
        })
}

async fn task_delivery(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let action = required_string(&args, "action")?;
    let action = DeliveryAction::parse(&action).ok_or_else(|| {
        RpcError::invalid_params(format!(
            "task_delivery: unknown action `{action}` (expected `retry` or `abandon`)"
        ))
    })?;
    let parsed = DeliveryActionArgs {
        key: required_string(&args, "key")?,
        expected_attempt_id: required_string(&args, "expected_attempt_id")?,
        expected_delivery_id: required_string(&args, "expected_delivery_id")?,
        idempotency_key: required_string(&args, "idempotency_key")?,
        action,
        reason: optional_string(&args, "reason")?,
    };
    match apply_delivery_action(&ctx, &identity, parsed).await {
        Ok(receipt) => serde_json::to_value(receipt)
            .map_err(|error| RpcError::internal(format!("task_delivery: {error}"))),
        Err(CalmError::Conflict(msg)) => Err(RpcError::custom(-32409, msg)),
        Err(CalmError::BadRequest(msg)) => {
            Err(RpcError::invalid_params(format!("task_delivery: {msg}")))
        }
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("task_delivery: forbidden: {msg}"),
        )),
        Err(error) => Err(RpcError::internal(format!("task_delivery: {error}"))),
    }
}

/// A present `name` must be a string (`null` counts as absent); any other JSON type is malformed.
fn optional_string(args: &Value, name: &str) -> Result<Option<String>, RpcError> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(RpcError::invalid_params(format!(
            "task_delivery: `{name}` must be a string, got {other}"
        ))),
    }
}

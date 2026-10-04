//! Worker outcome tools (`neige.task.report_success`, `neige.task.report_failure`). Every emitted event's scope is
//! anchored on the caller's card.

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::event::Event;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::CardRole;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TASK_REPORT_SUCCESS: &str = "neige.task.report_success";
pub const TOOL_TASK_REPORT_FAILURE: &str = "neige.task.report_failure";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(task_report_success_descriptor(), wrap(task_report_success));
    registry.register(task_report_failure_descriptor(), wrap(task_report_failure));
}

/// Turns a typed async fn into the boxed-future `ToolHandler` the registry expects.
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

fn task_report_success_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_REPORT_SUCCESS.into(),
        description: include_str!("../../../prompts/tools/neige.task.report_success.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["attempt_id"],
            "properties": {
                "attempt_id": { "type": "string", "minLength": 1 },
                "result": {},
                "artifacts": { "type": "array" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // Visible to workers so a codex worker's `tools/list` advertises the native completion tool.
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_report_success(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let attempt_id = required_attempt_id(&args, "task_report_success")?;
    let result = args.get("result").cloned().unwrap_or(Value::Null);
    let artifacts_val = args
        .get("artifacts")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    let artifacts: Vec<crate::event::ArtifactRef> =
        serde_json::from_value(artifacts_val).map_err(|e| {
            RpcError::invalid_params(format!("task_report_success: invalid artifacts: {e}"))
        })?;

    let event = Event::TaskCompleted {
        idempotency_key: attempt_id.clone(),
        result,
        artifacts,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    submit_reported_delivery(&ctx, &identity, &attempt_id).await;
    Ok(json!({ "status": "report_received" }))
}

/// The report transaction wrote the attempt's first delivery row (#1830 S2 D7, a kernel-delivery
/// lease only); submit it under its persisted key now. A failure is logged: the row is durable,
/// and the scheduler's pass (poked by `workspace.released`) and the reconcile sweep resubmit it.
async fn submit_reported_delivery(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    attempt_id: &str,
) {
    if let Err(error) =
        crate::git_candidate::delivery::submit_reported_delivery(ctx, attempt_id).await
    {
        tracing::warn!(
            card_id = %identity.card_id,
            track_id = identity.track_id.as_deref().unwrap_or("<missing>"),
            error = %error,
            "worker report persisted but its delivery submission failed; the scheduler resubmits"
        );
    }
}

fn task_report_failure_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_REPORT_FAILURE.into(),
        description: include_str!("../../../prompts/tools/neige.task.report_failure.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["attempt_id", "reason"],
            "properties": {
                "attempt_id": { "type": "string", "minLength": 1 },
                "reason": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // Visible to workers (see `task_report_success_descriptor`).
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_report_failure(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let attempt_id = required_attempt_id(&args, "task_report_failure")?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_report_failure: missing `reason` (non-empty)")
        })?
        .to_string();

    let event = Event::TaskFailed {
        idempotency_key: attempt_id.clone(),
        reason,
        details: None,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    submit_reported_delivery(&ctx, &identity, &attempt_id).await;
    Ok(json!({ "status": "report_received" }))
}

/// The task execution a report or verdict names; persisted as the event's `idempotency_key`.
pub(crate) fn required_attempt_id(args: &Value, tool: &str) -> Result<String, RpcError> {
    args.get("attempt_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            RpcError::invalid_params(format!("{tool}: missing `attempt_id` (non-empty)"))
        })
}

async fn commit_worker_task_report_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    event: Event,
) -> Result<(), RpcError> {
    let kind_tag = event.kind_tag();
    let result = CardDecisionSink::from_app_context(ctx)
        .commit_worker_task_report(identity, event)
        .await;

    match result {
        Ok(_) => Ok(()),
        Err(CalmError::Forbidden(msg)) => {
            // Role gate refusal — a custom error code so a mis-roled card sees a deterministic failure shape.
            Err(RpcError::custom(
                -32403,
                format!("emit {kind_tag}: forbidden: {msg}"),
            ))
        }
        Err(CalmError::Conflict(msg)) => Err(RpcError::custom(-32409, msg)),
        Err(CalmError::NotFound(msg)) => Err(RpcError::custom(-32404, msg)),
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

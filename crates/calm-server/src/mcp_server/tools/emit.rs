//! Worker outcome tools (`neige_task_done`, `neige_task_fail`). Every emitted event's scope is
//! anchored on the caller's card.

use crate::decision_sink::{CardDecisionSink, CommitMessage, DeliveryMessage, WorkerTaskReport};
use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::CardRole;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TASK_DONE: &str = "neige_task_done";
pub const TOOL_TASK_FAIL: &str = "neige_task_fail";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(task_done_descriptor(), wrap(task_done));
    registry.register(task_fail_descriptor(), wrap(task_fail));
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

fn task_done_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_DONE.into(),
        description: include_str!("../../../prompts/tools/neige_task_done.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["attempt_id"],
            "properties": {
                "attempt_id": { "type": "string", "minLength": 1 },
                "result": {},
                "artifacts": { "type": "array" },
                "commit_message": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // Visible to workers so a codex worker's `tools/list` advertises the native completion tool.
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_done(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let attempt_id = required_attempt_id(&args, TOOL_TASK_DONE)?;
    let commit_message = commit_message_arg(&args)?;
    let result = args.get("result").cloned().unwrap_or(Value::Null);
    let artifacts_val = args
        .get("artifacts")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    let artifacts: Vec<crate::event::ArtifactRef> =
        serde_json::from_value(artifacts_val).map_err(|e| {
            RpcError::invalid_params(format!("neige_task_done: invalid artifacts: {e}"))
        })?;

    let report = WorkerTaskReport::Completed {
        attempt_id: attempt_id.clone(),
        result,
        artifacts,
        commit_message,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, report).await?;
    submit_reported_delivery(&ctx, &identity, &attempt_id).await;
    Ok(json!({ "status": "report_received" }))
}

/// `commit_message` (#2139), parsed before the report transaction: an invalid one refuses the
/// whole report and writes nothing, so the still-running attempt can report again. Absent means
/// the kernel's own text.
fn commit_message_arg(args: &Value) -> Result<DeliveryMessage, RpcError> {
    match args.get("commit_message") {
        None => Ok(DeliveryMessage::Kernel),
        Some(Value::String(text)) => CommitMessage::parse(text)
            .map(DeliveryMessage::Worker)
            .map_err(|error| RpcError::invalid_params(format!("neige_task_done: {error}"))),
        Some(_) => Err(RpcError::invalid_params(
            "neige_task_done: commit_message must be a string",
        )),
    }
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

fn task_fail_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_FAIL.into(),
        description: include_str!("../../../prompts/tools/neige_task_fail.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["attempt_id", "reason"],
            "properties": {
                "attempt_id": { "type": "string", "minLength": 1 },
                "reason": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // Visible to workers (see `task_done_descriptor`).
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_fail(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let attempt_id = required_attempt_id(&args, TOOL_TASK_FAIL)?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("neige_task_fail: missing `reason` (non-empty)"))?
        .to_string();

    let report = WorkerTaskReport::Failed {
        attempt_id: attempt_id.clone(),
        reason,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, report).await?;
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
    report: WorkerTaskReport,
) -> Result<(), RpcError> {
    let kind_tag = report.event().kind_tag();
    let result = CardDecisionSink::from_app_context(ctx)
        .commit_worker_task_report(identity, report)
        .await;

    match result {
        Ok(_) => Ok(()),
        Err(e @ (CalmError::Forbidden(_) | CalmError::Conflict(_) | CalmError::NotFound(_))) => {
            Err(crate::mcp_server::framing::calm_error(e))
        }
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

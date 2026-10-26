//! Worker and child Planner outcome tools (`neige_task_done`, `neige_task_fail`). Workers report
//! on their own card; child Planners settle their validated parent assignment.

use crate::decision_sink::{CardDecisionSink, CommitMessage, WorkerTaskReport};
use crate::error::CalmError;
use crate::git_candidate::commit_message::DeliveryMessage;
use crate::git_candidate::delivery::delivery_latest_for_attempt;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    role_gated_write_annotations,
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
        roles: &[CardRole::Worker, CardRole::Planner],
        listed_for: &[CardRole::Worker, CardRole::Planner],
    }
}

async fn task_done(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let attempt_id = required_attempt_id(&args, TOOL_TASK_DONE)?;
    if identity.role == CardRole::Planner {
        if ["result", "artifacts", "commit_message"]
            .iter()
            .any(|key| args.get(key).is_some())
        {
            return Err(RpcError::invalid_params(
                "child Planner results are kernel-owned; result, artifacts and commit_message are Worker-only",
            ));
        }
        return super::track_state::report_child_assignment(ctx, identity, attempt_id, None).await;
    }
    let commit_message = CommitMessage::from_tool_arg(&args)
        .map_err(|error| RpcError::invalid_params(format!("{TOOL_TASK_DONE}: {error}")))?;
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
    // Read the admitted delivery input, including on idempotent repeats. Never infer text
    // from a candidate SHA: no-op checkpoints may pin a foreign base commit.
    let pool = ctx
        .sqlite_db
        .as_ref()
        .ok_or_else(|| RpcError::internal("read delivery message: SQLite pool not configured"))?;
    let delivery = delivery_latest_for_attempt(pool.read_pool(), &attempt_id)
        .await
        .map_err(|error| RpcError::internal(format!("read delivery message: {error}")))?;
    let message = match delivery.map(|delivery| delivery.commit_message) {
        Some(DeliveryMessage::Worker(message)) => Some(message.as_str().to_string()),
        Some(DeliveryMessage::Kernel) | None => None,
    };
    Ok(json!({
        "status": "report_received",
        "delivery_commit_message": message,
        "gate_run_commits_keep_original_message": true,
    }))
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
        roles: &[CardRole::Worker, CardRole::Planner],
        listed_for: &[CardRole::Worker, CardRole::Planner],
    }
}

async fn task_fail(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let attempt_id = required_attempt_id(&args, TOOL_TASK_FAIL)?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("neige_task_fail: missing `reason` (non-empty)"))?
        .to_string();

    if identity.role == CardRole::Planner {
        return super::track_state::report_child_assignment(
            ctx,
            identity,
            attempt_id,
            Some(reason),
        )
        .await;
    }

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
    let kind_tag = match &report {
        WorkerTaskReport::Completed { .. } => "task.completed",
        WorkerTaskReport::Failed { .. } => "task.failed",
    };
    let result = CardDecisionSink::from_app_context(ctx)
        .commit_worker_task_report(identity, report)
        .await;

    match result {
        Ok(_) => Ok(()),
        Err(
            e @ (CalmError::BadRequest(_)
            | CalmError::Forbidden(_)
            | CalmError::Conflict(_)
            | CalmError::NotFound(_)),
        ) => Err(crate::mcp_server::framing::calm_error(e)),
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

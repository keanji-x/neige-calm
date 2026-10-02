//! Worker outcome tools (`calm.task.complete`, `calm.task.fail`) and the retired
//! `calm.dispatch_request` shim. Every emitted event's scope is anchored on the caller's card.

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::event::{Event, ForgeEventSpec};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    register_deprecated_alias, require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::write_args::message_schema;
use crate::mcp_server::transport::PluginForgePayload;
use crate::model::CardRole;
use crate::operation::forge_action_adapter::ProbeSpec;
use serde_json::Map;
use serde_json::{Value, json};
use std::sync::Arc;

const TOOL_DISPATCH_REQUEST: &str = "calm.dispatch_request";
pub const TOOL_TASK_COMPLETE: &str = "calm.task.complete";
pub const TOOL_TASK_FAIL: &str = "calm.task.fail";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(dispatch_request_descriptor(), wrap(dispatch_request));
    registry.register(task_complete_descriptor(), wrap(task_complete));
    registry.register(task_fail_descriptor(), wrap(task_fail));
    register_deprecated_alias(registry, "calm.task_completed", TOOL_TASK_COMPLETE);
    register_deprecated_alias(registry, "calm.task_failed", TOOL_TASK_FAIL);
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

fn dispatch_request_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_DISPATCH_REQUEST.into(),
        description: include_str!("../../../prompts/tools/calm.dispatch_request.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["codex", "terminal"] },
                "idempotency_key": { "type": "string", "minLength": 1 },
                "goal": { "type": "string" },
                "context": {},
                "acceptance_criteria": { "type": ["string", "null"] },
                "cmd": { "type": "string" },
                "cwd": { "type": ["string", "null"] },
                "message": message_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[],
    }
}

async fn dispatch_request(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    Ok(json!({
        "error": "calm.dispatch_request was retired (#644); no task was dispatched",
        "migration": {
            "use": "calm.report.commit",
            "shape": "{ message, ops: [{ op: \"upsert\", kind: \"task\", payload: { key, kind, goal (codex/claude) | command (terminal), acceptance?, depends_on?, priority?, gate?, ready: true, declared_by: \"spec\" } }] }",
            "notes": "Read the report with calm.report.read first. The kernel schedules ready task blocks and runs verification gates; use `neige state` for status."
        }
    }))
}

fn task_complete_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_COMPLETE.into(),
        description: include_str!("../../../prompts/tools/calm.task.complete.md")
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

async fn task_complete(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let attempt_id = required_attempt_id(&args, "task_complete")?;
    let result = args.get("result").cloned().unwrap_or(Value::Null);
    let artifacts_val = args
        .get("artifacts")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    let artifacts: Vec<crate::event::ArtifactRef> = serde_json::from_value(artifacts_val)
        .map_err(|e| RpcError::invalid_params(format!("task_complete: invalid artifacts: {e}")))?;

    let event = Event::TaskCompleted {
        idempotency_key: attempt_id.clone(),
        result,
        artifacts,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    submit_reported_delivery(&ctx, &identity, &attempt_id).await;
    Ok(json!({ "status": "emitted" }))
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

/// The one constructor of a worker delivery payload (the kernel delivery's).
pub(crate) fn worker_delivery_payload(
    idem_key: String,
    argv: Vec<String>,
    table: ForgeEventSpec,
    probes: ProbeSpec,
) -> PluginForgePayload {
    PluginForgePayload {
        argv,
        idem_key,
        event_spec: Some(table),
        subject: None,
        context: Map::new(),
        probe: Some(probes),
        parked: false,
    }
}

fn task_fail_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_FAIL.into(),
        description: include_str!("../../../prompts/tools/calm.task.fail.md")
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
        // Visible to workers (see `task_complete_descriptor`).
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_fail(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let attempt_id = required_attempt_id(&args, "task_fail")?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("task_fail: missing `reason` (non-empty)"))?
        .to_string();

    let event = Event::TaskFailed {
        idempotency_key: attempt_id.clone(),
        reason,
        details: None,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    submit_reported_delivery(&ctx, &identity, &attempt_id).await;
    Ok(json!({ "status": "emitted" }))
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

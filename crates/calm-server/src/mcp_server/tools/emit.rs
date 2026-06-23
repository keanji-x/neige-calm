//! Emit tools for dispatching workers and recording worker outcomes.
//!
//! All three lower a JSON `arguments` object to a single eventized
//! write. The kernel translates the per-call [`ToolCallIdentity`]
//! into an [`ActorId`] (Spec → `AiSpec`, Worker → `AiCodex`) and emits
//! through `write_with_event_typed`, which runs the role gate, persists
//! the event row, and broadcasts on the bus.
//!
//! ## Tool surface
//!
//! * `calm.task.dispatch` — retired #644 compatibility shim. Hidden
//!   from tools/list; persisted pre-cutover spec threads can still call
//!   it and receive a structured migration payload. It performs no write.
//!
//! * `calm.task.complete` — Worker reports success with an opaque
//!   result + artifact list. Maps to `Event::TaskCompleted`.
//!
//! * `calm.task.fail` — Worker reports failure with a free-form
//!   reason. Maps to `Event::TaskFailed`.
//!
//! ## Scope construction
//!
//! Every emitted event's `EventScope` is anchored on the *caller's*
//! card — the kernel pulls `wave_id` + `cove_id` by looking up the
//! card row + the wave row, so the spec card's emissions land under
//! `EventScope::Card { card, wave, cove }`. Worker cards emit under
//! their own card scope; the role gate enforces that they can't
//! escape it.

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::event::Event;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    register_deprecated_alias, require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::lifecycle_args::{lifecycle_schema, message_schema};
use crate::model::CardRole;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TASK_DISPATCH: &str = "calm.task.dispatch";
pub const TOOL_TASK_COMPLETE: &str = "calm.task.complete";
pub const TOOL_TASK_FAIL: &str = "calm.task.fail";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(task_dispatch_descriptor(), wrap(task_dispatch));
    registry.register(task_complete_descriptor(), wrap(task_complete));
    registry.register(task_fail_descriptor(), wrap(task_fail));
    register_deprecated_alias(registry, "calm.dispatch_request", TOOL_TASK_DISPATCH);
    register_deprecated_alias(registry, "calm.task_completed", TOOL_TASK_COMPLETE);
    register_deprecated_alias(registry, "calm.task_failed", TOOL_TASK_FAIL);
}

/// Common wrapper that turns a typed async fn into the boxed-future
/// `ToolHandler` the registry expects. Saves three copies of the same
/// `Box::pin` boilerplate.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

// ---------------------------------------------------------------------------
// calm.task.dispatch
// ---------------------------------------------------------------------------

fn task_dispatch_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_DISPATCH.into(),
        description: "Deprecated compatibility shim: `calm.task.dispatch` was \
             retired in #644. Use `calm.plan.upsert` to maintain the task plan; \
             the kernel schedules ready tasks and runs gates."
            .into(),
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
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[],
    }
}

async fn task_dispatch(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    Ok(json!({
        "error": "calm.task.dispatch was retired (#644); no task was dispatched",
        "migration": {
            "use": "calm.plan.upsert",
            "shape": "{ tasks: [{ key, kind, goal, depends_on?, priority?, gate? }], message }",
            "notes": "The kernel schedules ready tasks and runs verification gates. Use calm.plan.list to see task status."
        }
    }))
}

// ---------------------------------------------------------------------------
// calm.task.complete
// ---------------------------------------------------------------------------

fn task_complete_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_COMPLETE.into(),
        description: "Report that a worker card has completed its task. \
             `idempotency_key` should echo the kernel-provided task id so \
             the spec card can correlate."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "result": {},
                "artifacts": { "type": "array" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #838 Move 2 — visible to workers so a codex worker's `tools/list`
        // advertises the native completion tool (it reports completion via
        // this tool instead of the `neige` CLI). Role-gated to Worker (the
        // handler also `require_role(Worker)`s).
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_complete(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_complete: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let result = args.get("result").cloned().unwrap_or(Value::Null);
    let artifacts_val = args
        .get("artifacts")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    let artifacts: Vec<crate::event::ArtifactRef> = serde_json::from_value(artifacts_val)
        .map_err(|e| RpcError::invalid_params(format!("task_complete: invalid artifacts: {e}")))?;

    let event = Event::TaskCompleted {
        idempotency_key,
        result,
        artifacts,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// calm.task.fail
// ---------------------------------------------------------------------------

fn task_fail_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_FAIL.into(),
        description: "Report that a worker card has failed its task. \
             `reason` is free-form and persisted verbatim on the event row."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key", "reason"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "reason": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #838 Move 2 — visible to workers (see `task_complete_descriptor`).
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_fail(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_fail: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("task_fail: missing `reason`"))?
        .to_string();

    let event = Event::TaskFailed {
        idempotency_key,
        reason,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// Shared emit path — derives the session-shaped actor from ToolCallIdentity
// inside CardDecisionSink and delegates the eventized write.
// ---------------------------------------------------------------------------

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
            // Role gate refusal — surface as a custom error code so a
            // mis-roled card sees a deterministic failure shape rather
            // than a generic internal error.
            Err(RpcError::custom(
                -32403,
                format!("emit {kind_tag}: forbidden: {msg}"),
            ))
        }
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

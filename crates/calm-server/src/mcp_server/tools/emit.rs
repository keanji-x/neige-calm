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
use crate::event::{Event, FieldSource, ForgeEventSpec};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    register_deprecated_alias, require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::lifecycle_args::{lifecycle_schema, message_schema};
use crate::mcp_server::transport::{PluginForgePayload, submit_forge_action};
use crate::model::CardRole;
use crate::operation::forge_action_adapter::ProbeSpec;
use crate::operation::workspace_lease::{
    git_repo_root_for_wave_cwd, workspace_lease_path_for, workspace_slice_branch_for,
};
use serde_json::Map;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const TOOL_TASK_DISPATCH: &str = "calm.task.dispatch";
pub const TOOL_TASK_COMPLETE: &str = "calm.task.complete";
pub const TOOL_TASK_FAIL: &str = "calm.task.fail";
const GIT_FORGE_PLUGIN_ID: &str = "dev.neige.git-forge";
const GIT_COMMIT_SCRIPT: &str = "git add -A && git commit -m \"$1\" || true; \
     git log -1 --format='{\"commit\":\"%H\",\"branch\":\"'\"$2\"'\"}'";
const GIT_COMMIT_PROBE_SCRIPT: &str = "git rev-parse --verify HEAD >/dev/null 2>&1 || exit 3; \
     if git diff --cached --quiet 2>/dev/null; then exit 0; else exit 1; fi";
const GIT_COMMIT_OUTPUT_PROBE_SCRIPT: &str =
    "git log -1 --format='{\"commit\":\"%H\",\"branch\":\"'\"$1\"'\"}'";

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
        visible_to_roles: &[],
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
    if let Err(error) = submit_worker_success_commit(&ctx, &identity).await {
        tracing::warn!(
            card_id = %identity.card_id,
            wave_id = identity.wave_id.as_deref().unwrap_or("<missing>"),
            error = %error,
            "task_complete: worker success persisted but deterministic commit enqueue failed"
        );
    }
    Ok(json!({ "status": "emitted" }))
}

async fn submit_worker_success_commit(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(), String> {
    let wave_id = identity
        .wave_id
        .clone()
        .ok_or_else(|| "worker success commit requires a wave-scoped caller".to_string())?;
    let wave = ctx
        .repo
        .wave_get(&wave_id)
        .await
        .map_err(|e| format!("worker success commit wave lookup: {e}"))?
        .ok_or_else(|| format!("unknown wave `{wave_id}`"))?;
    if wave.cove_id.as_str() != identity.cove_id.as_str() {
        return Err("worker success commit wave belongs to a different cove".into());
    }

    let repo_root = git_repo_root_for_wave_cwd(&wave_id, &wave.cwd)
        .map_err(|e| format!("worker success commit repo root: {e}"))?;
    let card_id = identity.card_id.clone();
    let cwd_lease = workspace_lease_path_for(&repo_root, &wave_id, &card_id)
        .map_err(|e| format!("worker success commit worktree path: {e}"))?;
    let branch = workspace_slice_branch_for(&wave_id, &card_id)
        .map_err(|e| format!("worker success commit branch: {e}"))?;
    let message = format!("neige: worker {card_id} @ wave {wave_id}");

    let payload = PluginForgePayload {
        argv: vec![
            "sh".into(),
            "-c".into(),
            GIT_COMMIT_SCRIPT.into(),
            "sh".into(),
            message,
            branch.clone(),
        ],
        idem_key: "git.commit:auto".into(),
        event_spec: Some(worktree_committed_event_spec()),
        subject: None,
        context: Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: vec![
                "sh".into(),
                "-c".into(),
                GIT_COMMIT_PROBE_SCRIPT.into(),
                "sh".into(),
            ],
            output_probe_argv: Some(vec![
                "sh".into(),
                "-c".into(),
                GIT_COMMIT_OUTPUT_PROBE_SCRIPT.into(),
                "sh".into(),
                branch,
            ]),
        }),
        parked: false,
    };

    match submit_forge_action(
        ctx,
        GIT_FORGE_PLUGIN_ID,
        wave_id,
        card_id,
        cwd_lease,
        payload,
    )
    .await
    .map_err(|e| e.to_string())?
    {
        Ok(_submission) => Ok(()),
        Err(error) => Err(error),
    }
}

fn worktree_committed_event_spec() -> ForgeEventSpec {
    ForgeEventSpec {
        event_kind: "worktree.committed".into(),
        fields: BTreeMap::from([
            (
                "branch".into(),
                FieldSource::JsonField {
                    path: "/branch".into(),
                },
            ),
            (
                "commit_sha".into(),
                FieldSource::JsonField {
                    path: "/commit".into(),
                },
            ),
        ]),
    }
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
        visible_to_roles: &[],
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

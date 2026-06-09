//! PR7a (#136) emit tools — `calm.dispatch_request`,
//! `calm.task_completed`, `calm.task_failed`.
//!
//! All three lower a JSON `arguments` object to a single eventized
//! write. The kernel translates the per-call [`ToolCallIdentity`]
//! into an [`ActorId`] (Spec → `AiSpec`, Worker → `AiCodex`) and emits
//! through `write_with_event_typed`, which runs the role gate, persists
//! the event row, and broadcasts on the bus.
//!
//! ## Tool surface
//!
//! * `calm.dispatch_request` — Spec card asks the kernel dispatcher to
//!   spawn a worker (codex or terminal). Two variants distinguished by
//!   `kind: "codex" | "terminal"`. Maps to
//!   `Event::CodexJobRequested` / `Event::TerminalJobRequested`.
//!
//! * `calm.task_completed` — Worker reports success with an opaque
//!   result + artifact list. Maps to `Event::TaskCompleted`.
//!
//! * `calm.task_failed` — Worker reports failure with a free-form
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

use crate::db::write_with_event_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::CardId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, require_role_any, role_gated_write_annotations,
};
use crate::model::CardRole;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_DISPATCH_REQUEST: &str = "calm.dispatch_request";
pub const TOOL_TASK_COMPLETED: &str = "calm.task_completed";
pub const TOOL_TASK_FAILED: &str = "calm.task_failed";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(dispatch_request_descriptor(), wrap(dispatch_request));
    registry.register(task_completed_descriptor(), wrap(task_completed));
    registry.register(task_failed_descriptor(), wrap(task_failed));
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
// calm.dispatch_request
// ---------------------------------------------------------------------------

fn dispatch_request_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_DISPATCH_REQUEST.into(),
        description: "Spec-only: request that the kernel dispatcher spawn a worker card. \
             `kind` selects codex vs terminal; `idempotency_key` must be \
             stable across retries so the dispatcher dedupes."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["kind", "idempotency_key"],
            "properties": {
                "kind": { "type": "string", "enum": ["codex", "terminal"] },
                "idempotency_key": { "type": "string", "minLength": 1 },
                "goal": { "type": "string" },
                "context": {},
                "acceptance_criteria": { "type": ["string", "null"] },
                "cmd": { "type": "string" },
                "cwd": { "type": ["string", "null"] }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn dispatch_request(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("dispatch_request: missing `idempotency_key` (non-empty)")
        })?
        .to_string();

    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("dispatch_request: missing `kind`"))?;

    let event = match kind {
        "codex" => {
            let goal = args
                .get("goal")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::invalid_params("dispatch_request[codex]: missing `goal`"))?
                .to_string();
            let context = args.get("context").cloned().unwrap_or(Value::Null);
            let acceptance_criteria = args
                .get("acceptance_criteria")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Event::CodexJobRequested {
                idempotency_key,
                goal,
                context,
                acceptance_criteria,
            }
        }
        "terminal" => {
            let cmd = args
                .get("cmd")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    RpcError::invalid_params("dispatch_request[terminal]: missing `cmd`")
                })?
                .to_string();
            let cwd = args
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Event::TerminalJobRequested {
                idempotency_key,
                cmd,
                cwd,
            }
        }
        other => {
            return Err(RpcError::invalid_params(format!(
                "dispatch_request: unknown kind `{other}` (expected `codex` or `terminal`)"
            )));
        }
    };

    emit_event_for_identity(&ctx, &identity, event).await?;

    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// calm.task_completed
// ---------------------------------------------------------------------------

fn task_completed_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_COMPLETED.into(),
        description: "Report that a worker card has completed its task. \
             `idempotency_key` should echo the matching `*.job_requested` \
             so the spec card can correlate."
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

async fn task_completed(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_completed: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let result = args.get("result").cloned().unwrap_or(Value::Null);
    let artifacts_val = args
        .get("artifacts")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    let artifacts: Vec<crate::event::ArtifactRef> = serde_json::from_value(artifacts_val)
        .map_err(|e| RpcError::invalid_params(format!("task_completed: invalid artifacts: {e}")))?;

    let event = Event::TaskCompleted {
        idempotency_key,
        result,
        artifacts,
    };
    emit_event_for_identity(&ctx, &identity, event).await?;
    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// calm.task_failed
// ---------------------------------------------------------------------------

fn task_failed_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_FAILED.into(),
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

async fn task_failed(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_failed: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("task_failed: missing `reason`"))?
        .to_string();

    let event = Event::TaskFailed {
        idempotency_key,
        reason,
    };
    emit_event_for_identity(&ctx, &identity, event).await?;
    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// Shared emit path — resolves scope from the caller's card and runs
// the eventized write through the role gate.
// ---------------------------------------------------------------------------

async fn emit_event_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    event: Event,
) -> Result<(), RpcError> {
    let actor = identity.to_actor_id();
    let card_id_str = identity.card_id.as_str().to_string();

    // Resolve `wave_id` + `cove_id` from the card -> wave chain so the
    // event's `scope_*` columns carry the full ancestor breadcrumbs.
    // The card was minted at handshake bind time; a card row going
    // missing between then and now indicates a delete-while-active race
    // — surface as InternalError so the operator sees it.
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("emit: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "emit: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("emit: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "emit: wave {} for card {} not found",
                card.wave_id.as_str(),
                card_id_str
            ))
        })?;

    let scope = EventScope::Card {
        card: CardId::from(card_id_str.clone()),
        wave: wave.id,
        cove: wave.cove_id,
    };
    let kind_tag = event.kind_tag();

    let result = write_with_event_typed::<(), _>(
        ctx.repo.as_ref(),
        actor,
        scope,
        None,
        &ctx.events,
        &ctx.write,
        move |_tx| {
            let event = event.clone();
            Box::pin(async move { Ok(((), event)) })
        },
    )
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

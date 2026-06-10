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
//! * `calm.task.dispatch` — Spec card asks the kernel dispatcher to
//!   spawn a worker (codex or terminal). Two variants distinguished by
//!   `kind: "codex" | "terminal"`. Maps to
//!   `Event::CodexWorkerRequested` / `Event::TerminalWorkerRequested`.
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

use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    register_deprecated_alias, require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::lifecycle_args::{
    lifecycle_schema, message_schema, parse_write_args,
};
use crate::model::CardRole;
use crate::wave_lifecycle::{
    apply_requested_transition_in_tx, auto_promote_draft_in_tx, auto_transition_if_current_in_tx,
};
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
        description: "Spec-only: request that the kernel dispatcher spawn a worker card. \
             `kind` selects codex vs terminal; `idempotency_key` must be \
             stable across retries so the dispatcher dedupes. `message` is \
             required and should explain why this dispatch is being made; it \
             is persisted as `agent_message`. Optional `lifecycle` drives the \
             wave state machine in the same atomic write when you need to \
             advance to planning/dispatching/working/blocked/reviewing/done/failed."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["kind", "idempotency_key", "message"],
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
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn task_dispatch(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let write_args = parse_write_args(&args, "task_dispatch")?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_dispatch: missing `idempotency_key` (non-empty)")
        })?
        .to_string();

    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("task_dispatch: missing `kind`"))?;

    let event = match kind {
        "codex" => {
            let goal = args
                .get("goal")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::invalid_params("task_dispatch[codex]: missing `goal`"))?
                .to_string();
            let context = args.get("context").cloned().unwrap_or(Value::Null);
            let acceptance_criteria = args
                .get("acceptance_criteria")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Event::CodexWorkerRequested {
                idempotency_key,
                goal,
                context,
                acceptance_criteria,
                agent_message: Some(write_args.message.clone()),
            }
        }
        "terminal" => {
            let cmd = args
                .get("cmd")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::invalid_params("task_dispatch[terminal]: missing `cmd`"))?
                .to_string();
            let cwd = args
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Event::TerminalWorkerRequested {
                idempotency_key,
                cmd,
                cwd,
                agent_message: Some(write_args.message.clone()),
            }
        }
        other => {
            return Err(RpcError::invalid_params(format!(
                "task_dispatch: unknown kind `{other}` (expected `codex` or `terminal`)"
            )));
        }
    };

    emit_spec_write_for_identity(
        &ctx,
        &identity,
        event,
        write_args.lifecycle,
        write_args.message,
    )
    .await?;

    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// calm.task.complete
// ---------------------------------------------------------------------------

fn task_complete_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_COMPLETE.into(),
        description: "Report that a worker card has completed its task. \
             `idempotency_key` should echo the matching `*.worker_requested` \
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
    emit_task_report_for_identity(&ctx, &identity, event).await?;
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
    emit_task_report_for_identity(&ctx, &identity, event).await?;
    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// Shared emit path — resolves scope from the caller's card and runs
// the eventized write through the role gate.
// ---------------------------------------------------------------------------

async fn emit_spec_write_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    event: Event,
    lifecycle: Option<crate::model::WaveLifecycle>,
    message: String,
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
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_id = wave.id.clone();
    let kind_tag = event.kind_tag();

    let result = write_with_actor_events_typed::<(), _>(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            let event = event.clone();
            let wave_id = wave_id.clone();
            let wave_scope = wave_scope.clone();
            let scope = scope.clone();
            let actor = actor.clone();
            let message = message.clone();
            Box::pin(async move {
                let mut events = Vec::new();
                if let Some(auto_events) = auto_promote_draft_in_tx(tx, &wave_id).await? {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &wave_id,
                        target,
                        &actor,
                        message.clone(),
                    )
                    .await?
                {
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), wave_scope.clone(), event)),
                    );
                }
                events.push((actor, scope, event));
                Ok(((), events))
            })
        },
    )
    .await;

    match result {
        Ok(_) => Ok(()),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("emit {kind_tag}: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

async fn emit_task_report_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    event: Event,
) -> Result<(), RpcError> {
    let actor = identity.to_actor_id();
    let card_id_str = identity.card_id.as_str().to_string();

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
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_id = wave.id.clone();
    let kind_tag = event.kind_tag();

    let result = write_with_actor_events_typed::<(), _>(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            let event = event.clone();
            let actor = actor.clone();
            let scope = scope.clone();
            let wave_scope = wave_scope.clone();
            let wave_id = wave_id.clone();
            Box::pin(async move {
                let mut events = vec![(actor, scope, event)];
                if let Some(auto_events) = auto_transition_if_current_in_tx(
                    tx,
                    &wave_id,
                    crate::model::WaveLifecycle::Working,
                    crate::model::WaveLifecycle::Reviewing,
                    &ActorId::Kernel,
                    Some("[auto] first task report".to_string()),
                )
                .await?
                {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                    );
                }
                Ok(((), events))
            })
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

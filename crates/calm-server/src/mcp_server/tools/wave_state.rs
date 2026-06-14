//! Wave-state tools for reading wave shape and recording spec verdicts on
//! worker outcomes.
//!
//! These tools complete the spec-card closed loop: a spec daemon reads
//! the current wave snapshot and marks individual worker results as
//! accepted / rejected during validation. The dispatcher then closes the
//! loop by pushing the next worker-emitted event onto the spec's thread
//! as a turn input (#293 — no polling).
//!
//! ## Tool surface
//!
//! * `calm.wave.state` — Spec **or** Worker callable. Returns the
//!   thread-mapped card's wave row + the wave's card list
//!   (id/kind/role/runtime) as one JSON snapshot. No event emission.
//!   Workers occasionally peek wave state before they report; the spec
//!   gets a full snapshot every loop iteration.
//!
//! * `calm.task.verdict` — Spec only. Records the spec's
//!   accept/reject verdict on a worker's prior result. Lowers to
//!   either `Event::TaskCompleted` (verdict = "accepted") or
//!   `Event::TaskFailed` (verdict = "rejected"); the `idempotency_key`
//!   echoes the original `*.worker_requested` so consumers can correlate.
//!
//!   ### Variant choice (TaskCompleted/TaskFailed reuse vs. new variant)
//!
//!   The earliest-stage design considered adding
//!   `Event::TaskMetaUpdated { idempotency_key, metadata: Value }` as
//!   an explicit metadata channel. We picked the reuse path because:
//!     * the only PR7b use case is the spec's accept/reject verdict on
//!       a completed worker run — perfectly captured by the existing
//!       success/failure semantics;
//!     * the spec's verdict *is* a terminal outcome from the spec's
//!       point of view, mirroring how the worker would report its own
//!       outcome — a single kind for "this idempotency_key is done"
//!       keeps consumer code (and the dispatcher's correlator)
//!       simpler;
//!     * a future PR that needs richer task metadata (per-iteration
//!       checkpoints, partial progress, structured artifacts) can add
//!       the dedicated variant then without rewriting today's
//!       call sites — the MCP tool name stays stable while the wire
//!       event shape evolves under it.
//!
//!   The verdict + optional reason are folded into the
//!   `TaskCompleted.result` JSON (`{status, reason}`) so the audit log
//!   carries the spec's rationale verbatim.
//!
//! ## Scope construction
//!
//! Unlike PR7a's emit tools (which scope to the caller's *card*), the
//! the verdict write scopes to the caller's *wave*. The verdict is
//! wave-level metadata about a worker the spec supervises, not the
//! spec's own card state.

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::event::Event;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, register_deprecated_alias, require_role, require_role_any,
    role_gated_write_annotations,
};
use crate::mcp_server::tools::lifecycle_args::{
    lifecycle_schema, message_schema, parse_write_args,
};
use crate::model::{CardRole, Wave};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_WAVE_STATE: &str = "calm.wave.state";
pub const TOOL_TASK_VERDICT: &str = "calm.task.verdict";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(wave_state_descriptor(), wrap(wave_state));
    registry.register(task_verdict_descriptor(), wrap(task_verdict));
    register_deprecated_alias(registry, "calm.get_wave_state", TOOL_WAVE_STATE);
    register_deprecated_alias(registry, "calm.update_task_meta", TOOL_TASK_VERDICT);
}

/// Common wrapper that turns a typed async fn into the boxed-future
/// `ToolHandler` the registry expects. Mirrors `emit::wrap`.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

// ---------------------------------------------------------------------------
// calm.wave.state
// ---------------------------------------------------------------------------

fn wave_state_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_STATE.into(),
        description: "Read the current wave snapshot bound to the calling card. \
             Returns the wave row plus a card list so a spec daemon can see \
             worker progress without a second call. Each card carries `id`, \
             `kind`, `role`, `sort`, `created_at`, `updated_at`, plus \
             `runtime` (typed `CardRuntimeView` or `null` when no runtime row). \
             Callable by spec and worker cards alike; no event is emitted."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

async fn wave_state(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let mut cards = ctx
        .repo
        .cards_by_wave(wave.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_state: cards_by_wave: {e}")))?;
    crate::runtime_lookup::project_runtime_into_cards_payload(ctx.repo.as_ref(), &mut cards)
        .await
        .map_err(|e| RpcError::internal(format!("wave_state: runtime projection: {e}")))?;

    // We re-query the role cache rather than fetching `cards.role` on
    // the card row — the cache is the canonical source the role gate
    // already trusts, and `Card` doesn't carry `role` on the struct
    // (it's a column the cache mirrors). One cache hit per card; the
    // cache is in-process and lock-free for reads.
    let cards_json: Vec<Value> = cards
        .iter()
        .map(|c| {
            let role = ctx.write.verify_role(&c.id).unwrap_or_default();
            json!({
                "id": c.id,
                "kind": c.kind,
                "role": role,
                "sort": c.sort,
                "created_at": c.created_at,
                "updated_at": c.updated_at,
                "runtime": c.runtime.clone(),
            })
        })
        .collect();

    Ok(json!({
        "wave": wave,
        "cards": cards_json,
    }))
}

// ---------------------------------------------------------------------------
// calm.task.verdict
// ---------------------------------------------------------------------------

fn task_verdict_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_VERDICT.into(),
        description: "Spec-only: record the spec's accept/reject verdict on \
             a worker's prior result. `idempotency_key` echoes the original \
             `*.worker_requested`. `status = \"accepted\"` emits \
             `task.completed`; `status = \"rejected\"` emits `task.failed` \
             with `reason` (free-form). `message` is required and should \
             explain the verdict; it is persisted as `agent_message`. \
             Optional `lifecycle` drives the wave state machine in the same \
             atomic write when accepting, rejecting, blocking, or continuing \
             the wave. The verdict is persisted on the events log so audit \
             replay surfaces the spec's rationale."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key", "status", "message"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "status": { "type": "string", "enum": ["accepted", "rejected"] },
                "reason": { "type": "string" },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn task_verdict(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let write_args = parse_write_args(&args, "task_verdict")?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_verdict: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("task_verdict: missing `status`"))?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let event = match status {
        "accepted" => Event::TaskCompleted {
            idempotency_key,
            // Fold the verdict + reason into `result` so audit replay
            // sees the spec's rationale verbatim. Workers' own
            // task.completed emits leave `result` to free-form agent
            // output; the spec's emits use this structured shape so a
            // downstream consumer can pattern-match on
            // `result.status == "accepted"` to tell verdicts apart
            // from worker self-reports.
            result: json!({
                "status": "accepted",
                "reason": reason.unwrap_or_default(),
            }),
            artifacts: vec![],
            agent_message: Some(write_args.message.clone()),
        },
        "rejected" => Event::TaskFailed {
            idempotency_key,
            // `reason` is required-by-convention for rejections; an
            // empty string is a valid value (the spec might reject
            // for "no reason given" — we don't second-guess the
            // verdict).
            reason: reason.unwrap_or_default(),
            agent_message: Some(write_args.message.clone()),
        },
        other => {
            return Err(RpcError::invalid_params(format!(
                "task_verdict: unknown status `{other}` (expected `accepted` or `rejected`)"
            )));
        }
    };

    let kind_tag = event.kind_tag();
    let res = CardDecisionSink::from_app_context(&ctx)
        .commit_spec_verdict(&identity, write_args.message, write_args.lifecycle, event)
        .await;

    match res {
        Ok(_) => Ok(json!({ "ok": true })),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("emit {kind_tag}: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Look up the wave the calling card belongs to, returning the card +
/// wave rows. Mirrors PR7a's `emit_event_for_identity` resolve step:
/// the thread-mapped card must exist while its daemon is active; a
/// missing row means a delete-while-active race, which we surface as
/// `InternalError` (the operator wants to see this loud).
async fn resolve_wave_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(crate::model::Card, Wave), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("wave_state: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_state: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_state: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_state: wave {} for card {} not found",
                card.wave_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, wave))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CardRole;

    fn identity_with_role(role: CardRole) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: "card-1".to_string(),
            role,
            session_id: "session-1".to_string(),
            wave_id: Some("wave-1".to_string()),
            cove_id: "cove-1".to_string(),
            thread_id: "thread-1".to_string(),
        }
    }

    #[test]
    fn require_role_accepts_matching_role() {
        let id = identity_with_role(CardRole::Spec);
        assert!(require_role(&id, CardRole::Spec).is_ok());
    }

    #[test]
    fn require_role_rejects_worker_for_spec_tool() {
        let id = identity_with_role(CardRole::Worker);
        let err = require_role(&id, CardRole::Spec).expect_err("worker must be denied");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("Spec"),
            "error should mention required role: {err:?}"
        );
        assert!(
            err.message.contains("Worker"),
            "error should mention got role: {err:?}"
        );
    }
}

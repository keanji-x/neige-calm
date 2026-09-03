//! Track-state tools for reading track shape and recording planner verdicts on
//! worker outcomes.
//!
//! These tools complete the planner-card closed loop: a planner daemon reads
//! the current track snapshot and marks individual worker results as
//! accepted / rejected during validation. The dispatcher then closes the
//! loop by pushing the next worker-emitted event onto the planner's thread
//! as a turn input (#293 — no polling).
//!
//! ## Tool surface
//!
//! * `calm.track.state` — Planner **or** Worker callable. Returns the
//!   thread-mapped card's track row + the track's card list
//!   (id/kind/role/runtime) as one JSON snapshot. No event emission.
//!   Workers occasionally peek track state before they report; the planner
//!   gets a full snapshot every loop iteration.
//!
//! * `calm.task.verdict` — Planner only. Records the planner's
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
//!     * the only PR7b use case is the planner's accept/reject verdict on
//!       a completed worker run — perfectly captured by the existing
//!       success/failure semantics;
//!     * the planner's verdict *is* a terminal outcome from the planner's
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
//!   carries the planner's rationale verbatim.
//!
//! ## Scope construction
//!
//! Unlike PR7a's emit tools (which scope to the caller's *card*), the
//! the verdict write scopes to the caller's *track*. The verdict is
//! track-level metadata about a worker the planner supervises, not the
//! planner's own card state.

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
use crate::model::{Card, CardRole, Track};
use crate::track_report::TrackReportPayload;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TRACK_STATE: &str = "calm.track.state";
pub const TOOL_TASK_VERDICT: &str = "calm.task.verdict";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(track_state_descriptor(), wrap(track_state));
    registry.register(task_verdict_descriptor(), wrap(task_verdict));
    register_deprecated_alias(registry, "calm.get_track_state", TOOL_TRACK_STATE);
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
// calm.track.state
// ---------------------------------------------------------------------------

fn track_state_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_STATE.into(),
        description: "Read the current track snapshot bound to the calling card. \
             Returns the track row plus a card list so a planner daemon can see \
             worker progress without a second call. Each card carries `id`, \
             `kind`, `role`, `sort`, `created_at`, `updated_at`, plus \
             `runtime` (typed `CardRuntimeView` or `null` when no runtime row). \
             `report_startup_read_required` is true iff the track-report \
             summary/body is not the canonical empty initial report. \
             Callable by planner and worker cards alike; no event is emitted."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

async fn track_state(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let mut cards = ctx
        .repo
        .cards_by_track(track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_state: cards_by_track: {e}")))?;
    crate::session_projection_lookup::project_runtime_into_cards_payload(
        ctx.repo.as_ref(),
        &mut cards,
    )
    .await
    .map_err(|e| RpcError::internal(format!("track_state: runtime projection: {e}")))?;

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
        "track": track,
        "cards": cards_json,
        "report_startup_read_required": report_startup_read_required(&cards),
    }))
}

/// #1110 S3 — false only for the canonical empty initial report (or when
/// the track has no report card). Unparseable payloads are not that
/// placeholder, so they require a startup read.
fn report_startup_read_required(cards: &[Card]) -> bool {
    match cards.iter().find(|card| card.kind == "track-report") {
        Some(card) => serde_json::from_value::<TrackReportPayload>(card.payload.clone())
            .map(|payload| payload.report_startup_read_required())
            .unwrap_or(true),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// calm.task.verdict
// ---------------------------------------------------------------------------

fn task_verdict_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_VERDICT.into(),
        description: "Planner-only: record the planner's accept/reject verdict on \
             a worker's prior result. `idempotency_key` echoes the original \
             `*.worker_requested`. `status = \"accepted\"` emits \
             `task.completed`; `status = \"rejected\"` emits `task.failed` \
             with `reason` (free-form). `message` is required and should \
             explain the verdict; it is persisted as `agent_message`. \
             Optional `lifecycle` drives the track state machine in the same \
             atomic write when accepting, rejecting, blocking, or continuing \
             the track. The verdict is persisted on the events log so audit \
             replay surfaces the planner's rationale."
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
        visible_to_roles: &[CardRole::Planner],
    }
}

async fn task_verdict(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
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
            // sees the planner's rationale verbatim. Workers' own
            // task.completed emits leave `result` to free-form agent
            // output; the planner's emits use this structured shape so a
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
            // empty string is a valid value (the planner might reject
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
        .commit_planner_verdict(&identity, write_args.message, write_args.lifecycle, event)
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

/// Look up the track the calling card belongs to, returning the card +
/// track rows. Mirrors PR7a's `emit_event_for_identity` resolve step:
/// the thread-mapped card must exist while its daemon is active; a
/// missing row means a delete-while-active race, which we surface as
/// `InternalError` (the operator wants to see this loud).
async fn resolve_track_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(crate::model::Card, Track), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("track_state: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_state: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_state: track lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_state: track {} for card {} not found",
                card.track_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, track))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CardRole;

    fn identity_with_role(role: CardRole) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: "card-1".to_string(),
            role,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: "session-1".to_string(),
            track_id: Some("track-1".to_string()),
            area_id: "area-1".to_string(),
            thread_id: "thread-1".to_string(),
        }
    }

    #[test]
    fn require_role_accepts_matching_role() {
        let id = identity_with_role(CardRole::Planner);
        assert!(require_role(&id, CardRole::Planner).is_ok());
    }

    #[test]
    fn require_role_rejects_worker_for_planner_tool() {
        let id = identity_with_role(CardRole::Worker);
        let err = require_role(&id, CardRole::Planner).expect_err("worker must be denied");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("Planner"),
            "error should mention required role: {err:?}"
        );
        assert!(
            err.message.contains("Worker"),
            "error should mention got role: {err:?}"
        );
    }
}

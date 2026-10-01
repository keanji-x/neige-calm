//! Track-state tools: `calm.track.state` (Planner or Worker snapshot read, no event emission),
//! `calm.task.verdict` (Planner-only accept/reject, lowered to `TaskCompleted` / `TaskFailed`, scoped to the caller's track)
//! and `calm.track.close` (Planner-only close of the caller's track).

use crate::decision_sink::{CardDecisionSink, CardDecisionSinkRecorderShadowProbe};
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, register_deprecated_alias, require_role, require_role_any,
    role_gated_write_annotations,
};
use crate::mcp_server::tools::write_args::{message_schema, parse_write_args};
use crate::model::{Card, CardRole, Track, TrackPatch};
use crate::recorder_shadow::{RecorderShadowDecisionKind, RecorderShadowProbe};
use crate::track_report::TrackReportPayload;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TRACK_STATE: &str = "calm.track.state";
pub const TOOL_TASK_VERDICT: &str = "calm.task.verdict";
pub const TOOL_TRACK_CLOSE: &str = "calm.track.close";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(track_state_descriptor(), wrap(track_state));
    registry.register(task_verdict_descriptor(), wrap(task_verdict));
    registry.register(track_close_descriptor(), wrap(track_close));
    register_deprecated_alias(registry, "calm.get_track_state", TOOL_TRACK_STATE);
    register_deprecated_alias(registry, "calm.update_task_meta", TOOL_TASK_VERDICT);
}

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

fn track_state_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_STATE.into(),
        description: include_str!("../../../prompts/tools/calm.track.state.md")
            .trim_end()
            .to_string(),
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
    let (caller, track) = resolve_track_for_identity(&ctx, &identity).await?;
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

    // The role cache is the canonical source the role gate already trusts; `Card` doesn't carry `role` on the struct.
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

    // `tasks_by_track` reads the `current_tasks` view: one row per key, its current execution only,
    // the same notion of current as `calm.plan.list`.
    let tasks: Vec<Value> = ctx
        .repo
        .tasks_by_track(track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_state: tasks_by_track: {e}")))?
        .into_iter()
        .map(|task| {
            json!({
                "key": task.key,
                "status": task.status,
                "worker_card_id": task.worker_card_id,
            })
        })
        .collect();

    Ok(json!({
        "track": track,
        "caller_card_id": caller.id,
        "cards": cards_json,
        "report_startup_read_required": report_startup_read_required(&cards),
        "tasks": tasks,
    }))
}

/// False only for an unwritten report (the header placeholder, or the frozen pre-header body byte for byte) or when the track has no report card.
fn report_startup_read_required(cards: &[Card]) -> bool {
    match cards.iter().find(|card| card.kind == "track-report") {
        Some(card) => serde_json::from_value::<TrackReportPayload>(card.payload.clone())
            .map(|payload| payload.report_startup_read_required())
            .unwrap_or(true),
        None => false,
    }
}

fn task_verdict_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_VERDICT.into(),
        description: calm_types::observation::render_task_acceptance_guidance(
            include_str!("../../../prompts/tools/calm.task.verdict.md").trim_end(),
        ),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key", "status", "message"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "status": { "type": "string", "enum": ["accepted", "rejected"] },
                "reason": { "type": "string" },
                "message": message_schema()
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
    let message = parse_write_args(&args, "task_verdict")?;

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
            // Structured `{status, reason}` so a consumer can tell planner verdicts (`result.status == "accepted"`) apart from workers' free-form self-reports.
            result: json!({
                "status": "accepted",
                "reason": reason.unwrap_or_default(),
            }),
            artifacts: vec![],
            agent_message: Some(message.clone()),
        },
        "rejected" => Event::TaskFailed {
            idempotency_key,
            // An empty reason is a valid value; the verdict is not second-guessed.
            reason: reason.unwrap_or_default(),
            details: None,
            agent_message: Some(message.clone()),
        },
        other => {
            return Err(RpcError::invalid_params(format!(
                "task_verdict: unknown status `{other}` (expected `accepted` or `rejected`)"
            )));
        }
    };

    let kind_tag = event.kind_tag();
    let res = CardDecisionSink::from_app_context(&ctx)
        .commit_planner_verdict(&identity, event)
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

fn track_close_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_CLOSE.into(),
        description: include_str!("../../../prompts/tools/calm.track.close.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["message"],
            "properties": {
                "message": message_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

/// Closes the caller's track; closing a closed track is a no-op that returns its `closed_at`.
async fn track_close(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let message = parse_write_args(&args, TOOL_TRACK_CLOSE)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let recorder = CardDecisionSinkRecorderShadowProbe::for_identity(&identity, track.id.clone());
    // A close that finds the track already closed writes nothing: the batch may not be empty, so
    // the closure leaves the stamp here and rolls the transaction back with any error.
    let already_closed = Arc::new(std::sync::OnceLock::<i64>::new());
    let already_closed_in_tx = Arc::clone(&already_closed);
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let written = crate::db::write_with_events_typed(
        ctx.repo.as_ref(),
        identity.to_actor_id(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                // In the transaction: a session superseded after the transport check is denied here,
                // and a close that raced another close sees it and writes nothing.
                recorder
                    .record(tx, RecorderShadowDecisionKind::TrackClose)
                    .await?;
                let current = crate::db::sqlite::track_get_tx(tx, &track.id).await?;
                if let Some(closed_at) = current.closed_at {
                    let _ = already_closed_in_tx.set(closed_at);
                    return Err(CalmError::Conflict("track already closed".into()));
                }
                let closed = crate::db::sqlite::track_update_tx(
                    tx,
                    track.id.as_str(),
                    TrackPatch {
                        closed: Some(true),
                        ..TrackPatch::default()
                    },
                )
                .await?;
                let closed_at = closed
                    .closed_at
                    .ok_or_else(|| CalmError::Internal("a close left closed_at unset".into()))?;
                let event = Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                    closed,
                    Some(message),
                ));
                Ok((closed_at, vec![(scope, event)]))
            })
        },
    )
    .await;
    let closed = match written {
        Ok((closed_at, _)) => closed_at,
        Err(_) if let Some(closed_at) = already_closed.get() => *closed_at,
        Err(CalmError::Forbidden(msg)) => {
            return Err(RpcError::custom(
                -32403,
                format!("{TOOL_TRACK_CLOSE}: forbidden: {msg}"),
            ));
        }
        Err(e) => return Err(RpcError::internal(format!("{TOOL_TRACK_CLOSE}: {e}"))),
    };
    Ok(json!({ "closed_at": closed }))
}

/// A missing thread-mapped card while its daemon is active is a delete-while-active race, surfaced as `InternalError`.
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

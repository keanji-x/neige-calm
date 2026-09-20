//! Track-state tools: `calm.track.state` (Planner or Worker snapshot read, no event emission) and
//! `calm.task.verdict` (Planner-only accept/reject, lowered to `TaskCompleted` / `TaskFailed`, scoped to the caller's track).

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
use crate::mcp_server::tools::plan::TOOL_PLAN_CANCEL;
use crate::mcp_server::tools::track_report::{TOOL_REPORT_EDIT, TOOL_REPORT_WRITE};
use crate::model::{Card, CardRole, Track, TrackLifecycle};
use crate::track_lifecycle::planner_allowed_targets;
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

    let tasks_declared = ctx
        .repo
        .tasks_by_track(track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_state: tasks_by_track: {e}")))?
        .len();
    let next = planner_next_steps(track.lifecycle, tasks_declared);

    Ok(json!({
        "track": track,
        "cards": cards_json,
        "report_startup_read_required": report_startup_read_required(&cards),
        "tasks_declared": tasks_declared,
        "next": next,
    }))
}

/// `calm.task.verdict` and `calm.plan.cancel` need a declared task to act on, so a track with no tasks lists only the report tools.
fn planner_next_steps(current: TrackLifecycle, tasks_declared: usize) -> Vec<Value> {
    let mut via = vec![TOOL_REPORT_WRITE, TOOL_REPORT_EDIT];
    if tasks_declared > 0 {
        via.push(TOOL_TASK_VERDICT);
        via.push(TOOL_PLAN_CANCEL);
    }
    planner_allowed_targets(current)
        .into_iter()
        .map(|target| {
            json!({
                "lifecycle": target,
                "via": via,
                "note": planner_next_note(current, target),
            })
        })
        .collect()
}

fn planner_next_note(current: TrackLifecycle, target: TrackLifecycle) -> &'static str {
    use TrackLifecycle as L;
    match (current, target) {
        (L::Draft, L::Planning) => {
            "start planning (the kernel also does this on your first report write)"
        }
        (L::Planning, L::Reviewing) => {
            "deliverable ready for judgement (also the self-executed path when nothing was dispatched)"
        }
        (_, L::Reviewing) => "deliverable ready for judgement",
        (_, L::Dispatching) => "tasks declared; the kernel advances this itself when it claims one",
        (L::Blocked, L::Working) | (L::Reviewing, L::Working) => "resume: more work is needed",
        (_, L::Working) => "work underway; the kernel advances this itself when it claims a task",
        (_, L::Blocked) => "waiting on the user",
        (_, L::Done) => "conclude the track",
        (_, L::Failed) => "give up: the track cannot be completed",
        (_, L::Canceled) => "cancel (user-only)",
        (_, L::Draft) | (_, L::Planning) => "return to planning",
    }
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
        description: include_str!("../../../prompts/tools/calm.task.verdict.md")
            .trim_end()
            .to_string(),
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
            // Structured `{status, reason}` so a consumer can tell planner verdicts (`result.status == "accepted"`) apart from workers' free-form self-reports.
            result: json!({
                "status": "accepted",
                "reason": reason.unwrap_or_default(),
            }),
            artifacts: vec![],
            agent_message: Some(write_args.message.clone()),
        },
        "rejected" => Event::TaskFailed {
            idempotency_key,
            // An empty reason is a valid value; the verdict is not second-guessed.
            reason: reason.unwrap_or_default(),
            details: None,
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

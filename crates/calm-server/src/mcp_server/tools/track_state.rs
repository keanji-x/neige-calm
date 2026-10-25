//! Track-state tools: `neige_track_status` (Planner or Worker snapshot read, no event emission),
//! `neige_task_accept` / `neige_task_reject` (Planner-only verdicts, lowered to `TaskCompleted` / `TaskFailed`, scoped to the caller's track),
//! `neige_task_regate` (Planner-only re-run of a failed gate on the same candidate, #2405)
//! `neige_track_handback` (return an unmet child assignment), and `neige_track_close`
//! (own close, returned child, or an Admin closing a Track it added).

use crate::decision_sink::{CardDecisionSink, CardDecisionSinkRecorderAdmissionGate};
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, role_gated_write_annotations,
};
use crate::mcp_server::tools::track_file::resolve_track_for_identity;
use crate::mcp_server::tools::write_args::{message_schema, parse_write_args};
use crate::model::{Card, CardRole, Track, TrackPatch};
use crate::recorder_admission::{RecorderAdmissionDecisionKind, RecorderAdmissionGate};
use crate::track_report::TrackReportPayload;
use serde_json::{Value, json};
use std::sync::Arc;

mod handback;
pub use handback::TOOL_TRACK_HANDBACK;

pub const TOOL_TRACK_STATUS: &str = "neige_track_status";
pub const TOOL_TASK_ACCEPT: &str = "neige_task_accept";
pub const TOOL_TASK_REJECT: &str = "neige_task_reject";
pub const TOOL_TRACK_CLOSE: &str = "neige_track_close";
pub const TOOL_TASK_REGATE: &str = "neige_task_regate";

pub fn register_into(registry: &mut ToolRegistry) {
    handback::register_into(registry);
    registry.register(track_state_descriptor(), wrap(track_state));
    for verdict in [Verdict::Accept, Verdict::Reject] {
        registry.register(
            task_verdict_descriptor(verdict),
            wrap(move |ctx, identity, args| task_verdict(ctx, identity, args, verdict)),
        );
    }
    registry.register(task_regate_descriptor(), wrap(task_regate));
    registry.register(track_close_descriptor(), wrap(track_close));
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
        name: TOOL_TRACK_STATUS.into(),
        description: include_str!("../../../prompts/tools/neige_track_status.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        }),
        annotations: Some(read_only_annotations()),
        roles: &[CardRole::Planner, CardRole::Worker],
        listed_for: &[],
    }
}

async fn track_state(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    let (caller, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let mut cards = ctx
        .repo
        .cards_by_track(track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("neige_track_status: cards_by_track: {e}")))?;
    crate::session_projection_lookup::project_runtime_into_cards_payload(
        ctx.repo.as_ref(),
        &mut cards,
    )
    .await
    .map_err(|e| RpcError::internal(format!("neige_track_status: runtime projection: {e}")))?;

    // The role cache is the canonical source the role gate already trusts; `Card` doesn't carry `role` on the struct.
    let cards_json: Vec<Value> = cards
        .iter()
        .map(|c| {
            let role = ctx.write.verify_role(&c.id).unwrap_or_default();
            json!({
                "id": c.id,
                "kind": c.kind,
                "provider": c.runtime.as_ref().and_then(|runtime| runtime.provider.clone()),
                "role": role,
                "sort": c.sort,
                "created_at": crate::time_format::at(c.created_at),
                "updated_at": crate::time_format::at(c.updated_at),
                "runtime": c.runtime.as_ref().map_or(Value::Null, runtime_output),
            })
        })
        .collect();

    // `tasks_by_track` reads the `current_tasks` view: one row per key, its current execution only.
    let tasks: Vec<Value> = ctx
        .repo
        .tasks_by_track(track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("neige_track_status: tasks_by_track: {e}")))?
        .into_iter()
        .map(|task| {
            json!({
                "key": task.key,
                "status": task.status,
                "access": task.access,
                "start": task.start,
                "worker_card_id": task.worker_card_id,
            })
        })
        .collect();

    let creation = crate::managed_track::creation_identity(&ctx, track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let mut track_row = json!(track);
    crate::time_format::rewrite_at(
        &mut track_row,
        &["pinned_at", "closed_at", "created_at", "updated_at"],
    );
    crate::time_format::rewrite_at(&mut track_row["workspace"], &["frozen_at"]);
    let mut result = json!({
        "track": track_row,
        "caller_card_id": caller.id,
        "cards": cards_json,
        "report_startup_read_required": report_startup_read_required(&cards),
        "tasks": tasks,
    });
    if let Some(identity) = creation {
        result["creation_identity"] = identity;
    }
    Ok(result)
}

/// A card's runtime as the REST card serves it, with its two unix-ms times as `<event>_at`
/// RFC 3339 strings, always present and `null` when absent (agent-commands §4); the REST card
/// omits an absent one.
fn runtime_output(runtime: &crate::model::CardRuntimeView) -> Value {
    let mut out = json!(runtime);
    if let Some(object) = out.as_object_mut() {
        for (from, to) in [
            ("updated_at_ms", "updated_at"),
            ("last_turn_completed_ms", "last_turn_completed_at"),
        ] {
            let ms = object.remove(from).and_then(|ms| ms.as_i64());
            object.insert(to.into(), crate::time_format::at_opt(ms));
        }
    }
    out
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

/// The Planner's two decisions on an attempt; one verdict path serves both tools.
#[derive(Clone, Copy)]
enum Verdict {
    Accept,
    Reject,
}

impl Verdict {
    fn tool(self) -> &'static str {
        match self {
            Verdict::Accept => TOOL_TASK_ACCEPT,
            Verdict::Reject => TOOL_TASK_REJECT,
        }
    }

    fn keys(self) -> &'static [&'static str] {
        match self {
            Verdict::Accept => &["attempt_id", "message"],
            Verdict::Reject => &["attempt_id", "message", "reason"],
        }
    }
}

fn task_verdict_descriptor(verdict: Verdict) -> ToolDescriptor {
    let mut properties = json!({
        "attempt_id": { "type": "string", "minLength": 1 },
        "message": message_schema()
    });
    let description = match verdict {
        Verdict::Accept => calm_types::observation::render_task_acceptance_guidance(
            include_str!("../../../prompts/tools/neige_task_accept.md").trim_end(),
        ),
        Verdict::Reject => {
            properties["reason"] = json!({ "type": "string", "minLength": 1 });
            include_str!("../../../prompts/tools/neige_task_reject.md")
                .trim_end()
                .to_string()
        }
    };
    ToolDescriptor {
        name: verdict.tool().into(),
        description,
        input_schema: json!({
            "type": "object",
            "required": verdict.keys(),
            "additionalProperties": false,
            "properties": properties
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

async fn task_verdict(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
    verdict: Verdict,
) -> Result<Value, RpcError> {
    let tool = verdict.tool();
    let message = parse_write_args(&args, tool)?;
    let attempt_id = crate::mcp_server::tools::emit::required_attempt_id(&args, tool)?;

    let event = match verdict {
        Verdict::Accept => Event::TaskCompleted {
            notification: calm_types::event::PlannerNotification::Suppress,
            idempotency_key: attempt_id,
            // Structured `{status}` so a consumer can tell planner verdicts (`result.status == "accepted"`) apart from workers' free-form self-reports; the rationale is the `message`.
            result: json!({ "status": "accepted" }),
            artifacts: vec![],
            agent_message: Some(message.clone()),
        },
        Verdict::Reject => Event::TaskFailed {
            notification: calm_types::event::PlannerNotification::Suppress,
            idempotency_key: attempt_id,
            reason: args
                .get("reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.trim().is_empty())
                .ok_or_else(|| {
                    RpcError::invalid_params(format!("{tool}: missing `reason` (non-empty)"))
                })?
                .to_string(),
            details: None,
            agent_message: Some(message.clone()),
        },
    };

    let res = CardDecisionSink::from_app_context(&ctx)
        .commit_planner_verdict(&identity, event)
        .await;

    match res {
        Ok(_) => Ok(json!({ "ok": true })),
        Err(e) => Err(crate::mcp_server::framing::calm_error(e)),
    }
}

fn task_regate_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_REGATE.into(),
        description: include_str!("../../../prompts/tools/neige_task_regate.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["attempt_id", "message"],
            "additionalProperties": false,
            "properties": {
                "attempt_id": { "type": "string", "minLength": 1 },
                "message": message_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

/// Parses and resolves the caller's Track; admission and the transition belong to the task-verify
/// module (`regate_in_tx`), in this one transaction with the event.
async fn task_regate(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let message = parse_write_args(&args, TOOL_TASK_REGATE)?;
    let attempt_id = crate::mcp_server::tools::emit::required_attempt_id(&args, TOOL_TASK_REGATE)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let (regated, _) = crate::db::write_with_events_typed(
        ctx.repo.as_ref(),
        identity.to_actor_id(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let (regated, event) = crate::operation::task_verify_adapter::regate::regate_in_tx(
                    tx,
                    &track.id,
                    &attempt_id,
                    message,
                )
                .await?;
                Ok((regated, vec![(scope, event)]))
            })
        },
    )
    .await
    .map_err(crate::mcp_server::framing::calm_error)?;
    Ok(json!({
        "status": "verifying",
        "key": regated.key,
        "previous_gate_run": regated.previous_gate_attempt,
        "next_gate_run": regated.next_gate_attempt(),
    }))
}

fn track_close_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_CLOSE.into(),
        description: include_str!("../../../prompts/tools/neige_track_close.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["message"],
            "properties": {
                "message": message_schema(),
                "track_id": { "type": "string", "minLength": 1 }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

/// Resolve the caller and preflight the target before transactional admission and close.
async fn track_close(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let message = parse_write_args(&args, TOOL_TRACK_CLOSE)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let target_id = match args.get("track_id") {
        None => track.id.clone(),
        Some(Value::String(id)) if !id.trim().is_empty() => id.clone().into(),
        Some(_) => return Err(RpcError::invalid_params("track_id must be non-empty")),
    };
    let other = target_id != track.id;
    if other {
        // Ownership is the first target-specific read: foreign ids disclose no lifecycle facts.
        let target = ctx
            .repo
            .track_get(target_id.as_str())
            .await
            .map_err(|error| crate::mcp_server::framing::calm_error(error.into()))?;
        let pool = ctx
            .sqlite_db
            .as_ref()
            .ok_or_else(|| RpcError::internal("close needs sqlite"))?;
        let is_parent: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tracks WHERE id=?1 AND parent_track_id=?2)",
        )
        .bind(target_id.as_str())
        .bind(track.id.as_str())
        .fetch_one(pool.read_pool())
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
        if !is_parent
            && !target
                .as_ref()
                .is_some_and(|target| added_by(target, &track))
        {
            return Err(close_refusal(-32404, "unknown_track", json!({})));
        }
        let pool = ctx
            .sqlite_db
            .as_ref()
            .ok_or_else(|| RpcError::internal("close needs sqlite"))?;
        // Defensive preflight only; the forge operation's parked recovery remains the backstop.
        if crate::operation::workspace_lease::track_has_active_forge_action(
            pool.read_pool(),
            target_id.as_str(),
        )
        .await
        .map_err(crate::mcp_server::framing::calm_error)?
        {
            return Err(RpcError::conflict("target has an in-flight forge-action"));
        }
    } else if crate::managed_track::kernel_controls_lifecycle(&ctx, track.id.as_str())
        .await
        .map_err(crate::mcp_server::framing::calm_error)?
    {
        return Err(RpcError::forbidden(
            "The kernel controls this Track’s lifecycle.",
        ));
    }
    let recorder = CardDecisionSinkRecorderAdmissionGate::for_identity(&identity, track.id.clone());
    // The write adapter transports storage errors; preserve typed refusals across its rollback.
    let refusal = Arc::new(std::sync::OnceLock::<CloseError>::new());
    let refusal_in_tx = refusal.clone();
    let written = crate::db::write_with_events_typed(
        ctx.repo.as_ref(),
        identity.to_actor_id(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let result =
                    close_authorized_tx(tx, recorder, track, target_id, other, message).await;
                result.map_err(|error| match error {
                    CloseError::Storage(error) => error,
                    error => {
                        let _ = refusal_in_tx.set(error);
                        CalmError::Conflict("close refused".into())
                    }
                })
            })
        },
    )
    .await;
    let closed = match written {
        Ok((closed_at, _)) => closed_at,
        Err(error) => match refusal.get() {
            Some(CloseError::AlreadyClosed(closed_at)) => *closed_at,
            Some(CloseError::Refusal(error)) => return Err(error.clone()),
            _ => return Err(crate::mcp_server::framing::calm_error(error)),
        },
    };
    Ok(json!({ "closed_at": crate::time_format::at(closed) }))
}

/// Admission and the recorder share the write transaction; only the caller supplies authority.
async fn close_authorized_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    recorder: CardDecisionSinkRecorderAdmissionGate,
    caller: Track,
    target_id: crate::ids::TrackId,
    other: bool,
    message: String,
) -> Result<(i64, Vec<(EventScope, Event)>), CloseError> {
    let detached_child: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tracks child WHERE child.id=?1 AND child.parent_track_id=?2 \
         AND NOT EXISTS(SELECT 1 FROM current_tasks task WHERE task.track_id=?2 \
           AND task.child_track_id=child.id AND task.status IN ('dispatched','running')))",
    )
    .bind(target_id.as_str())
    .bind(caller.id.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(CalmError::from)?;
    if other && !detached_child {
        crate::area_admin::require_admin_tx(tx, caller.id.as_str())
            .await
            .map_err(|error| match error {
                crate::area_admin::RequireAdminError::NotAdmin => {
                    CloseError::Refusal(close_refusal(-32403, "not_admin", json!({})))
                }
                crate::area_admin::RequireAdminError::Storage(error) => CloseError::Storage(error),
            })?;
    }
    // Always bind the recorder to the caller, never the close target.
    recorder
        .require_admission(tx, RecorderAdmissionDecisionKind::TrackClose)
        .await?;
    let target = calm_truth::db::sqlite::track_get_tx(tx, &target_id)
        .await
        .map_err(CalmError::from);
    let target = if other {
        match target {
            Ok(target) if detached_child || added_by(&target, &caller) => target,
            Ok(_) | Err(CalmError::NotFound(_)) => {
                return Err(CloseError::Refusal(close_refusal(
                    -32404,
                    "unknown_track",
                    json!({}),
                )));
            }
            Err(error) => return Err(error.into()),
        }
    } else {
        target?
    };
    if other && target.closed_at.is_none() {
        use sqlx::Row;
        let sql = format!(
            "SELECT key, status FROM current_tasks WHERE track_id=?1 AND status IN {} ORDER BY key",
            crate::planner_hibernation::RUNNING_WORK_STATUSES_SQL
        );
        let active: Vec<Value> = sqlx::query(&sql)
            .bind(target.id.as_str())
            .fetch_all(&mut **tx)
            .await
            .map_err(CalmError::from)?
            .into_iter()
            .map(|row| {
                json!({
                    "key": row.get::<String,_>("key"), "status": row.get::<String,_>("status")
                })
            })
            .collect();
        if !active.is_empty() {
            return Err(CloseError::Refusal(close_refusal(
                -32409,
                "active_work",
                json!({"active": active}),
            )));
        }
    }
    close_tx(tx, target, message).await
}

enum CloseError {
    Storage(CalmError),
    Refusal(RpcError),
    AlreadyClosed(i64),
}

impl From<CalmError> for CloseError {
    fn from(error: CalmError) -> Self {
        Self::Storage(error)
    }
}

fn added_by(target: &Track, caller: &Track) -> bool {
    target.creator_track_id.as_deref() == Some(caller.id.as_str())
}

fn close_refusal(code: i64, refusal: &str, mut data: Value) -> RpcError {
    data["refusal"] = json!(refusal);
    RpcError {
        code,
        message: if refusal == "not_admin" {
            "not_admin: ask with neige_user_ask action grant_admin".into()
        } else {
            refusal.into()
        },
        data: Some(data),
    }
}

/// The shared close writer. An idempotent close rolls back even the recorder's writes.
async fn close_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target: Track,
    message: String,
) -> Result<(i64, Vec<(EventScope, Event)>), CloseError> {
    if let Some(closed_at) = target.closed_at {
        return Err(CloseError::AlreadyClosed(closed_at));
    }
    let scope = EventScope::Track {
        track: target.id.clone(),
        area: target.area_id.clone(),
    };
    let closed = crate::db::sqlite::track_update_tx(
        tx,
        target.id.as_str(),
        TrackPatch {
            closed: Some(true),
            ..TrackPatch::default()
        },
    )
    .await
    .map_err(CalmError::from)?;
    let closed_at = closed
        .closed_at
        .ok_or_else(|| CalmError::Internal("a close left closed_at unset".into()))?;
    let event = Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
        closed,
        Some(message),
    ));
    Ok((closed_at, vec![(scope, event)]))
}

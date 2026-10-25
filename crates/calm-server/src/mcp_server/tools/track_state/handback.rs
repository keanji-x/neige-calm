//! Explicit return of an unmet child assignment; admission, guard and event share one writer.
use super::*;

pub const TOOL_TRACK_HANDBACK: &str = "neige_track_handback";

pub(super) fn register_into(registry: &mut ToolRegistry) {
    registry.register(
        ToolDescriptor {
            name: TOOL_TRACK_HANDBACK.into(),
            description: include_str!("../../../../prompts/tools/neige_track_handback.md")
                .trim_end()
                .into(),
            input_schema: json!({"type":"object","additionalProperties":false,
            "required":["blocked_on"],"properties":{"blocked_on":{"type":"string","minLength":1}}}),
            annotations: Some(role_gated_write_annotations()),
            roles: &[CardRole::Planner],
            listed_for: &[CardRole::Planner],
        },
        wrap(handback),
    );
}

async fn handback(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let blocked_on = args
        .get("blocked_on")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("blocked_on must be non-empty"))?
        .to_owned();
    let (_, child) = resolve_track_for_identity(&ctx, &identity).await?;
    let recorder = CardDecisionSinkRecorderAdmissionGate::for_identity(&identity, child.id.clone());
    let refusal = Arc::new(std::sync::OnceLock::new());
    let refusal_in_tx = refusal.clone();
    // The child's recorder authorizes the request. The resulting parent failure is a kernel
    // lifecycle event, like scheduler success, rather than a parent Planner verdict.
    let written = crate::db::write_with_events_typed(
        ctx.repo.as_ref(),
        crate::ids::ActorId::KernelDispatcher,
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let result = handback_tx(tx, recorder, child, blocked_on).await;
                result.map_err(|error| match error {
                    CloseError::Storage(error) => error,
                    CloseError::Refusal(error) => {
                        let _ = refusal_in_tx.set(error);
                        CalmError::Conflict("handback refused".into())
                    }
                    CloseError::AlreadyClosed(_) => CalmError::Conflict("child is closed".into()),
                })
            })
        },
    )
    .await;
    match written {
        Ok((result, _)) => Ok(result),
        Err(error) => Err(refusal
            .get()
            .cloned()
            .unwrap_or_else(|| crate::mcp_server::framing::calm_error(error))),
    }
}

async fn handback_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    recorder: CardDecisionSinkRecorderAdmissionGate,
    child: Track,
    blocked_on: String,
) -> Result<(Value, Vec<(EventScope, Event)>), CloseError> {
    recorder
        .require_admission(tx, RecorderAdmissionDecisionKind::TrackHandback)
        .await?;
    let binding: Option<(String, String, String)> = sqlx::query_as(
        "SELECT task.id,parent.id,parent.area_id FROM tracks child \
         JOIN current_tasks task ON task.child_track_id=child.id AND task.track_id=child.parent_track_id \
         JOIN tracks parent ON parent.id=task.track_id \
         WHERE child.id=?1 AND child.closed_at IS NULL AND task.status IN ('dispatched','running')")
        .bind(child.id.as_str()).fetch_optional(&mut **tx).await.map_err(CalmError::from)?;
    let Some((attempt_id, parent_id, area_id)) = binding else {
        return Err(CloseError::Refusal(RpcError::conflict(
            "no active parent assignment",
        )));
    };
    let inflight_sql = format!(
        "SELECT EXISTS(SELECT 1 FROM current_tasks WHERE track_id=?1 AND status IN {})",
        crate::planner_hibernation::RUNNING_WORK_STATUSES_SQL
    );
    let inflight: bool = sqlx::query_scalar(&inflight_sql)
        .bind(child.id.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(CalmError::from)?;
    if inflight {
        return Err(CloseError::Refusal(close_refusal(
            -32409,
            "active_work",
            json!({}),
        )));
    }
    let result =
        crate::child_track_result::result_ref_tx(tx, child.id.as_str(), Some(&blocked_on)).await?;
    let now = crate::model::now_ms();
    let guard_sql = format!(
        "UPDATE tasks SET status='failed',status_detail='child-track-handed-back', \
        worker_card_id=NULL,running_deadline_ms=NULL,updated_at_ms=?1,finished_at_ms=?1 \
        WHERE id=?2 AND track_id=?3 AND child_track_id=?4 AND status IN ('dispatched','running') \
        AND EXISTS(SELECT 1 FROM tracks child WHERE child.id=?4 AND child.parent_track_id=?3 AND child.closed_at IS NULL) \
        AND NOT EXISTS(SELECT 1 FROM current_tasks WHERE track_id=?4 AND status IN {})",
        crate::planner_hibernation::RUNNING_WORK_STATUSES_SQL
    );
    let changed = sqlx::query(&guard_sql)
        .bind(now)
        .bind(&attempt_id)
        .bind(&parent_id)
        .bind(child.id.as_str())
        .execute(&mut **tx)
        .await
        .map_err(CalmError::from)?
        .rows_affected();
    if changed != 1 {
        return Err(CloseError::Refusal(RpcError::conflict(
            "parent assignment changed",
        )));
    }
    let event = Event::TaskFailed {
        notification: calm_types::event::PlannerNotification::Notify,
        idempotency_key: attempt_id,
        reason: blocked_on,
        details: Some(result.clone()),
        agent_message: None,
    };
    Ok((
        result,
        vec![(
            EventScope::Track {
                track: parent_id.into(),
                area: area_id.into(),
            },
            event,
        )],
    ))
}

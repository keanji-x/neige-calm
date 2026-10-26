//! Child Planner outcome admission and atomic settlement. Worker reports keep their own sink.
use super::*;

pub(in crate::mcp_server::tools) async fn report(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    requested_attempt_id: String,
    blocked_on: Option<String>,
) -> Result<Value, RpcError> {
    let (_, child) = resolve_track_for_identity(&ctx, &identity).await?;
    let recorder = CardDecisionSinkRecorderAdmissionGate::for_identity(&identity, child.id.clone());
    let refusal = Arc::new(std::sync::OnceLock::new());
    let refusal_in_tx = refusal.clone();
    // The child's recorder authorizes the request. Parent outcomes are kernel lifecycle
    // events, like scheduler recovery, rather than parent Planner verdicts.
    let written = crate::db::write_with_events_typed(
        ctx.repo.as_ref(),
        crate::ids::ActorId::KernelDispatcher,
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let result = report_tx(tx, recorder, child, requested_attempt_id, blocked_on).await;
                result.map_err(|error| match error {
                    CloseError::Storage(error) => error,
                    CloseError::Refusal(error) => {
                        let _ = refusal_in_tx.set(error);
                        CalmError::Conflict("child report refused".into())
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

async fn report_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    recorder: CardDecisionSinkRecorderAdmissionGate,
    child: Track,
    requested_attempt_id: String,
    blocked_on: Option<String>,
) -> Result<(Value, Vec<(EventScope, Event)>), CloseError> {
    recorder
        .require_admission(tx, RecorderAdmissionDecisionKind::ChildTaskReport)
        .await?;
    let binding: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT task.id,parent.id,parent.area_id,task.gate_json FROM tracks child \
         JOIN current_tasks task ON task.child_track_id=child.id AND task.track_id=child.parent_track_id \
         JOIN tracks parent ON parent.id=task.track_id \
         WHERE child.id=?1 AND child.closed_at IS NULL AND task.status IN ('dispatched','running')")
        .bind(child.id.as_str()).fetch_optional(&mut **tx).await.map_err(CalmError::from)?;
    let Some((attempt_id, parent_id, area_id, gate_json)) = binding else {
        return Err(CloseError::Refusal(RpcError::conflict(
            "no active parent assignment",
        )));
    };
    if requested_attempt_id != attempt_id {
        return Err(CloseError::Refusal(RpcError::conflict(
            "attempt is not the active child assignment",
        )));
    }
    if blocked_on.is_none() {
        let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM current_tasks WHERE track_id=?1 AND status IN ('pending','dispatched','running','verifying'))")
            .bind(child.id.as_str()).fetch_one(&mut **tx).await.map_err(CalmError::from)?;
        if pending {
            return Err(CloseError::Refusal(close_refusal(
                -32409,
                "active_work",
                json!({}),
            )));
        }
        let (_, mut events) = close_tx(tx, child.clone(), "Parent assignment met".into()).await?;
        let event = crate::child_track_result::success_tx(
            tx,
            &attempt_id,
            child.id.as_str(),
            gate_json.is_some(),
        )
        .await?
        .ok_or_else(|| CloseError::Refusal(RpcError::conflict("parent assignment changed")))?;
        let Event::TaskCompleted { ref result, .. } = event else {
            unreachable!()
        };
        let result = result.clone();
        events.push((
            EventScope::Track {
                track: parent_id.into(),
                area: area_id.into(),
            },
            event,
        ));
        return Ok((result, events));
    }
    let blocked_on = blocked_on.expect("failure reason checked by emit");
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

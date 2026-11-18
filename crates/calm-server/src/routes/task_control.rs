//! Authenticated User control of the current native Worker; MCP retains its Planner admission.
use crate::auth::{HumanInputSource, Principal};
use crate::extract::{Json, JsonBody, Path};
use crate::{
    actor::Actor,
    error::{CalmError, ErrorBody, Result},
    event::{Event, EventScope},
    ids::ActorId,
    model::{Task, TaskStatus},
    operation::Tx,
    state::{AppState, RouteState},
};
use axum::{Router, extract::State, routing::post};

use calm_types::task_recovery::{
    TaskCancelRequest, TaskCancelResponse, TaskMessageRequest, TaskMessageResponse,
};
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks/{id}/tasks/{key}/messages", post(message))
        .route("/api/tracks/{id}/tasks/{key}/cancel", post(cancel))
}
async fn current_user_task_tx(
    tx: &mut Tx<'_>,
    track: &str,
    key: &str,
    attempt: &str,
) -> Result<Task> {
    calm_truth::decision_gate::PrincipalDecisionGate::new(calm_types::worker::Principal::User)
        .decide_user_task_input(tx, &track.into())
        .await?
        .into_result()?;
    let row = crate::db::sqlite::track_get_tx(tx, &track.into()).await?;
    if !row.is_open() {
        return Err(CalmError::Conflict("track is closed".into()));
    }
    let task = crate::db::sqlite::task_current_get_tx(tx, track, key)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("task {key}")))?;
    if task.id != attempt {
        return Err(CalmError::Conflict(
            "current execution changed; refresh task attempts".into(),
        ));
    }
    if !crate::task_control::is_native(&task)? {
        return Err(CalmError::Conflict(
            "task has no native Worker input transport".into(),
        ));
    }
    Ok(task)
}
#[utoipa::path(post,path="/api/tracks/{id}/tasks/{key}/messages",params(("id"=String,Path),("key"=String,Path)),request_body=TaskMessageRequest,responses((status=200,body=TaskMessageResponse),(status=400,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=500,body=ErrorBody)),tag="tracks")]
pub(crate) async fn message(
    State(s): State<RouteState>,
    _principal: Principal,
    source: HumanInputSource,
    actor: Actor,
    Path((track, key)): Path<(String, String)>,
    JsonBody(body): JsonBody<TaskMessageRequest>,
) -> Result<Json<TaskMessageResponse>> {
    source.current_human_principal()?;
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "task message",
        "Use the authenticated User channel.",
    )?;
    let repo = s.repo.clone();
    let repo_for_tx = repo.clone();
    let (message_id,_)=crate::task_control::write_worker_events(repo.as_ref(),None,&s.events,&s.write,move|tx|Box::pin(async move{
        let task=current_user_task_tx(tx,&track,&key,&body.attempt_id).await?;
        let prior:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM task_planner_messages WHERE attempt_id=?1 AND idempotency_key=?2)").bind(&task.id).bind(&body.idempotency_key).fetch_one(&mut **tx).await?;
        let message=crate::task_control::message_tx(repo_for_tx,tx,&task,&body.idempotency_key,&body.text,calm_types::track_fs_dto::TaskMessageSource::User).await?;
        let events=if prior{vec![]}else{vec![(ActorId::User,EventScope::Track{track:track.clone().into(),area:crate::db::sqlite::track_get_tx(tx,&track.clone().into()).await?.area_id},Event::PlanUpdated{track_id:track.into(),changed_keys:vec![key],agent_message:Some("User message queued for native Worker".into())})]};
        Ok((message,events))
    })).await?;
    Ok(Json(TaskMessageResponse { message_id }))
}
#[utoipa::path(post,path="/api/tracks/{id}/tasks/{key}/cancel",operation_id="cancel_task_worker",params(("id"=String,Path),("key"=String,Path)),request_body=TaskCancelRequest,responses((status=200,body=TaskCancelResponse),(status=400,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=500,body=ErrorBody)),tag="tracks")]
pub(crate) async fn cancel(
    State(s): State<RouteState>,
    _principal: Principal,
    source: HumanInputSource,
    actor: Actor,
    Path((track, key)): Path<(String, String)>,
    JsonBody(body): JsonBody<TaskCancelRequest>,
) -> Result<Json<TaskCancelResponse>> {
    source.current_human_principal()?;
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "task cancel",
        "Use the authenticated User channel.",
    )?;
    // The spawned write owns post-commit fencing even if the browser disconnects.
    tokio::spawn(async move {
        let repo = s.repo.clone();
        let repo_for_tx = repo.clone();
        let (captured, _) = crate::task_control::write_worker_events(
            repo.as_ref(),
            None,
            &s.events,
            &s.write,
            move |tx| {
                Box::pin(async move {
                    let task = current_user_task_tx(tx, &track, &key, &body.attempt_id).await?;
                    if task.status == TaskStatus::Canceled {
                        return Ok((None, vec![]));
                    }
                    crate::task_control::admit_native_tx(repo_for_tx, tx, &task).await?;
                    let captured =
                        crate::task_control::capture_worker_identity_tx(tx, &task).await?;
                    let (rows, mut events) =
                        crate::task_control::cancel::cancel_running_in_tx(tx, &task, &key).await?;
                    if rows > 0 {
                        events.push((
                            ActorId::User,
                            EventScope::Track {
                                track: track.clone().into(),
                                area: crate::db::sqlite::track_get_tx(tx, &track.clone().into())
                                    .await?
                                    .area_id,
                            },
                            Event::PlanUpdated {
                                track_id: track.into(),
                                changed_keys: vec![key],
                                agent_message: Some("User canceled native Worker".into()),
                            },
                        ));
                    }
                    Ok((captured, events))
                })
            },
        )
        .await?;
        if let Some((session, turn)) = captured {
            s.operation_runtime
                .fence_worker_input_identity(&session, turn.as_deref())
                .await?;
        }
        if let Some(poke) = s.mcp_context.scheduler_poke.get() {
            poke.poke_worker_cleanups();
        }
        Ok::<_, CalmError>(())
    })
    .await
    .map_err(|e| CalmError::Internal(format!("task cancel failed: {e}")))??;
    Ok(Json(TaskCancelResponse { ok: true }))
}

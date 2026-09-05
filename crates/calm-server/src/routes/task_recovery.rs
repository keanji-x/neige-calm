//! Authenticated User access to continuing task executions.
use crate::actor::Actor;
use crate::auth::Principal;
use crate::error::{ErrorBody, Result};
use crate::ids::ActorId;
use crate::state::{AppState, RouteState};
use crate::task_recovery::{
    RecoveryContext, TaskRecoveryView, recover_failed_task, task_recovery_view,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use calm_types::task_recovery::{TaskRecoveryReceipt, TaskRecoveryRequest};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks/{id}/tasks/{key}/attempts", get(get_attempts))
        .route("/api/tracks/{id}/tasks/{key}/recover", post(recover))
}

#[utoipa::path(get, path = "/api/tracks/{id}/tasks/{key}/attempts",
    params(("id" = String, Path), ("key" = String, Path)),
    responses((status = 200, body = TaskRecoveryView), (status = 404, body = ErrorBody)), tag = "tracks")]
pub async fn get_attempts(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((track_id, key)): Path<(String, String)>,
) -> Result<Json<TaskRecoveryView>> {
    super::track_report_blocks::require_rest_user_actor(&actor)?;
    Ok(Json(
        task_recovery_view(state.repo.as_ref(), &track_id, &key, ActorId::User).await?,
    ))
}

#[utoipa::path(post, path = "/api/tracks/{id}/tasks/{key}/recover",
    params(("id" = String, Path), ("key" = String, Path)), request_body = TaskRecoveryRequest,
    responses((status = 200, body = TaskRecoveryReceipt), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)), tag = "tracks")]
pub async fn recover(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((track_id, key)): Path<(String, String)>,
    Json(request): Json<TaskRecoveryRequest>,
) -> Result<Json<TaskRecoveryReceipt>> {
    super::track_report_blocks::require_rest_user_actor(&actor)?;
    Ok(Json(
        recover_failed_task(
            RecoveryContext {
                repo: state.repo.as_ref(),
                events: &state.events,
                write: &state.write,
            },
            &track_id,
            &key,
            request,
            ActorId::User,
        )
        .await?,
    ))
}

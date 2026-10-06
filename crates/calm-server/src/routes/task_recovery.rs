//! Authenticated User access to continuing task executions.
use crate::actor::Actor;
use crate::auth::Principal;
use crate::error::{ErrorBody, Result};
use crate::state::{AppState, RouteState};
use crate::task_recovery::{TaskRecoveryView, task_recovery_view};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/tracks/{id}/tasks/{key}/attempts", get(get_attempts))
}

#[utoipa::path(get, path = "/api/tracks/{id}/tasks/{key}/attempts",
    params(("id" = String, Path), ("key" = String, Path)),
    responses(
        (status = 200, body = TaskRecoveryView),
        (status = 403, body = ErrorBody, description = "Only `X-Calm-Actor: user` may read task attempts"),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The task's declaration is ambiguous or invalid (`conflict`)"),
        (status = 500, body = ErrorBody),
    ), tag = "tracks")]
pub async fn get_attempts(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((track_id, key)): Path<(String, String)>,
) -> Result<Json<TaskRecoveryView>> {
    super::track_report_blocks::require_rest_user_actor(&actor)?;
    Ok(Json(
        task_recovery_view(state.repo.as_ref(), &track_id, &key).await?,
    ))
}

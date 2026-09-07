//! Bounded User read of one file named by an exact accepted task report.
use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::write_in_tx_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::isolated_codex::files;
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/tracks/{id}/tasks/{key}/attempts/{attempt_id}/artifacts/{index}",
        get(get_file).layer(axum::middleware::from_fn(file_headers)),
    )
}
async fn file_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactFileResponse {
    pub attempt_id: String,
    pub index: usize,
    pub name: String,
    pub size: usize,
    pub content_base64: String,
}

#[utoipa::path(get, path = "/api/tracks/{id}/tasks/{key}/attempts/{attempt_id}/artifacts/{index}", tag = "tracks",
    params(("id" = String, Path), ("key" = String, Path), ("attempt_id" = String, Path), ("index" = usize, Path)),
    responses((status = 200, body = TaskArtifactFileResponse), (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)))]
pub async fn get_file(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((track_id, key, attempt_id, index)): Path<(String, String, String, usize)>,
) -> Result<Json<TaskArtifactFileResponse>> {
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "Read reported task file",
        "Task files are available to the person.",
    )?;
    let response_attempt = attempt_id.clone();
    let (snapshot, relative) = write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let report =
                super::isolated_tasks::accepted_report_tx(tx, &track_id, &key, &attempt_id).await?;
            let Some(super::isolated_tasks::AcceptedTaskReport::Completed { artifacts, .. }) =
                report
            else {
                return Err(CalmError::NotFound(
                    "Completed task artifact is unavailable.".into(),
                ));
            };
            let reference = artifacts.get(index).ok_or_else(|| {
                CalmError::NotFound("Reported task artifact index is unavailable.".into())
            })?;
            let relative = files::relative_reference(reference)?;
            let snapshot = files::snapshot_tx(tx, &track_id, &key, &attempt_id).await?;
            Ok((snapshot, relative))
        })
    })
    .await?;
    let directory = tokio::task::spawn_blocking(move || snapshot.open())
        .await
        .map_err(|_| CalmError::Conflict("Task workspace ownership is unavailable.".into()))??;
    let bytes = files::read(directory, &relative).await?;
    let name = relative
        .rsplit('/')
        .next()
        .expect("validated nonempty reference")
        .to_string();
    Ok(Json(TaskArtifactFileResponse {
        attempt_id: response_attempt,
        index,
        name,
        size: bytes.len(),
        content_base64: STANDARD.encode(bytes),
    }))
}

//! `/api/waves`, `/api/coves/:id/waves` — Wave CRUD. **Owned by Track B.**

use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::model::{NewWave, Wave, WaveDetail, WavePatch};
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/waves", axum::routing::post(create_wave))
        .route(
            "/api/waves/{id}",
            get(get_wave_detail).patch(update_wave).delete(delete_wave),
        )
        .route("/api/coves/{cove_id}/waves", get(list_waves_by_cove))
}

#[utoipa::path(
    get,
    path = "/api/coves/{cove_id}/waves",
    tag = "waves",
    params(("cove_id" = String, Path, description = "Cove id")),
    responses(
        (status = 200, description = "Waves under cove", body = Vec<Wave>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_waves_by_cove(
    State(s): State<AppState>,
    Path(cove_id): Path<String>,
) -> Result<Json<Vec<Wave>>> {
    let waves = s.repo.waves_by_cove(&cove_id).await?;
    Ok(Json(waves))
}

#[utoipa::path(
    get,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    responses(
        (status = 200, description = "Wave detail (wave + its cards + overlays)", body = WaveDetail),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_wave_detail(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<WaveDetail>> {
    let detail = s
        .repo
        .wave_detail(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    Ok(Json(detail))
}

#[utoipa::path(
    post,
    path = "/api/waves",
    tag = "waves",
    request_body = NewWave,
    responses(
        (status = 201, description = "Wave created", body = Wave),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn create_wave(
    State(s): State<AppState>,
    Json(p): Json<NewWave>,
) -> Result<(StatusCode, Json<Wave>)> {
    let wave = s.repo.wave_create(p).await?;
    s.events.emit(Event::WaveUpdated(wave.clone()));
    Ok((StatusCode::CREATED, Json(wave)))
}

#[utoipa::path(
    patch,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    request_body = WavePatch,
    responses(
        (status = 200, description = "Wave updated", body = Wave),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_wave(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<WavePatch>,
) -> Result<Json<Wave>> {
    let wave = s.repo.wave_update(&id, p).await?;
    s.events.emit(Event::WaveUpdated(wave.clone()));
    Ok(Json(wave))
}

#[utoipa::path(
    delete,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    responses(
        (status = 204, description = "Wave deleted"),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_wave(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Look up first so we know the cove_id for the delete event.
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    s.repo.wave_delete(&id).await?;
    s.events.emit(Event::WaveDeleted {
        id: wave.id,
        cove_id: wave.cove_id,
    });
    Ok(StatusCode::NO_CONTENT)
}

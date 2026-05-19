//! `/api/waves`, `/api/coves/:id/waves` — Wave CRUD. **Owned by Track B.**

use crate::error::{CalmError, Result};
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
        .route("/api/waves", axum::routing::post(create))
        .route(
            "/api/waves/{id}",
            get(detail).patch(update).delete(delete_),
        )
        .route("/api/coves/{cove_id}/waves", get(list_by_cove))
}

async fn list_by_cove(
    State(s): State<AppState>,
    Path(cove_id): Path<String>,
) -> Result<Json<Vec<Wave>>> {
    let waves = s.repo.waves_by_cove(&cove_id).await?;
    Ok(Json(waves))
}

async fn detail(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<WaveDetail>> {
    let detail = s
        .repo
        .wave_detail(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    Ok(Json(detail))
}

async fn create(
    State(s): State<AppState>,
    Json(p): Json<NewWave>,
) -> Result<(StatusCode, Json<Wave>)> {
    let wave = s.repo.wave_create(p).await?;
    s.events.emit(Event::WaveUpdated(wave.clone()));
    Ok((StatusCode::CREATED, Json(wave)))
}

async fn update(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<WavePatch>,
) -> Result<Json<Wave>> {
    let wave = s.repo.wave_update(&id, p).await?;
    s.events.emit(Event::WaveUpdated(wave.clone()));
    Ok(Json(wave))
}

async fn delete_(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
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

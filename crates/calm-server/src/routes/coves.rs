//! `/api/coves` — Cove CRUD. **Owned by Track B.**
//!
//! After each successful mutation, emit the matching `Event` via
//! `state.events.emit(...)` so the WS bus can fan out.

use crate::error::Result;
use crate::event::Event;
use crate::model::{Cove, CovePatch, NewCove};
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/coves", get(list).post(create))
        .route(
            "/api/coves/{id}",
            axum::routing::patch(update).delete(delete_),
        )
}

async fn list(State(s): State<AppState>) -> Result<Json<Vec<Cove>>> {
    let coves = s.repo.coves_list().await?;
    Ok(Json(coves))
}

async fn create(
    State(s): State<AppState>,
    Json(p): Json<NewCove>,
) -> Result<(StatusCode, Json<Cove>)> {
    let cove = s.repo.cove_create(p).await?;
    s.events.emit(Event::CoveUpdated(cove.clone()));
    Ok((StatusCode::CREATED, Json(cove)))
}

async fn update(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<CovePatch>,
) -> Result<Json<Cove>> {
    let cove = s.repo.cove_update(&id, p).await?;
    s.events.emit(Event::CoveUpdated(cove.clone()));
    Ok(Json(cove))
}

async fn delete_(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.repo.cove_delete(&id).await?;
    s.events.emit(Event::CoveDeleted { id });
    Ok(StatusCode::NO_CONTENT)
}

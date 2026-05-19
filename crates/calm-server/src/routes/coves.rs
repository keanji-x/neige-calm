//! `/api/coves` — Cove CRUD. **Owned by Track B.**
//!
//! After each successful mutation, emit the matching `Event` via
//! `state.events.emit(...)` so the WS bus can fan out.

use crate::error::{ErrorBody, Result};
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
        .route("/api/coves", get(list_coves).post(create_cove))
        .route(
            "/api/coves/{id}",
            axum::routing::patch(update_cove).delete(delete_cove),
        )
}

#[utoipa::path(
    get,
    path = "/api/coves",
    tag = "coves",
    responses(
        (status = 200, description = "List all coves", body = Vec<Cove>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_coves(State(s): State<AppState>) -> Result<Json<Vec<Cove>>> {
    let coves = s.repo.coves_list().await?;
    Ok(Json(coves))
}

#[utoipa::path(
    post,
    path = "/api/coves",
    tag = "coves",
    request_body = NewCove,
    responses(
        (status = 201, description = "Cove created", body = Cove),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn create_cove(
    State(s): State<AppState>,
    Json(p): Json<NewCove>,
) -> Result<(StatusCode, Json<Cove>)> {
    let cove = s.repo.cove_create(p).await?;
    s.events.emit(Event::CoveUpdated(cove.clone()));
    Ok((StatusCode::CREATED, Json(cove)))
}

#[utoipa::path(
    patch,
    path = "/api/coves/{id}",
    tag = "coves",
    params(("id" = String, Path, description = "Cove id")),
    request_body = CovePatch,
    responses(
        (status = 200, description = "Cove updated", body = Cove),
        (status = 404, description = "Cove not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_cove(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<CovePatch>,
) -> Result<Json<Cove>> {
    let cove = s.repo.cove_update(&id, p).await?;
    s.events.emit(Event::CoveUpdated(cove.clone()));
    Ok(Json(cove))
}

#[utoipa::path(
    delete,
    path = "/api/coves/{id}",
    tag = "coves",
    params(("id" = String, Path, description = "Cove id")),
    responses(
        (status = 204, description = "Cove deleted"),
        (status = 404, description = "Cove not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_cove(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    s.repo.cove_delete(&id).await?;
    s.events.emit(Event::CoveDeleted { id });
    Ok(StatusCode::NO_CONTENT)
}

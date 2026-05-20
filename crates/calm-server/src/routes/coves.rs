//! `/api/coves` — Cove CRUD. **Owned by Track B.**
//!
//! Writes go through `Repo::write_with_event` (via the
//! `write_with_event_typed` ergonomic wrapper). The wrapper atomically
//! commits the entity write + the events-table insert, then broadcasts a
//! `BroadcastEnvelope { id, event }` on the bus. Handler-level `events.emit`
//! calls are gone after Scope A; see `docs/sync-engine-design.md` §3.

use crate::actor::Actor;
use crate::db::sqlite::{cove_create_tx, cove_delete_tx, cove_update_tx};
use crate::db::write_with_event_typed;
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
    actor: Actor,
    Json(p): Json<NewCove>,
) -> Result<(StatusCode, Json<Cove>)> {
    let (cove, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.as_str(),
        None,
        &s.events,
        move |tx| {
            Box::pin(async move {
                let cove = cove_create_tx(tx, p).await?;
                Ok((cove.clone(), Event::CoveUpdated(cove)))
            })
        },
    )
    .await?;
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
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<CovePatch>,
) -> Result<Json<Cove>> {
    let (cove, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.as_str(),
        None,
        &s.events,
        move |tx| {
            Box::pin(async move {
                let cove = cove_update_tx(tx, &id, p).await?;
                Ok((cove.clone(), Event::CoveUpdated(cove)))
            })
        },
    )
    .await?;
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
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.as_str(),
        None,
        &s.events,
        move |tx| {
            Box::pin(async move {
                cove_delete_tx(tx, &id).await?;
                Ok(((), Event::CoveDeleted { id }))
            })
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

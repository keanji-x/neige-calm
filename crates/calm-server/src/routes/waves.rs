//! `/api/waves`, `/api/coves/:id/waves` — Wave CRUD. **Owned by Track B.**
//!
//! Writes go through `Repo::write_with_event` (via the
//! `write_with_event_typed` ergonomic wrapper). See `routes/coves.rs` for
//! the migration pattern; this file follows the same shape.

use crate::actor::Actor;
use crate::db::sqlite::{wave_create_tx, wave_delete_tx, wave_update_tx};
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
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
    actor: Actor,
    Json(p): Json<NewWave>,
) -> Result<(StatusCode, Json<Wave>)> {
    // Cove is known up-front (it's a required field on NewWave); the wave
    // id is minted inside the txn, so we can't tag the create with
    // `EventScope::Wave { wave: <new_id>, ... }` without racing the
    // commit-then-emit invariant. `Cove`-scoped is the most specific
    // scope we can stamp deterministically — a per-cove subscriber sees
    // the create; a per-wave subscriber will pick it up via the cove
    // channel (PR5 routes by ancestor too).
    let cove = p.cove_id.clone();
    let (wave, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        EventScope::Cove { cove },
        None,
        &s.events,
        &s.card_role_cache,
        move |tx| {
            Box::pin(async move {
                let wave = wave_create_tx(tx, p).await?;
                Ok((wave.clone(), Event::WaveUpdated(wave)))
            })
        },
    )
    .await?;
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
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<WavePatch>,
) -> Result<Json<Wave>> {
    // Need cove_id for the scope. Wave rows are immutable wrt their
    // parent cove, so reading outside the txn is safe (same rationale as
    // the delete path below).
    let existing = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    let scope = EventScope::Wave {
        wave: existing.id.clone(),
        cove: existing.cove_id.clone(),
    };
    let (wave, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        move |tx| {
            Box::pin(async move {
                let wave = wave_update_tx(tx, &id, p).await?;
                Ok((wave.clone(), Event::WaveUpdated(wave)))
            })
        },
    )
    .await?;
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
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Look up first (outside the txn) so we know the cove_id for the
    // delete event. Reading outside the txn is fine — there's no
    // concurrent write that could change `wave.cove_id` between the
    // read and the write_with_event start (wave rows are immutable
    // wrt their parent cove).
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    let cove_id = wave.cove_id.clone();
    let wave_id = wave.id.clone();
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        move |tx| {
            Box::pin(async move {
                wave_delete_tx(tx, wave_id.as_ref()).await?;
                Ok((
                    (),
                    Event::WaveDeleted {
                        id: wave_id,
                        cove_id,
                    },
                ))
            })
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

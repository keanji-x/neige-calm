//! `/api/overlays` — read overlays attached to an entity.
//! **Owned by Track B.**
//!
//! Writes (`upsert`, `delete`) eventually come from plugins via MCP and live
//! in `plugin_host`. For M1 we expose write endpoints too so we can hand-test
//! overlay rendering without a real plugin.
//!
//! Writes go through `Repo::write_with_event` via `write_with_event_typed`
//! per Scope A — see `routes/coves.rs` for the template.

use crate::actor::Actor;
use crate::db::sqlite::{overlay_delete_tx, overlay_upsert_tx};
use crate::db::write_with_event_typed;
use crate::error::{ErrorBody, Result};
use crate::event::Event;
use crate::model::{NewOverlay, Overlay};
use crate::state::AppState;
use crate::validation::validate_overlay_payload;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/overlays", get(list_overlays).post(upsert_overlay))
        .route("/api/overlays/delete", axum::routing::post(delete_overlay))
}

#[derive(Deserialize, IntoParams, ToSchema)]
pub struct OverlayQuery {
    pub entity_kind: String,
    /// Optional. When omitted, returns every overlay of `entity_kind`
    /// across the workspace — the sidebar uses this form to render
    /// accurate per-wave status without fetching each wave's detail.
    pub entity_id: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/overlays",
    tag = "overlays",
    params(OverlayQuery),
    responses(
        (status = 200, description = "Overlays for an entity (or all of a kind when entity_id is omitted)", body = Vec<Overlay>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_overlays(
    State(s): State<AppState>,
    Query(q): Query<OverlayQuery>,
) -> Result<Json<Vec<Overlay>>> {
    let overlays = match q.entity_id.as_deref() {
        Some(eid) => s.repo.overlays_for(&q.entity_kind, eid).await?,
        None => s.repo.overlays_by_kind(&q.entity_kind).await?,
    };
    Ok(Json(overlays))
}

#[utoipa::path(
    post,
    path = "/api/overlays",
    tag = "overlays",
    request_body = NewOverlay,
    responses(
        (status = 200, description = "Overlay upserted", body = Overlay),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn upsert_overlay(
    State(s): State<AppState>,
    actor: Actor,
    Json(p): Json<NewOverlay>,
) -> Result<Json<Overlay>> {
    // D4: kernel-owned overlay kinds (status/progress/eta/now) must match
    // their shape; plugin-defined kinds stay opaque.
    validate_overlay_payload(&p.kind, &p.payload)?;
    let (overlay, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.as_str(),
        None,
        &s.events,
        move |tx| {
            Box::pin(async move {
                let overlay = overlay_upsert_tx(tx, p).await?;
                Ok((overlay.clone(), Event::OverlaySet(overlay)))
            })
        },
    )
    .await?;
    Ok(Json(overlay))
}

#[derive(Deserialize, ToSchema)]
pub struct OverlayDeleteBody {
    pub plugin_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub kind: String,
}

#[utoipa::path(
    post,
    path = "/api/overlays/delete",
    tag = "overlays",
    request_body = OverlayDeleteBody,
    responses(
        (status = 204, description = "Overlay deleted"),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_overlay(
    State(s): State<AppState>,
    actor: Actor,
    Json(b): Json<OverlayDeleteBody>,
) -> Result<StatusCode> {
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.as_str(),
        None,
        &s.events,
        move |tx| {
            Box::pin(async move {
                overlay_delete_tx(tx, &b.plugin_id, &b.entity_kind, &b.entity_id, &b.kind).await?;
                Ok((
                    (),
                    Event::OverlayDeleted {
                        plugin_id: b.plugin_id,
                        entity_kind: b.entity_kind,
                        entity_id: b.entity_id,
                        kind: b.kind,
                    },
                ))
            })
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

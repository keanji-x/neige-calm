//! `/api/overlays` — read overlays attached to an entity.
//! **Owned by Track B.**
//!
//! Writes (`upsert`, `delete`) eventually come from plugins via MCP and live
//! in `plugin_host`. For M1 we expose write endpoints too so we can hand-test
//! overlay rendering without a real plugin.

use crate::error::Result;
use crate::event::Event;
use crate::model::{NewOverlay, Overlay};
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/overlays", get(list).post(upsert))
        .route("/api/overlays/delete", axum::routing::post(delete_))
}

#[derive(Deserialize)]
pub struct OverlayQuery {
    pub entity_kind: String,
    pub entity_id: String,
}

async fn list(
    State(s): State<AppState>,
    Query(q): Query<OverlayQuery>,
) -> Result<Json<Vec<Overlay>>> {
    let overlays = s.repo.overlays_for(&q.entity_kind, &q.entity_id).await?;
    Ok(Json(overlays))
}

async fn upsert(
    State(s): State<AppState>,
    Json(p): Json<NewOverlay>,
) -> Result<Json<Overlay>> {
    let overlay = s.repo.overlay_upsert(p).await?;
    s.events.emit(Event::OverlaySet(overlay.clone()));
    Ok(Json(overlay))
}

#[derive(Deserialize)]
pub struct OverlayDeleteBody {
    pub plugin_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub kind: String,
}

async fn delete_(
    State(s): State<AppState>,
    Json(b): Json<OverlayDeleteBody>,
) -> Result<StatusCode> {
    s.repo
        .overlay_delete(&b.plugin_id, &b.entity_kind, &b.entity_id, &b.kind)
        .await?;
    s.events.emit(Event::OverlayDeleted {
        plugin_id: b.plugin_id,
        entity_kind: b.entity_kind,
        entity_id: b.entity_id,
        kind: b.kind,
    });
    Ok(StatusCode::NO_CONTENT)
}

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
use crate::db::RepoRead;
use crate::db::sqlite::{overlay_delete_tx, overlay_upsert_tx};
use crate::db::write_with_event_typed;
use crate::error::{ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::model::{NewOverlay, Overlay};
use crate::state::AppState;
use crate::validation::{should_skip_overlay, validate_overlay_payload};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

/// Build an `EventScope` for an overlay write keyed by `(entity_kind, entity_id)`.
/// PR2 of #136 — overlays are polymorphic, so we pattern-match on the
/// declared `entity_kind`:
///
///   * `"card"` — resolve `card → wave → cove` and emit `EventScope::Card`.
///   * `"wave"` — resolve `wave → cove` and emit `EventScope::Wave`.
///   * anything else — fall back to `EventScope::System` (a plugin
///     emitting on a kind the kernel doesn't model can't be routed by
///     ancestor scope; the firehose still sees it).
///
/// Missing card / wave rows surface as `EventScope::System` rather than
/// `NotFound` — overlay writes against a deleted entity are legal (the
/// row just becomes a tombstone). We don't want to refuse the write for
/// a stale row when the same write against a live row would succeed.
pub(crate) async fn overlay_scope(
    repo: &dyn RepoRead,
    entity_kind: &str,
    entity_id: &str,
) -> Result<EventScope> {
    match entity_kind {
        "card" => {
            let card = match repo.card_get(entity_id).await? {
                Some(c) => c,
                None => return Ok(EventScope::System),
            };
            let wave = match repo.wave_get(card.wave_id.as_str()).await? {
                Some(w) => w,
                None => return Ok(EventScope::System),
            };
            Ok(EventScope::Card {
                card: card.id,
                wave: wave.id,
                cove: wave.cove_id,
            })
        }
        "wave" => {
            let wave = match repo.wave_get(entity_id).await? {
                Some(w) => w,
                None => return Ok(EventScope::System),
            };
            Ok(EventScope::Wave {
                wave: wave.id,
                cove: wave.cove_id,
            })
        }
        // `view` and any other plugin-defined entity_kind: the kernel
        // doesn't track an ancestor chain we can populate, so go System
        // and let PR5's per-entity subscriber filter on the payload.
        _ => Ok(EventScope::System),
    }
}

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
    Ok(Json(filter_unsupported_overlay_versions(overlays)))
}

/// Tier A read-side guard (issue #198 concern 4): drop kernel-owned overlay
/// rows whose persisted `schemaVersion` exceeds what this binary supports.
///
/// The write path already refuses future versions on ingest, but a row can
/// still appear here if a newer kernel binary wrote to the same DB and then
/// the operator downgraded (or in a split-deploy where two binaries point at
/// one DB). Without this filter, those rows would deserialize successfully —
/// because the `Overlay.payload` column is opaque JSON — and either fall
/// through to the frontend (where the Tier A `schemaVersion` check would
/// then log + skip them) or break invariants in any server consumer that
/// inspects the payload shape.
///
/// Plugin-defined overlay kinds (`max_supported_overlay_schema_version`
/// returns `None`) are passed through untouched: the kernel has no schema
/// for them and explicitly opts out of any version policy on their payloads.
///
/// Visibility note: `pub(super)` so `routes::waves::get_wave_detail` can apply
/// the same guard to overlays returned alongside the wave row. The reviewer of
/// PR #214 (issue #198 concern 4 follow-up) flagged that `GET /api/waves/{id}`
/// is the primary read path the frontend uses to render status/progress/eta/
/// now overlays on a wave's detail view, and a future-`schemaVersion` row
/// would sail through that route while being correctly filtered out of
/// `GET /api/overlays`. We keep the route-level filter co-located here so
/// both HTTP call-sites share one implementation without expanding the
/// `Repo` trait surface; the per-row predicate itself lives in
/// `crate::validation::should_skip_overlay` so the WS broadcast/replay
/// path in `ws::events` can apply the same gate to `Event::OverlaySet`
/// frames without a routes → ws dependency.
pub(super) fn filter_unsupported_overlay_versions(overlays: Vec<Overlay>) -> Vec<Overlay> {
    overlays
        .into_iter()
        .filter(|o| !should_skip_overlay(o))
        .collect()
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
    let scope = overlay_scope(s.repo.as_ref(), &p.entity_kind, &p.entity_id).await?;
    let (overlay, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
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
    let scope = overlay_scope(s.repo.as_ref(), &b.entity_kind, &b.entity_id).await?;
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
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

//! `/api/overlays` — read overlays attached to an entity, plus hand-testing write
//! endpoints bounded to the same surface a plugin gets.

use crate::actor::Actor;
use crate::db::RepoRead;
use crate::db::sqlite::{overlay_delete_tx, overlay_upsert_tx};
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::model::{NewOverlay, Overlay};
use crate::state::{AppState, RouteState};
use crate::validation::{
    KERNEL_OVERLAY_PLUGIN_ID, OVERLAY_ENTITY_SCOPE_REGISTRY, should_skip_overlay,
    validate_overlay_payload,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

/// Build an `EventScope` for an overlay write keyed by `(entity_kind, entity_id)`.
/// Missing card / track rows surface as `EventScope::System` rather than `NotFound`:
/// overlay writes against a deleted entity are legal (the row becomes a tombstone).
pub(crate) async fn overlay_scope(
    repo: &dyn RepoRead,
    entity_kind: &str,
    entity_id: &str,
) -> Result<EventScope> {
    OVERLAY_ENTITY_SCOPE_REGISTRY
        .route_scope(repo, entity_kind, entity_id)
        .await
        .map_err(Into::into)
}

/// Admission gate for the public overlay write endpoints: `plugin_id` `"kernel"` and
/// the kernel-projection entity kinds (`view`, `system`) are unforgeable from outside.
/// Answers 403 and runs before `validate_overlay_payload` so a refused write never
/// reveals whether its payload would have parsed.
fn ensure_overlay_write_allowed(plugin_id: &str, entity_kind: &str) -> Result<()> {
    if plugin_id == KERNEL_OVERLAY_PLUGIN_ID {
        return Err(CalmError::Forbidden(format!(
            "plugin_id `{KERNEL_OVERLAY_PLUGIN_ID}` is reserved for kernel-authored overlays",
        )));
    }
    if !OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable(entity_kind) {
        let kinds = OVERLAY_ENTITY_SCOPE_REGISTRY
            .externally_writable_kinds()
            .join(", ");
        return Err(CalmError::Forbidden(format!(
            "entity_kind must be one of [{kinds}], got `{entity_kind}`",
        )));
    }
    Ok(())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/overlays", get(list_overlays).post(upsert_overlay))
        .route("/api/overlays/delete", axum::routing::post(delete_overlay))
}

#[derive(Deserialize, IntoParams, ToSchema)]
pub struct OverlayQuery {
    pub entity_kind: String,
    /// Optional. When omitted, returns every overlay of `entity_kind` across the workspace.
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
    State(s): State<RouteState>,
    Query(q): Query<OverlayQuery>,
) -> Result<Json<Vec<Overlay>>> {
    let overlays = match q.entity_id.as_deref() {
        Some(eid) => s.repo.overlays_for(&q.entity_kind, eid).await?,
        None => s.repo.overlays_by_kind(&q.entity_kind).await?,
    };
    Ok(Json(filter_unsupported_overlay_versions(overlays)))
}

/// Drop kernel-owned overlay rows whose persisted `schemaVersion` exceeds what this
/// binary supports (a newer binary wrote to the same DB). Plugin-defined kinds pass
/// through untouched. `pub(super)` so `get_track_detail` applies the same guard.
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
        (status = 403, description = "Reserved kernel namespace (plugin_id or entity_kind)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn upsert_overlay(
    State(s): State<RouteState>,
    actor: Actor,
    Json(p): Json<NewOverlay>,
) -> Result<Json<Overlay>> {
    // Reserved namespaces first — permission before shape.
    ensure_overlay_write_allowed(&p.plugin_id, &p.entity_kind)?;
    // Kernel-owned overlay kinds must match their shape; plugin-defined kinds stay opaque.
    validate_overlay_payload(&p.kind, &p.payload)?;
    let scope = overlay_scope(s.repo.as_ref(), &p.entity_kind, &p.entity_id).await?;
    let (overlay, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
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
        (status = 403, description = "Reserved kernel namespace (plugin_id or entity_kind)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_overlay(
    State(s): State<RouteState>,
    actor: Actor,
    Json(b): Json<OverlayDeleteBody>,
) -> Result<StatusCode> {
    // Deleting a kernel-authored row is the second half of a forge, so the gate applies
    // on the delete side too.
    ensure_overlay_write_allowed(&b.plugin_id, &b.entity_kind)?;
    let scope = overlay_scope(s.repo.as_ref(), &b.entity_kind, &b.entity_id).await?;
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
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

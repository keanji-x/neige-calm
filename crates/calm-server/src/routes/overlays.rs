//! `/api/overlays` — read overlays attached to an entity.
//! **Owned by Track B.**
//!
//! Writes (`upsert`, `delete`) eventually come from plugins via MCP and live
//! in `plugin_host`. For M1 we expose write endpoints too so we can hand-test
//! overlay rendering without a real plugin. That hand-testing affordance is
//! bounded by [`ensure_overlay_write_allowed`] (#1297): it reaches the same
//! surface a plugin gets, never the kernel's own reserved namespaces.
//!
//! Writes go through `Repo::write_with_event` via `write_with_event_typed`
//! per Scope A — see `routes/areas.rs` for the template.

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
/// Missing card / track rows surface as `EventScope::System` rather than
/// `NotFound` — overlay writes against a deleted entity are legal (the row
/// just becomes a tombstone).
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

/// Admission gate for the public overlay write endpoints (issue #1297).
///
/// Two reserved namespaces must be unforgeable from outside the process:
///
///   * **`entity_kind`** — `view` and `system` hold kernel projections that
///     the kernel reads back as fact. A `kernel/view/template` row decides
///     whether the scheduler dispatches a track's tasks at all
///     (`scheduler::…` admission and its in-claim backstop), whether a planner
///     harness may start, and whether the track appears in `GET /api/tracks`.
///     Before this gate, any client with a session could POST that row onto
///     a *running* track and silently strand it — dispatch stops and the track
///     vanishes from the list, with nothing in the UI to say why.
///   * **`plugin_id`** — `"kernel"` is the namespace `card_fsm` stamps on
///     its own rows precisely so they are "unambiguously kernel-owned". A
///     client writing under it forges that ownership.
///
/// The `entity_kind` half is not a new criterion: the registry column it
/// asks is the same one the plugin RPC path has always asked
/// (`plugin_host::callbacks::overlay_set`). This endpoint simply never
/// asked it — the gap was one entry point wide, not one rule wide.
///
/// Both are permission failures rather than shape failures, so they answer
/// 403, and both run *before* `validate_overlay_payload` so a refused write
/// never reveals whether its payload would have parsed.
///
/// Kernel-internal writers are unaffected: they call `overlay_upsert_tx`
/// directly (track structure creation, `card_fsm`, `child_track_adapter`) and
/// never traverse this router.
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
    /// Optional. When omitted, returns every overlay of `entity_kind`
    /// across the workspace — the sidebar uses this form to render
    /// accurate per-track status without fetching each track's detail.
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
/// Visibility note: `pub(super)` so `routes::tracks::get_track_detail` can apply
/// the same guard to overlays returned alongside the track row. The reviewer of
/// PR #214 (issue #198 concern 4 follow-up) flagged that `GET /api/tracks/{id}`
/// is the primary read path the frontend uses to render status/progress/eta/
/// now overlays on a track's detail view, and a future-`schemaVersion` row
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
        (status = 403, description = "Reserved kernel namespace (plugin_id or entity_kind)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn upsert_overlay(
    State(s): State<RouteState>,
    actor: Actor,
    Json(p): Json<NewOverlay>,
) -> Result<Json<Overlay>> {
    // #1297: reserved namespaces first — permission before shape.
    ensure_overlay_write_allowed(&p.plugin_id, &p.entity_kind)?;
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
    // #1297: deleting a kernel-authored row is the second half of the forge
    // — mark a track as a template, act, then remove the evidence.
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

//! `/api/areas` — Area CRUD. **Owned by Track B.**
//!
//! Writes go through `Repo::write_with_event` (via the
//! `write_with_event_typed` ergonomic wrapper). The wrapper atomically
//! commits the entity write + the events-table insert, then broadcasts a
//! `BroadcastEnvelope { id, actor, event }` on the bus. Handler-level `events.emit`
//! calls are gone after Scope A; see `docs/sync-engine-design.md` §3.
//!
//! Issue #175 — `areas.kind` (introduced in migration 0009) marks rows as
//! either user-visible or system-owned. `GET /api/areas` defaults to the
//! filtered `kind='user'` list so the kernel-minted system area (which
//! hosts the default Today terminal's track + card) doesn't leak into the
//! sidebar; opt back into the full list via `?include_system=true`.
//! `POST /api/areas` never accepts a `kind` field — every area created
//! through the regular surface lands as `User`. The system area is minted
//! exclusively via the idempotent `POST /api/areas/system` upsert, and
//! `DELETE /api/areas/{id}` refuses (`403 forbidden`) when the target row
//! has `kind = 'system'` — system scaffolding is kernel-owned and not
//! user-deletable.

use crate::actor::Actor;
use crate::db::sqlite::{
    area_create_system_tx, area_create_tx, area_delete_tx, area_update_tx,
    overlay_delete_by_entity_tx, overlay_delete_subtree_by_area_tx, terminal_delete_tx,
};
use crate::db::{write_with_actor_events_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{Area, AreaKind, AreaPatch, NewArea};
use crate::operation::workspace_lease::{
    any_track_has_active_forge_action, release_workspace_leases_for_track_tx,
    sweep_workspace_worktrees_for_tracks_repo,
};
use crate::routes::cards::interrupt_shared_card_active_turn;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::workspace_materialize::validate_attached_workspace;
use crate::workspace_recycle;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::area_folders::normalize_path;
use crate::templates::template_by_key;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/areas", get(list_areas).post(create_area))
        .route(
            "/api/areas/system",
            axum::routing::post(get_or_create_system_area),
        )
        .route(
            "/api/areas/{id}",
            axum::routing::patch(update_area).delete(delete_area),
        )
}

/// Query string accepted by `GET /api/areas`.
///
/// Issue #175 — `include_system=true` opts into the full list (including
/// the singleton system area). Default false: the system area stays hidden
/// from the user-facing surface so the sidebar doesn't render it.
#[derive(Debug, Default, Deserialize, IntoParams, ToSchema)]
pub struct ListAreasQuery {
    /// When true, also include `kind='system'` areas in the response.
    /// Default false — the sidebar / Today UI consume the filtered list
    /// and never need the system area. Documented opt-in for debug surfaces
    /// and integration tests.
    #[serde(default)]
    pub include_system: bool,
}

/// User-facing Area creation. The raw sync-domain `NewArea` stays narrow for
/// internal callers; these two preferences belong to the REST product surface
/// and are applied inside the same audited transaction as the Area row.
///
/// Deliberately permissive about unknown JSON keys, matching the historical
/// `NewArea` contract: in particular a caller-supplied `kind` must continue to
/// be ignored rather than gaining a path to create a system Area.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAreaRequest {
    pub name: String,
    pub color: String,
    /// If absent, server appends to end.
    pub sort: Option<f64>,
    #[serde(default)]
    pub default_template_id: Option<String>,
    #[serde(default)]
    pub default_cwd: Option<String>,
}

fn validate_default_template(default_template_id: Option<&str>) -> Result<()> {
    let Some(template_id) = default_template_id else {
        return Ok(());
    };
    if template_by_key(template_id).is_none() {
        return Err(CalmError::BadRequest(format!(
            "area default: `default_template_id` must reference a known track template; got `{template_id}`"
        )));
    }
    Ok(())
}

fn validate_and_normalize_default_cwd(default_cwd: &mut Option<String>) -> Result<()> {
    let Some(cwd) = default_cwd else {
        return Ok(());
    };
    validate_attached_workspace(std::path::Path::new(cwd))?;
    *cwd = normalize_path(cwd);
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/areas",
    tag = "areas",
    params(ListAreasQuery),
    responses(
        (status = 200, description = "List all areas (filtered to `kind='user'` unless `include_system=true` is set)", body = Vec<Area>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_areas(
    State(s): State<RouteState>,
    Query(q): Query<ListAreasQuery>,
) -> Result<Json<Vec<Area>>> {
    // Issue #175 — default to the user-visible subset so the sidebar
    // never sees the singleton system area. `?include_system=true` is
    // the opt-in escape hatch for debug surfaces and integration tests
    // that need to assert on the full row set.
    let areas = if q.include_system {
        s.repo.areas_list().await?
    } else {
        s.repo.areas_list_user_visible().await?
    };
    Ok(Json(areas))
}

#[utoipa::path(
    post,
    path = "/api/areas",
    tag = "areas",
    request_body = CreateAreaRequest,
    responses(
        (status = 201, description = "Area created", body = Area),
        (status = 400, description = "Unknown default template or invalid attached default folder", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn create_area(
    State(s): State<RouteState>,
    actor: Actor,
    Json(mut request): Json<CreateAreaRequest>,
) -> Result<(StatusCode, Json<Area>)> {
    validate_default_template(request.default_template_id.as_deref())?;
    validate_and_normalize_default_cwd(&mut request.default_cwd)?;
    let p = NewArea {
        name: request.name,
        color: request.color,
        sort: request.sort,
    };
    let has_defaults = request.default_template_id.is_some() || request.default_cwd.is_some();
    let defaults = AreaPatch {
        default_template_id: request.default_template_id.map(Some),
        default_cwd: request.default_cwd.map(Some),
        ..AreaPatch::default()
    };
    // Judgment call (PR2 of #136): create uses `EventScope::System`
    // rather than `EventScope::Area { area: <new_id> }`. The area id is
    // minted inside the txn closure; we don't know it before the write.
    // Capturing the id post-commit to pass into the scope would make the
    // commit-then-emit invariant racy. `System` is also defensible
    // semantically — at the moment the event fires, the area is new to
    // every replica anyway, so per-area subscribers can pick it up via
    // the broader system-wide channel.
    //
    // Issue #175 — `NewArea` carries no `kind` field; `area_create_tx`
    // unconditionally lands rows as `AreaKind::User`. The system area
    // has its own endpoint below.
    let (area, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        EventScope::System,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                let mut area = area_create_tx(tx, p).await?;
                if has_defaults {
                    let area_id = area.id.clone();
                    area = area_update_tx(tx, area_id.as_str(), defaults).await?;
                }
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(area)))
}

#[utoipa::path(
    post,
    path = "/api/areas/system",
    tag = "areas",
    responses(
        (status = 200, description = "System area already existed; returned the existing row", body = Area),
        (status = 201, description = "System area minted", body = Area),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
/// Issue #175 — idempotent upsert for the singleton system area that
/// hosts the default Today terminal's track + card. Returns 200 with the
/// existing row when one is present; otherwise mints a new row and
/// returns 201. The DB-level partial unique index on
/// `areas(kind) WHERE kind = 'system'` enforces the at-most-one
/// invariant as a backstop, so two tabs racing this endpoint can both
/// safely call it: the loser of the write race catches the unique
/// violation, re-reads the row the winner committed, and returns 200
/// to its own caller. From the frontend's perspective both racers see a
/// success and a populated `Area` body — the only observable difference
/// is the status code (201 vs 200), and `useTodayTerminal` treats both
/// as success.
///
/// The endpoint exists so the frontend's `useTodayTerminal` hook can
/// bootstrap a default terminal without exposing the underlying system
/// area to the regular `POST /api/areas` surface (which the sidebar
/// "+ New area" affordance consumes and which would otherwise need a
/// reserved-name policy).
pub(crate) async fn get_or_create_system_area(
    State(s): State<RouteState>,
    // Note: `Actor` is extracted to keep this handler consistent with the
    // rest of the area surface (it forces the middleware to validate the
    // `X-Calm-Actor` header), but the value is intentionally **not**
    // propagated into the event log. The system area is kernel-owned
    // scaffolding — a `area.updated` event for the mint stamped with
    // `User` would be untruthful and would let a future audit pipeline
    // misattribute the row to the human caller. We hardcode
    // `ActorId::Kernel` below, mirroring the convention the FSM projector
    // and terminal sweeper already use for server-internal lifecycle.
    _actor: Actor,
) -> Result<(StatusCode, Json<Area>)> {
    // Existence check first — the common path is "system area already
    // exists, just return it" (every Today-page load after the first
    // ever). Avoids opening a write transaction in the hot path.
    if let Some(existing) = s.repo.area_get_system().await? {
        return Ok((StatusCode::OK, Json(existing)));
    }
    // Mint the row inside a `write_with_event` closure so the create
    // emits a `area.updated` envelope on the bus, just like the regular
    // `POST /api/areas`. Scope is `System` (same rationale as
    // `create_area`: the area id is minted inside the closure). Actor is
    // hardcoded to `ActorId::Kernel` — see the `_actor` extractor doc
    // above for the rationale.
    let mint_result = write_with_event_typed(
        s.repo.as_ref(),
        ActorId::Kernel,
        EventScope::System,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                let area = area_create_system_tx(tx).await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await;
    match mint_result {
        Ok((area, _id)) => Ok((StatusCode::CREATED, Json(area))),
        // Race: two cold-boot Today-page loads can both see `area_get_system()
        // == None` above and both reach the mint closure; the partial unique
        // index on `areas(kind) WHERE kind = 'system'` from migration 0009
        // backstops the at-most-one invariant by failing the loser's INSERT.
        // We catch that DB error, re-read the now-existing row, and return
        // 200 — the caller's effective postcondition (a present system area)
        // is satisfied. Without this fallback the loser would surface a 500
        // and `useTodayTerminal` would render the Today page in an error
        // state until reload. We're permissive (any `Db` error retries the
        // read) rather than down-casting to a typed `sqlx::error::DatabaseError`
        // because sqlx requires an `Any` boundary for that and the repo's
        // existing precedent (`dispatcher::is_sqlite_busy`) likewise
        // matches on the surface string; if the original error is something
        // other than the unique violation, the follow-up read returns `None`
        // and we propagate it unchanged.
        Err(e) => match e {
            CalmError::Db(_) => match s.repo.area_get_system().await? {
                Some(existing) => Ok((StatusCode::OK, Json(existing))),
                None => Err(e),
            },
            other => Err(other),
        },
    }
}

#[utoipa::path(
    patch,
    path = "/api/areas/{id}",
    tag = "areas",
    params(("id" = String, Path, description = "Area id")),
    request_body = AreaPatch,
    responses(
        (status = 200, description = "Area updated", body = Area),
        (status = 400, description = "Unknown default template or invalid attached default folder", body = ErrorBody),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_area(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(mut p): Json<AreaPatch>,
) -> Result<Json<Area>> {
    // Preserve the route's resource-first error contract. Besides returning
    // the documented 404 for an unknown id, this prevents an invalid
    // caller-supplied path from triggering filesystem metadata and `git`
    // probes for a resource that does not exist.
    s.repo
        .area_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("area {id}")))?;
    validate_default_template(
        p.default_template_id
            .as_ref()
            .and_then(|value| value.as_deref()),
    )?;
    if let Some(value) = p.default_cwd.as_mut() {
        validate_and_normalize_default_cwd(value)?;
    }
    let scope = EventScope::Area {
        area: id.clone().into(),
    };
    let (area, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                let area = area_update_tx(tx, &id, p).await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await?;
    Ok(Json(area))
}

#[utoipa::path(
    delete,
    path = "/api/areas/{id}",
    tag = "areas",
    params(("id" = String, Path, description = "Area id")),
    responses(
        (status = 204, description = "Area deleted"),
        (status = 403, description = "Area is system-owned and cannot be deleted via REST", body = ErrorBody),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_area(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Issue #175 followup — refuse to delete the singleton system area
    // via the REST surface. The underlying `area_delete_tx` is a low-level
    // primitive that trusts its caller (the same helper is reachable from
    // server-internal sites like replay fixtures); the policy decision
    // "system areas are not user-deletable" lives at the handler boundary
    // here. We pre-check via `area_get` rather than threading the kind
    // through `_tx`'s WHERE clause because:
    //   * the read is cheap (single row, indexed by PK),
    //   * a transactional check would still need this surface to translate
    //     "no row affected because kind='system'" into a 403 rather than
    //     the txn's natural 404 — same code-shape, same trip to the DB,
    //     and the handler check fails fast without opening a write txn.
    // #1147 S5 — also the input to recycle guard 4. `None` (no such area)
    // stays `None` and makes every recycle below refuse; the row delete still
    // runs and 404s naturally in `area_delete_tx`.
    let area_kind = s.repo.area_get(&id).await?.map(|area| area.kind);
    if area_kind == Some(AreaKind::System) {
        return Err(CalmError::Forbidden(format!(
            "area {id} is system-owned and cannot be deleted via the public API"
        )));
    }

    // OperationRuntime is the common funnel for normal runtime/process starts.
    // Track DELETE holds this same guard through commit or compensation, so an
    // area deletion cannot erase the rows underneath a workspace restoration.
    let _operation_guard = s.operation_runtime.lock_for_track_delete().await;

    let tracks = s.repo.tracks_by_area(&id).await?;
    let mut guarded_track_ids = tracks
        .iter()
        .map(|track| track.id.to_string())
        .collect::<Vec<_>>();
    guarded_track_ids.sort();
    // Direct harness recovery and websocket terminal reattach bypass the
    // operation driver. Lock every member in stable order before teardown so
    // those paths either finish before this snapshot or observe deleted rows.
    let mut _track_delete_guards = Vec::with_capacity(guarded_track_ids.len());
    for track_id in &guarded_track_ids {
        _track_delete_guards
            .push(crate::per_card_lock::lock_key(&s.track_delete_locks, track_id).await);
    }
    let track_ids = guarded_track_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    // Defensive TOCTOU guard only: this non-transactional read happens before
    // the teardown tx, so a forge-action can still become in-flight before the
    // sweep. It shrinks the race; durable parked recovery is the backstop, and
    // the airtight in-tx/lease-hold guard belongs to slice ⑤.
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("delete_area forge-action fence requires sqlite-backed repo".into())
    })?;
    if any_track_has_active_forge_action(&pool, &track_ids).await? {
        return Err(CalmError::Conflict(format!(
            "area {id} has a child track with an in-flight forge-action; retry after it settles"
        )));
    }

    // Issue #197 — eager teardown for every terminal under the area.
    // `terminals.card_id` is `ON DELETE RESTRICT` (migration 0011), so
    // an area delete that would orphan a terminal row aborts the
    // surrounding txn unless we drain the table first. Walk
    // tracks → cards → terminal_get_by_card; reap the daemon + socket
    // for each; collect the terminal ids for the in-txn row delete. The
    // overlay sweep derives current track/card ids inside the write txn.
    let mut terminal_ids: Vec<String> = Vec::new();
    for track in &tracks {
        let cards = s.repo.cards_by_track(track.id.as_str()).await?;
        for card in &cards {
            interrupt_shared_card_active_turn(s.repo.as_ref(), &cs, card).await;
            if let Some(t) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
                reap_terminal_artifacts_with_renderer(Some(w.terminal_renderer.as_ref()), &t).await;
                terminal_ids.push(t.id);
            }
        }
        let active_runtime_ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM worker_sessions WHERE track_id=?1 \
             AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
        )
        .bind(track.id.as_str())
        .fetch_all(&pool)
        .await?;
        for runtime_id in active_runtime_ids {
            if let Some(harness) = w.harness.get(&runtime_id) {
                harness.shutdown().await?;
                let _ = w.harness.remove(&runtime_id);
            }
        }
    }

    // #1147 S5 — reclaim every managed workspace under this area, plus the
    // `<root>/<area_id>/` layer, before the rows go away.
    //
    // Until this slice, deleting an area released leases and swept worktrees but
    // left every managed repository on disk with no database row pointing at
    // it: an orphan tree that nothing could ever find again, let alone reclaim.
    //
    // Same ordering rationale as `delete_track`: terminals are already reaped,
    // and a failure here aborts the DELETE with rows and directories both
    // intact. Per-track refusals are not failures — they leave that one
    // directory (and therefore the area directory) in place, visibly.
    let now = crate::model::now_ms();
    let targets: Vec<workspace_recycle::RecycleTarget<'_>> = tracks
        .iter()
        .map(|track| workspace_recycle::RecycleTarget {
            track_id: track.id.as_str(),
            workspace: &track.workspace,
        })
        .collect();
    workspace_recycle::recycle_area_workspaces(&s.workspace_root, &id, area_kind, &targets, now)?;
    workspace_recycle::gc_trash_best_effort(&s.workspace_root, now);

    let scope = EventScope::Area {
        area: id.clone().into(),
    };
    let delete_actor = actor.to_actor_id();
    let (sweeps, _ids) =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            Box::pin(async move {
                // Drop terminal rows first; tolerate NotFound on each
                // (a racing sweeper tick may have beaten us to one).
                for tid in &terminal_ids {
                    match terminal_delete_tx(tx, tid).await.map_err(CalmError::from) {
                        Ok(()) => {}
                        Err(CalmError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                overlay_delete_subtree_by_area_tx(tx, &id).await?;
                overlay_delete_by_entity_tx(tx, "area", &id).await?;
                let mut events = Vec::new();
                let mut sweeps = Vec::new();
                let track_ids: Vec<String> =
                    sqlx::query_scalar("SELECT id FROM tracks WHERE area_id = ?1")
                        .bind(&id)
                        .fetch_all(&mut **tx)
                        .await?;
                for track_id in &track_ids {
                    let release =
                        release_workspace_leases_for_track_tx(tx, track_id.as_str()).await?;
                    events.extend(release.events);
                    if let Some(sweep) = release.sweep {
                        sweeps.push(sweep);
                    }
                }
                area_delete_tx(tx, &id).await?;
                events.push((delete_actor, scope, Event::AreaDeleted { id: id.into() }));
                Ok((sweeps, events))
            })
        })
        .await?;
    for track_id in &guarded_track_ids {
        s.write
            .forget_track(&crate::ids::TrackId::from(track_id.clone()));
    }
    sweep_workspace_worktrees_for_tracks_repo(s.repo.as_ref(), &s.events, sweeps).await?;
    Ok(StatusCode::NO_CONTENT)
}

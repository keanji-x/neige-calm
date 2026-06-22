//! `/api/coves` — Cove CRUD. **Owned by Track B.**
//!
//! Writes go through `Repo::write_with_event` (via the
//! `write_with_event_typed` ergonomic wrapper). The wrapper atomically
//! commits the entity write + the events-table insert, then broadcasts a
//! `BroadcastEnvelope { id, actor, event }` on the bus. Handler-level `events.emit`
//! calls are gone after Scope A; see `docs/sync-engine-design.md` §3.
//!
//! Issue #175 — `coves.kind` (introduced in migration 0009) marks rows as
//! either user-visible or system-owned. `GET /api/coves` defaults to the
//! filtered `kind='user'` list so the kernel-minted system cove (which
//! hosts the default Today terminal's wave + card) doesn't leak into the
//! sidebar; opt back into the full list via `?include_system=true`.
//! `POST /api/coves` never accepts a `kind` field — every cove created
//! through the regular surface lands as `User`. The system cove is minted
//! exclusively via the idempotent `POST /api/coves/system` upsert, and
//! `DELETE /api/coves/{id}` refuses (`403 forbidden`) when the target row
//! has `kind = 'system'` — system scaffolding is kernel-owned and not
//! user-deletable.

use crate::actor::Actor;
use crate::db::sqlite::{
    cove_create_system_tx, cove_create_tx, cove_delete_tx, cove_update_tx,
    overlay_delete_by_entity_tx, overlay_delete_subtree_by_cove_tx, terminal_delete_tx,
};
use crate::db::{write_with_actor_events_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{Cove, CoveKind, CovePatch, NewCove};
use crate::operation::workspace_lease::{
    any_wave_has_active_forge_action, release_workspace_leases_for_wave_tx,
    sweep_workspace_worktrees_for_waves_repo,
};
use crate::state::{AppState, RouteState, WorkerState};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/coves", get(list_coves).post(create_cove))
        .route(
            "/api/coves/system",
            axum::routing::post(get_or_create_system_cove),
        )
        .route(
            "/api/coves/{id}",
            axum::routing::patch(update_cove).delete(delete_cove),
        )
}

/// Query string accepted by `GET /api/coves`.
///
/// Issue #175 — `include_system=true` opts into the full list (including
/// the singleton system cove). Default false: the system cove stays hidden
/// from the user-facing surface so the sidebar doesn't render it.
#[derive(Debug, Default, Deserialize, IntoParams, ToSchema)]
pub struct ListCovesQuery {
    /// When true, also include `kind='system'` coves in the response.
    /// Default false — the sidebar / Today UI consume the filtered list
    /// and never need the system cove. Documented opt-in for debug surfaces
    /// and integration tests.
    #[serde(default)]
    pub include_system: bool,
}

#[utoipa::path(
    get,
    path = "/api/coves",
    tag = "coves",
    params(ListCovesQuery),
    responses(
        (status = 200, description = "List all coves (filtered to `kind='user'` unless `include_system=true` is set)", body = Vec<Cove>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_coves(
    State(s): State<RouteState>,
    Query(q): Query<ListCovesQuery>,
) -> Result<Json<Vec<Cove>>> {
    // Issue #175 — default to the user-visible subset so the sidebar
    // never sees the singleton system cove. `?include_system=true` is
    // the opt-in escape hatch for debug surfaces and integration tests
    // that need to assert on the full row set.
    let coves = if q.include_system {
        s.repo.coves_list().await?
    } else {
        s.repo.coves_list_user_visible().await?
    };
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
    State(s): State<RouteState>,
    actor: Actor,
    Json(p): Json<NewCove>,
) -> Result<(StatusCode, Json<Cove>)> {
    // Judgment call (PR2 of #136): create uses `EventScope::System`
    // rather than `EventScope::Cove { cove: <new_id> }`. The cove id is
    // minted inside the txn closure; we don't know it before the write.
    // Capturing the id post-commit to pass into the scope would make the
    // commit-then-emit invariant racy. `System` is also defensible
    // semantically — at the moment the event fires, the cove is new to
    // every replica anyway, so per-cove subscribers can pick it up via
    // the broader system-wide channel.
    //
    // Issue #175 — `NewCove` carries no `kind` field; `cove_create_tx`
    // unconditionally lands rows as `CoveKind::User`. The system cove
    // has its own endpoint below.
    let (cove, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        EventScope::System,
        None,
        &s.events,
        &s.write,
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
    post,
    path = "/api/coves/system",
    tag = "coves",
    responses(
        (status = 200, description = "System cove already existed; returned the existing row", body = Cove),
        (status = 201, description = "System cove minted", body = Cove),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
/// Issue #175 — idempotent upsert for the singleton system cove that
/// hosts the default Today terminal's wave + card. Returns 200 with the
/// existing row when one is present; otherwise mints a new row and
/// returns 201. The DB-level partial unique index on
/// `coves(kind) WHERE kind = 'system'` enforces the at-most-one
/// invariant as a backstop, so two tabs racing this endpoint can both
/// safely call it: the loser of the write race catches the unique
/// violation, re-reads the row the winner committed, and returns 200
/// to its own caller. From the frontend's perspective both racers see a
/// success and a populated `Cove` body — the only observable difference
/// is the status code (201 vs 200), and `useTodayTerminal` treats both
/// as success.
///
/// The endpoint exists so the frontend's `useTodayTerminal` hook can
/// bootstrap a default terminal without exposing the underlying system
/// cove to the regular `POST /api/coves` surface (which the sidebar
/// "+ New cove" affordance consumes and which would otherwise need a
/// reserved-name policy).
pub(crate) async fn get_or_create_system_cove(
    State(s): State<RouteState>,
    // Note: `Actor` is extracted to keep this handler consistent with the
    // rest of the cove surface (it forces the middleware to validate the
    // `X-Calm-Actor` header), but the value is intentionally **not**
    // propagated into the event log. The system cove is kernel-owned
    // scaffolding — a `cove.updated` event for the mint stamped with
    // `User` would be untruthful and would let a future audit pipeline
    // misattribute the row to the human caller. We hardcode
    // `ActorId::Kernel` below, mirroring the convention the FSM projector
    // and terminal sweeper already use for server-internal lifecycle.
    _actor: Actor,
) -> Result<(StatusCode, Json<Cove>)> {
    // Existence check first — the common path is "system cove already
    // exists, just return it" (every Today-page load after the first
    // ever). Avoids opening a write transaction in the hot path.
    if let Some(existing) = s.repo.cove_get_system().await? {
        return Ok((StatusCode::OK, Json(existing)));
    }
    // Mint the row inside a `write_with_event` closure so the create
    // emits a `cove.updated` envelope on the bus, just like the regular
    // `POST /api/coves`. Scope is `System` (same rationale as
    // `create_cove`: the cove id is minted inside the closure). Actor is
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
                let cove = cove_create_system_tx(tx).await?;
                Ok((cove.clone(), Event::CoveUpdated(cove)))
            })
        },
    )
    .await;
    match mint_result {
        Ok((cove, _id)) => Ok((StatusCode::CREATED, Json(cove))),
        // Race: two cold-boot Today-page loads can both see `cove_get_system()
        // == None` above and both reach the mint closure; the partial unique
        // index on `coves(kind) WHERE kind = 'system'` from migration 0009
        // backstops the at-most-one invariant by failing the loser's INSERT.
        // We catch that DB error, re-read the now-existing row, and return
        // 200 — the caller's effective postcondition (a present system cove)
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
            CalmError::Db(_) => match s.repo.cove_get_system().await? {
                Some(existing) => Ok((StatusCode::OK, Json(existing))),
                None => Err(e),
            },
            other => Err(other),
        },
    }
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
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<CovePatch>,
) -> Result<Json<Cove>> {
    let scope = EventScope::Cove {
        cove: id.clone().into(),
    };
    let (cove, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
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
        (status = 403, description = "Cove is system-owned and cannot be deleted via REST", body = ErrorBody),
        (status = 404, description = "Cove not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_cove(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Issue #175 followup — refuse to delete the singleton system cove
    // via the REST surface. The underlying `cove_delete_tx` is a low-level
    // primitive that trusts its caller (the same helper is reachable from
    // server-internal sites like replay fixtures); the policy decision
    // "system coves are not user-deletable" lives at the handler boundary
    // here. We pre-check via `cove_get` rather than threading the kind
    // through `_tx`'s WHERE clause because:
    //   * the read is cheap (single row, indexed by PK),
    //   * a transactional check would still need this surface to translate
    //     "no row affected because kind='system'" into a 403 rather than
    //     the txn's natural 404 — same code-shape, same trip to the DB,
    //     and the handler check fails fast without opening a write txn.
    if let Some(target) = s.repo.cove_get(&id).await?
        && target.kind == CoveKind::System
    {
        return Err(CalmError::Forbidden(format!(
            "cove {id} is system-owned and cannot be deleted via the public API"
        )));
    }

    let waves = s.repo.waves_by_cove(&id).await?;
    let wave_ids = waves
        .iter()
        .map(|wave| wave.id.as_str())
        .collect::<Vec<_>>();
    // Defensive TOCTOU guard only: this non-transactional read happens before
    // the teardown tx, so a forge-action can still become in-flight before the
    // sweep. It shrinks the race; durable parked recovery is the backstop, and
    // the airtight in-tx/lease-hold guard belongs to slice ⑤.
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("delete_cove forge-action fence requires sqlite-backed repo".into())
    })?;
    if any_wave_has_active_forge_action(&pool, &wave_ids).await? {
        return Err(CalmError::Conflict(format!(
            "cove {id} has a child wave with an in-flight forge-action; retry after it settles"
        )));
    }

    // Issue #197 — eager teardown for every terminal under the cove.
    // `terminals.card_id` is `ON DELETE RESTRICT` (migration 0011), so
    // a cove delete that would orphan a terminal row aborts the
    // surrounding txn unless we drain the table first. Walk
    // waves → cards → terminal_get_by_card; reap the daemon + socket
    // for each; collect the terminal ids for the in-txn row delete. The
    // overlay sweep derives current wave/card ids inside the write txn.
    let mut terminal_ids: Vec<String> = Vec::new();
    for wave in &waves {
        let cards = s.repo.cards_by_wave(wave.id.as_str()).await?;
        for card in &cards {
            if let Some(t) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
                reap_terminal_artifacts_with_renderer(Some(w.terminal_renderer.as_ref()), &t).await;
                terminal_ids.push(t.id);
            }
        }
    }

    let scope = EventScope::Cove {
        cove: id.clone().into(),
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
                overlay_delete_subtree_by_cove_tx(tx, &id).await?;
                overlay_delete_by_entity_tx(tx, "cove", &id).await?;
                let mut events = Vec::new();
                let mut sweeps = Vec::new();
                let wave_ids: Vec<String> =
                    sqlx::query_scalar("SELECT id FROM waves WHERE cove_id = ?1")
                        .bind(&id)
                        .fetch_all(&mut **tx)
                        .await?;
                for wave_id in &wave_ids {
                    let release =
                        release_workspace_leases_for_wave_tx(tx, wave_id.as_str()).await?;
                    events.extend(release.events);
                    if let Some(sweep) = release.sweep {
                        sweeps.push(sweep);
                    }
                }
                cove_delete_tx(tx, &id).await?;
                events.push((delete_actor, scope, Event::CoveDeleted { id: id.into() }));
                Ok((sweeps, events))
            })
        })
        .await?;
    sweep_workspace_worktrees_for_waves_repo(s.repo.as_ref(), &s.events, sweeps).await?;
    Ok(StatusCode::NO_CONTENT)
}

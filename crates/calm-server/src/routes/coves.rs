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
//! exclusively via the idempotent `POST /api/coves/system` upsert.

use crate::actor::Actor;
use crate::db::sqlite::{
    cove_create_system_tx, cove_create_tx, cove_delete_tx, cove_update_tx,
};
use crate::db::write_with_event_typed;
use crate::error::{ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::model::{Cove, CovePatch, NewCove};
use crate::state::AppState;
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
        .route("/api/coves/system", axum::routing::post(get_or_create_system_cove))
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
    State(s): State<AppState>,
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
    State(s): State<AppState>,
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
        &s.card_role_cache,
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
/// safely call it: the loser of the write race re-reads on retry.
///
/// The endpoint exists so the frontend's `useTodayTerminal` hook can
/// bootstrap a default terminal without exposing the underlying system
/// cove to the regular `POST /api/coves` surface (which the sidebar
/// "+ New cove" affordance consumes and which would otherwise need a
/// reserved-name policy).
pub(crate) async fn get_or_create_system_cove(
    State(s): State<AppState>,
    actor: Actor,
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
    // `create_cove`: the cove id is minted inside the closure).
    let (cove, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        EventScope::System,
        None,
        &s.events,
        &s.card_role_cache,
        move |tx| {
            Box::pin(async move {
                let cove = cove_create_system_tx(tx).await?;
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
    let scope = EventScope::Cove {
        cove: id.clone().into(),
    };
    let (cove, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
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
    let scope = EventScope::Cove {
        cove: id.clone().into(),
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
                cove_delete_tx(tx, &id).await?;
                Ok(((), Event::CoveDeleted { id: id.into() }))
            })
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

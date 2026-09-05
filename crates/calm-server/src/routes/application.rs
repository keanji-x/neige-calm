//! Production route assembly, shared with boundary integration tests.

use crate::actor::{actor_middleware, require_loopback_connect_info};
use crate::auth::{self, AuthState};
use crate::state::AppState;
use crate::{routes, ws};
use axum::Router;

/// Assemble authenticated REST/WS, loopback-only worker hooks and public routes.
/// CORS and static frontend serving are added by the binary outside this boundary.
pub fn application_router(state: AppState, auth_state: AuthState) -> Router {
    // Scope G — REST routes carry the `X-Calm-Actor` middleware so handler
    // writes get a declared actor (user / ai:<id>).
    //
    // Issue #189 — the protected REST subtree (everything except version
    // + openapi.json + the auth endpoints themselves) sits behind the
    // session middleware. Order matters: `actor_middleware` wraps
    // BEFORE `require_session` so the session check runs first; an
    // unauthenticated request never reaches the actor-validation code.
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(actor_middleware))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));

    // Internal worker hooks — loopback callbacks from codex/Claude worker
    // subprocesses. They carry `X-Calm-Actor` but no browser session cookie,
    // so they get actor + loopback validation and stay outside the human
    // session gate.
    let internal_rest = routes::internal_router()
        .layer(axum::middleware::from_fn(actor_middleware))
        .layer(axum::middleware::from_fn(require_loopback_connect_info));

    // WS routes — issue #189 — every upgrade handshake must carry a valid
    // session cookie (cookies are sent automatically with the WS upgrade
    // GET). The `actor_middleware` layer is NOT applied here because the
    // existing convention (see `actor.rs` doc) is that WS frames don't go
    // through the write-eventized path; we only enforce auth.
    let protected_ws = ws::router().layer(axum::middleware::from_fn_with_state(
        auth_state.clone(),
        auth::require_session_ws,
    ));

    // Public REST — version + openapi.json. No session gate, no actor
    // gate.
    let public_rest = routes::public_router();

    // Auth routes — login/whoami/logout. Public; mounted as a
    // separately-stated router because they consume `AuthState`, not
    // `AppState`.
    let auth_router = auth::router().with_state(auth_state.clone());

    axum::Router::new()
        .merge(protected_rest)
        .merge(internal_rest)
        .merge(protected_ws)
        .merge(public_rest)
        .with_state(state)
        .merge(auth_router)
}

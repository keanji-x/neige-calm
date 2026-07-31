//! HTTP route registry. Each sub-module (`coves`, `waves`, ...) returns its
//! own `Router<AppState>`; this file merges them.

use crate::openapi::ApiDoc;
use crate::state::AppState;
use axum::{Json, Router, routing::get};
use utoipa::OpenApi;

pub mod cards;
pub mod claude;
pub mod claude_cards;
pub mod codex;
pub mod codex_cards;
pub mod cove_folders;
pub mod coves;
pub mod fs;
pub mod overlays;
pub mod plugins;
pub mod settings;
pub mod terminal;
pub mod terminal_cards;
pub mod theme;
pub mod threads;
pub mod today;
pub mod version;
pub mod waves;

/// Full REST surface. Includes both protected (`protected_router`) and
/// public (`public_router`) trees. Kept as a single helper so tests and
/// downstream consumers that don't care about auth can mount everything
/// in one call (`Router::merge(routes::router())`).
///
/// The production binary in `main.rs` no longer uses this aggregator —
/// it merges `protected_router` (behind the session middleware) and
/// `public_router` (un-gated) separately so the session middleware can
/// be applied to exactly the right subset of paths. See `auth::router`
/// for the auth endpoints themselves, which sit outside both trees.
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(protected_router())
        .merge(internal_router())
        .merge(public_router())
}

/// Protected REST surface — everything that requires a valid session in
/// production. The auth login/whoami/logout endpoints are intentionally
/// NOT here (they live in `auth::router`), and `/api/version` +
/// `/api/openapi.json` are in [`public_router`] so a pre-auth client can
/// still read them (compat probes need version, openapi consumers want
/// the spec without logging in).
pub fn protected_router() -> Router<AppState> {
    Router::new()
        .merge(coves::router())
        .merge(cove_folders::router())
        .merge(waves::router())
        .merge(cards::router())
        .merge(overlays::router())
        .merge(plugins::router())
        .merge(terminal::router())
        .merge(terminal_cards::router())
        .merge(today::router())
        .merge(claude_cards::router())
        .merge(codex_cards::router())
        .merge(fs::router())
        .merge(settings::router())
}

/// Internal worker hook surface.
///
/// These endpoints are loopback callbacks from worker subprocesses, not
/// browser/user REST calls, so they must not sit behind the human session
/// gate. Their identity inputs are the `X-Calm-Actor` header validated by
/// `actor_middleware` plus the `card_id` query parameter resolved during
/// ingest; unknown cards are rejected by the role gate instead of being
/// accepted as anonymous/internal writes.
pub fn internal_router() -> Router<AppState> {
    Router::new()
        .merge(claude::router())
        .merge(codex::router())
        .merge(threads::router())
}

/// Public REST surface — endpoints that must remain reachable BEFORE
/// auth. Today: `/api/version` (frontend compat gate hits this before
/// it even knows whether it's logged in) and `/api/openapi.json` (build-
/// time tooling consumes this with no creds).
pub fn public_router() -> Router<AppState> {
    Router::new()
        .merge(version::router())
        // OpenAPI document — the source-of-truth for web-calm's generated
        // TypeScript types. No swagger-ui — just the spec, served as JSON
        // so the frontend toolchain can hit it during build.
        .route("/api/openapi.json", get(openapi_spec))
}

/// Serve the generated OpenAPI document. Computed once per request — the
/// document is small (a few KB) and `OpenApi` is `Send + Sync`, so we could
/// also cache it in `OnceLock`; keeping it simple for M1.
async fn openapi_spec() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

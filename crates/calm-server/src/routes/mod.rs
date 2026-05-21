//! HTTP route registry. Each sub-module (`coves`, `waves`, ...) returns its
//! own `Router<AppState>`; this file merges them.

use crate::openapi::ApiDoc;
use crate::state::AppState;
use axum::{Json, Router, routing::get};
use utoipa::OpenApi;

pub mod cards;
pub mod codex;
pub mod coves;
pub mod fs;
pub mod overlays;
pub mod plugins;
pub mod settings;
pub mod terminal;
pub mod version;
pub mod waves;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(coves::router())
        .merge(waves::router())
        .merge(cards::router())
        .merge(overlays::router())
        .merge(plugins::router())
        .merge(terminal::router())
        .merge(codex::router())
        .merge(fs::router())
        .merge(settings::router())
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

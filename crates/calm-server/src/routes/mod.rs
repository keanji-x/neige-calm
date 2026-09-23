//! HTTP route registry: merges each sub-module's `Router<AppState>`.

use crate::openapi::ApiDoc;
use crate::state::AppState;
use axum::{Json, Router, routing::get};
use utoipa::OpenApi;

mod application;
pub use application::application_router;
pub use application::public_mobile_router;

pub mod area_folders;
pub mod areas;
pub mod cards;
pub mod claude;
pub mod claude_cards;
pub mod codex;
pub mod codex_cards;
pub mod conversations_shared;
pub mod fs;
pub mod isolated_tasks;
pub mod models;
pub mod overlays;
pub mod planner_input;
pub mod planner_model;
pub mod plugins;
pub mod settings;
pub mod task_artifacts;
pub mod task_recovery;
pub mod terminal;
pub mod terminal_cards;
pub mod theme;
pub mod threads;
pub mod today;
pub mod today_summary;
pub mod track_conversations;
pub mod track_previews;
pub mod track_recipes;
pub mod track_report_blocks;
pub mod track_report_series;
pub mod track_sources;
pub mod track_templates;
pub mod tracks;
pub mod version;

/// Full REST surface, protected and public trees together; the production binary
/// uses [`application_router`], which gates the protected surface.
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(protected_router())
        .merge(internal_router())
        .merge(public_router())
}

/// Protected REST surface — everything that requires a valid session in production.
pub fn protected_router() -> Router<AppState> {
    Router::new()
        .merge(areas::router())
        .merge(area_folders::router())
        .merge(tracks::router())
        .merge(track_conversations::router())
        .merge(track_previews::router())
        .merge(track_report_blocks::router())
        .merge(track_report_series::router())
        .merge(track_sources::router())
        .merge(task_recovery::router())
        .merge(task_artifacts::router())
        .merge(isolated_tasks::router())
        .merge(track_recipes::router())
        .merge(track_templates::router())
        .merge(cards::router())
        .merge(crate::planner_attachments::routes::router())
        .merge(overlays::router())
        .merge(plugins::router())
        .merge(terminal::router())
        .merge(terminal_cards::router())
        .merge(today::router())
        .merge(today_summary::router())
        .merge(claude_cards::router())
        .merge(codex_cards::router())
        .merge(fs::router())
        .merge(models::router())
        .merge(settings::router())
}

/// Internal worker hook surface: loopback callbacks from worker subprocesses, so not
/// behind the human session gate; identity comes from `X-Calm-Actor` plus `card_id`.
pub fn internal_router() -> Router<AppState> {
    Router::new()
        .merge(claude::router())
        .merge(codex::router())
        .merge(threads::router())
}

/// Public REST surface — endpoints that must remain reachable BEFORE auth.
pub fn public_router() -> Router<AppState> {
    Router::new()
        .merge(version::router())
        .route("/api/openapi.json", get(openapi_spec))
}

/// Serve the generated OpenAPI document.
async fn openapi_spec() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

mod planner_recovery;

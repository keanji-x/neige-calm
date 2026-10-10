//! `GET /api/claude/installed-version` and `POST /api/claude/installed-version/refresh` (#2516):
//! the installed Claude version a Claude card's Update compares its child's version against. The
//! read serves the cache; only the refresh runs the CLI, at most once a minute.

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use calm_types::runtime::ClaudeInstalledVersion;

use crate::error::{CalmError, Result};
use crate::extract::Json;
use crate::state::{AppState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/claude/installed-version",
            get(get_claude_installed_version),
        )
        .route(
            "/api/claude/installed-version/refresh",
            post(refresh_claude_installed_version),
        )
}

/// The installed Claude version as the server last saw it: refreshed at boot, every 10 minutes,
/// after a Claude card's Update, and by the refresh route. Never runs the CLI; `version` is null
/// until one run has answered.
#[utoipa::path(
    get,
    path = "/api/claude/installed-version",
    tag = "claude",
    responses(
        (status = 200, description = "The cached answer of `claude --version`", body = ClaudeInstalledVersion),
    ),
)]
pub(crate) async fn get_claude_installed_version(
    State(s): State<RouteState>,
) -> Json<ClaudeInstalledVersion> {
    Json(ClaudeInstalledVersion {
        version: s.claude_installed_version.installed(),
    })
}

/// Run `claude --version` again and keep its answer, unless the last such run was less than a
/// minute ago. Answers once the run has finished (or was skipped), so a read after it sees the
/// answer; a failed run keeps the last version.
#[utoipa::path(
    post,
    path = "/api/claude/installed-version/refresh",
    tag = "claude",
    responses(
        (status = 204, description = "Refreshed, or skipped within a minute of the last refresh"),
    ),
)]
pub(crate) async fn refresh_claude_installed_version(
    State(s): State<RouteState>,
) -> Result<StatusCode> {
    // An owned task: a client that goes away does not cancel the run.
    let cache = s.claude_installed_version.clone();
    tokio::spawn(async move { cache.refresh_on_demand(tokio::time::Instant::now()).await })
        .await
        .map_err(|error| {
            CalmError::Internal(format!("claude version refresh task failed: {error}"))
        })?;
    Ok(StatusCode::NO_CONTENT)
}

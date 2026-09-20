//! The browser's read of a track's captured sources: `GET /api/tracks/{id}/sources`
//! (the list, no bodies) and `GET /api/tracks/{id}/sources/{source_id}`. Every read is
//! one autocommit statement on the pool.

use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::report_sources::{
    Detail, SourceRow, TrackSourceDetail, TrackSourceList, TrackSourceSummary, captured_at_text,
    store,
};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks/{id}/sources", get(list_track_sources))
        .route(
            "/api/tracks/{id}/sources/{source_id}",
            get(get_track_source),
        )
}

fn summary_of(row: &SourceRow) -> TrackSourceSummary {
    TrackSourceSummary {
        source_id: row.source_id.clone(),
        provenance: row.provenance,
        origin: row.origin.clone(),
        title: row.title.clone(),
        published_at: row.published_at.clone(),
        content_id: row.origin.content_id().map(str::to_string),
        url: row.origin.url().map(str::to_string),
        body_bytes: row.body_bytes,
        body_sha256: row.body_sha256.clone(),
        captured_at: captured_at_text(row.captured_at),
        quotes: row.quotes.clone(),
    }
}

async fn pool_and_track(state: &RouteState, id: &str) -> Result<sqlx::SqlitePool> {
    state
        .repo
        .track_get(id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    state.mcp_context.sqlite_pool.clone().ok_or_else(|| {
        CalmError::Internal("report_sources: route requires a sqlite-backed repo".into())
    })
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/sources",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "The track's captured sources, oldest first, without bodies", body = TrackSourceList),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_track_sources(
    State(state): State<RouteState>,
    _principal: Principal,
    Path(id): Path<String>,
) -> Result<Response> {
    let pool = pool_and_track(&state, &id).await?;
    let rows = store::list(&pool, &id).await?;
    let sources = rows.iter().map(summary_of).collect();
    Ok((StatusCode::OK, Json(TrackSourceList { sources })).into_response())
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/sources/{source_id}",
    tag = "tracks",
    params(
        ("id" = String, Path, description = "Track id"),
        ("source_id" = String, Path, description = "The source (`src_` + 8 hex)"),
    ),
    responses(
        (status = 200, description = "The source with its stored body and anchors", body = TrackSourceDetail),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 404, description = "Track not found, or no such source in this track", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_track_source(
    State(state): State<RouteState>,
    _principal: Principal,
    Path((id, source_id)): Path<(String, String)>,
) -> Result<Response> {
    let pool = pool_and_track(&state, &id).await?;
    let row = store::get(&pool, &id, &source_id, Detail::Full)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id} has no source {source_id}")))?;
    let detail =
        TrackSourceDetail::from_summary(summary_of(&row), row.body.clone().unwrap_or_default());
    Ok((StatusCode::OK, Json(detail)).into_response())
}

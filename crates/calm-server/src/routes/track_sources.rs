//! #1669 §2.4 — the browser's read of a track's captured sources:
//! `GET /api/tracks/{id}/sources` (the list, no bodies) and
//! `GET /api/tracks/{id}/sources/{source_id}` (one source with its body
//! and anchors). Protected router, single-owner session (`Principal`), the
//! same shape as `track_report_series`: unauthenticated is 401, a source
//! of another track is 404.
//!
//! Every read is one autocommit statement on the pool (#930: no deferred
//! read transaction in production code).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::report_sources::{Detail, Origin, Provenance, SourceRow, captured_at_text, store};
use crate::state::{AppState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks/{id}/sources", get(list_track_sources))
        .route(
            "/api/tracks/{id}/sources/{source_id}",
            get(get_track_source),
        )
}

/// One anchor of a source: `text` is a byte-exact substring of the body.
/// `start`/`end` are UTF-8 byte offsets into `body` — a kernel-side
/// detail; the page locates the anchor by `text`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceQuote {
    pub id: String,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// A captured source without its body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackSourceSummary {
    pub source_id: String,
    pub provenance: Provenance,
    pub origin: Origin,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    /// `origin.content_id`, surfaced for the panel header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// `origin.url` (manual sources only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub body_bytes: usize,
    pub body_sha256: String,
    /// RFC 3339, UTC.
    pub captured_at: String,
    pub quotes: Vec<SourceQuote>,
}

/// A captured source with its body: the raw text the kernel stored,
/// verbatim (not Markdown-rendered by the panel).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackSourceDetail {
    #[serde(flatten)]
    pub summary: TrackSourceSummary,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackSourceList {
    pub sources: Vec<TrackSourceSummary>,
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
        quotes: row
            .quotes
            .iter()
            .map(|quote| SourceQuote {
                id: quote.id.clone(),
                text: quote.text.clone(),
                start: quote.start,
                end: quote.end,
            })
            .collect(),
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
    let detail = TrackSourceDetail {
        summary: summary_of(&row),
        body: row.body.clone().unwrap_or_default(),
    };
    Ok((StatusCode::OK, Json(detail)).into_response())
}

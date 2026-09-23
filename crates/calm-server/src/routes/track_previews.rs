//! `GET /api/tracks/{id}/previews`: the track's #1780 preview gateway registrations, each with
//! a `live` probe of its loopback target. In-memory and ephemeral, so the front end polls.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;
use tokio::net::TcpStream;
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::ids::TrackId;
use crate::state::{AppState, RouteState};

/// Bound on one target's TCP connect probe; the probes run concurrently.
pub const LIVE_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

pub fn router() -> Router<AppState> {
    Router::new().route("/api/tracks/{id}/previews", get(list_track_previews))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TrackPreview {
    pub key: String,
    pub title: String,
    /// The gateway pool port the browser loads.
    pub port: u16,
    /// Whether the dev server behind it accepted a TCP connection just now.
    pub live: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TrackPreviews {
    /// Ordered by `port`.
    pub previews: Vec<TrackPreview>,
}

async fn target_live(target_port: u16) -> bool {
    let connect = TcpStream::connect(("127.0.0.1", target_port));
    matches!(
        tokio::time::timeout(LIVE_PROBE_TIMEOUT, connect).await,
        Ok(Ok(_))
    )
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/previews",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "The track's registered previews", body = TrackPreviews),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_track_previews(
    State(state): State<RouteState>,
    _principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<TrackPreviews>> {
    state
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    let held = state.mcp_context.preview.for_track(&TrackId::from(id));
    let probes = held.iter().map(|(_, entry)| target_live(entry.target_port));
    let live = futures::future::join_all(probes).await;
    let previews = held
        .into_iter()
        .zip(live)
        .map(|((port, entry), live)| TrackPreview {
            key: entry.key,
            title: entry.title,
            port,
            live,
        })
        .collect();
    Ok(Json(TrackPreviews { previews }))
}

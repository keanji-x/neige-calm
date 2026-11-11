//! The browser's read of a track's captured sources: `GET /api/tracks/{id}/sources`
//! (the list, no bodies) and `GET /api/tracks/{id}/sources/{source_id}`. Every read is
//! one autocommit statement on the pool.

use crate::auth::Principal;
use crate::error::{ErrorBody, Result};
use crate::extract::Path;
use crate::report_sources::{TrackSourceDetail, TrackSourceList};
use crate::state::{AppState, RouteState};
use axum::{Router, extract::State, response::Response, routing::get};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks/{id}/sources", get(list_track_sources))
        .route(
            "/api/tracks/{id}/sources/{source_id}",
            get(get_track_source),
        )
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/sources",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "The track's captured sources, oldest first, without bodies", body = TrackSourceList),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Current Area access refused", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_track_sources(
    State(state): State<RouteState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Response> {
    crate::report_resource::list_track_sources(
        &state,
        crate::read_access::Audience::Owner(&principal),
        id,
    )
    .await
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
        (status = 403, description = "Current Area access refused", body = ErrorBody),
        (status = 404, description = "Track not found, or no such source in this track", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_track_source(
    State(state): State<RouteState>,
    principal: Principal,
    Path((id, source_id)): Path<(String, String)>,
) -> Result<Response> {
    crate::report_resource::get_track_source(
        &state,
        crate::read_access::Audience::Owner(&principal),
        id,
        source_id,
    )
    .await
}

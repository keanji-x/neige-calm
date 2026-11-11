//! Protected share management; no token-authenticated or anonymous surface.
use crate::auth::HumanInputSource;
use crate::error::{ErrorBody, Result};
use crate::extract::{Json, JsonBody, Path};
use crate::state::{AppState, RouteState};
use axum::{
    Router,
    extract::State,
    routing::{delete, get},
};
use calm_types::share_links::{CreateShareLink, ShareLink, ShareLinkCreated, ShareScope};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/tracks/{id}/share-links",
            get(list_track_share_links).post(create_track_share_link),
        )
        .route(
            "/api/areas/{id}/share-links",
            get(list_area_share_links).post(create_area_share_link),
        )
        .route("/api/share-links/{id}", delete(revoke_share_link))
}

#[utoipa::path(
    post, path = "/api/tracks/{id}/share-links", tag = "share_links",
    params(("id" = String, Path, description = "Track id")),
    request_body = CreateShareLink,
    responses(
        (status = 200, description = "Share management result", body = ShareLinkCreated),
        (status = 403, description = "Current deployment and Area owner required", body = ErrorBody),
        (status = 404, description = "Target not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    )
)]
pub(crate) async fn create_track_share_link(
    State(s): State<RouteState>,
    source: HumanInputSource,
    Path(id): Path<String>,
    JsonBody(_body): JsonBody<CreateShareLink>,
) -> Result<Json<ShareLinkCreated>> {
    Ok(Json(
        crate::share_access::create(s.repo.as_ref(), source, ShareScope::Track(id)).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/tracks/{id}/share-links", tag = "share_links",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Share management result", body = Vec<ShareLink>),
        (status = 403, description = "Current deployment and Area owner required", body = ErrorBody),
        (status = 404, description = "Target not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    )
)]
pub(crate) async fn list_track_share_links(
    State(s): State<RouteState>,
    source: HumanInputSource,
    Path(id): Path<String>,
) -> Result<Json<Vec<ShareLink>>> {
    Ok(Json(
        crate::share_access::list(s.repo.as_ref(), source, ShareScope::Track(id)).await?,
    ))
}

#[utoipa::path(
    post, path = "/api/areas/{id}/share-links", tag = "share_links",
    params(("id" = String, Path, description = "Area id")),
    request_body = CreateShareLink,
    responses(
        (status = 200, description = "Share management result", body = ShareLinkCreated),
        (status = 403, description = "Current deployment and Area owner required", body = ErrorBody),
        (status = 404, description = "Target not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    )
)]
pub(crate) async fn create_area_share_link(
    State(s): State<RouteState>,
    source: HumanInputSource,
    Path(id): Path<String>,
    JsonBody(_body): JsonBody<CreateShareLink>,
) -> Result<Json<ShareLinkCreated>> {
    Ok(Json(
        crate::share_access::create(s.repo.as_ref(), source, ShareScope::Area(id)).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/areas/{id}/share-links", tag = "share_links",
    params(("id" = String, Path, description = "Area id")),
    responses(
        (status = 200, description = "Share management result", body = Vec<ShareLink>),
        (status = 403, description = "Current deployment and Area owner required", body = ErrorBody),
        (status = 404, description = "Target not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    )
)]
pub(crate) async fn list_area_share_links(
    State(s): State<RouteState>,
    source: HumanInputSource,
    Path(id): Path<String>,
) -> Result<Json<Vec<ShareLink>>> {
    Ok(Json(
        crate::share_access::list(s.repo.as_ref(), source, ShareScope::Area(id)).await?,
    ))
}

#[utoipa::path(
    delete, path = "/api/share-links/{id}", tag = "share_links",
    params(("id" = String, Path, description = "Share link id")),
    responses(
        (status = 200, description = "Access revoked; delegation retained", body = ShareLink),
        (status = 403, description = "Current deployment and Area owner required", body = ErrorBody),
        (status = 404, description = "Share or target not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    )
)]
pub(crate) async fn revoke_share_link(
    State(s): State<RouteState>,
    source: HumanInputSource,
    Path(id): Path<String>,
) -> Result<Json<ShareLink>> {
    let link = crate::share_access::revoke(s.repo.as_ref(), source, id).await?;
    s.events.wake_access_readers();
    Ok(Json(link))
}

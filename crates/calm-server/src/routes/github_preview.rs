//! Protected, read-only GitHub citations. Policy and the credential boundary live in github_preview.
use crate::{
    error::{ErrorBody, Result},
    github_preview::{GitHubPreview, PreviewQuery},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::Query,
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/github/preview", get(read))
}

#[utoipa::path(get, path = "/api/github/preview", operation_id = "read_github_preview", tag = "github", params(PreviewQuery),
    responses((status = 200, description = "Issue or PR summary", body = GitHubPreview),
        (status = 400, description = "Invalid GitHub reference", body = ErrorBody),
        (status = 503, description = "GitHub unavailable or inaccessible", body = ErrorBody)))]
pub(crate) async fn read(Query(query): Query<PreviewQuery>) -> Result<Response> {
    let summary = crate::github_preview::read(query).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(summary)).into_response())
}

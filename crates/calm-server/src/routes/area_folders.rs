//! `/api/areas/:area_id/folders` + `/api/areas/resolve` — area ↔ folder
//! mapping. **Issue #250 PR 1.**
//!
//! A `area_folder` claims an absolute filesystem path for an area and
//! transparently covers every descendant. Given a `cwd`, the kernel
//! resolves the owning area by finding the row whose claim covers it.
//! Claims are exclusive: a path may be claimed by at most one area, and
//! ancestor/descendant overlap is rejected at create time — *atomically*,
//! inside the same `BEGIN IMMEDIATE` transaction as the INSERT — so the
//! covering row is unique and needs no tiebreak (issue #275).
//!
//! The claim rules themselves live in [`calm_truth::area_folder_claim`]
//! so this route and the wave-create `attach_folder` path in
//! [`crate::routes::waves`] cannot drift apart.
//!
//! These endpoints sit outside the event-sourced sync domain in PR 1
//! — folders are operational mapping state, not co-edit content. PR 2+
//! may revisit if a replication scenario emerges.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{AreaFolder, AreaResolve, FolderConflict, NewAreaFolder};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use calm_truth::area_folder_claim::AreaFolderClaim;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

pub fn router() -> Router<AppState> {
    Router::new()
        // `/resolve` must be registered BEFORE `/{area_id}/folders/...`
        // so axum's longest-match router doesn't capture `resolve` as
        // an area id and fail with a path-param decode error. Mounting
        // here at the same Router level is sufficient — axum prefers
        // static path segments over `{param}` captures.
        .route("/api/areas/resolve", get(resolve_path))
        .route(
            "/api/areas/{area_id}/folders",
            get(list_folders).post(create_folder),
        )
        .route(
            "/api/areas/{area_id}/folders/{folder_id}",
            axum::routing::delete(delete_folder),
        )
}

// Path/overlap vocabulary is owned by calm-truth so the repo's atomic
// writer and both HTTP resolvers share one definition (#275).
pub(crate) use calm_truth::area_folder_claim::{find_owner, is_descendant_of, normalize_path};

// ---------------------------------------------------------------------------
// GET /api/areas/:area_id/folders
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/areas/{area_id}/folders",
    tag = "area_folders",
    params(("area_id" = String, Path, description = "Area id")),
    responses(
        (status = 200, description = "Folders claimed by this area, sorted by path", body = Vec<AreaFolder>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_folders(
    State(s): State<RouteState>,
    Path(area_id): Path<String>,
) -> Result<Json<Vec<AreaFolder>>> {
    let folders = s.repo.area_folders_by_area(&area_id).await?;
    Ok(Json(folders))
}

// ---------------------------------------------------------------------------
// POST /api/areas/:area_id/folders
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/areas/{area_id}/folders",
    tag = "area_folders",
    params(("area_id" = String, Path, description = "Area id")),
    request_body = NewAreaFolder,
    responses(
        (status = 201, description = "Folder claimed", body = AreaFolder),
        (status = 400, description = "Path is not absolute", body = ErrorBody),
        (status = 409, description = "Path overlaps with an existing claim", body = FolderConflict),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn create_folder(
    State(s): State<RouteState>,
    Path(area_id): Path<String>,
    Json(body): Json<NewAreaFolder>,
) -> Result<Response> {
    if !body.path.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "path must be absolute (start with `/`); got `{}`",
            body.path
        )));
    }
    let normalized = normalize_path(&body.path);

    // Conflict detection + INSERT are ONE atomic step inside the repo
    // (#275). Doing the scan here and the insert there would put them on
    // two different pooled connections, and `UNIQUE(area_folders.path)`
    // only rejects an *equal* path — so concurrent `/a` and `/a/b`
    // claims would both commit and leave two rows covering `/a/b/c`.
    //
    // The scan itself is still an in-memory pass over the whole table:
    // it is tiny (a handful of folders per workspace) and that keeps the
    // SQL free of LIKE-pattern subtleties around `_` / `%` in user paths.
    match s
        .repo
        .area_folder_create_checked(&area_id, &normalized)
        .await?
    {
        AreaFolderClaim::Conflict(body) => Ok((StatusCode::CONFLICT, Json(body)).into_response()),
        AreaFolderClaim::Created(folder) => Ok((StatusCode::CREATED, Json(folder)).into_response()),
    }
}

// ---------------------------------------------------------------------------
// DELETE /api/areas/:area_id/folders/:folder_id
// ---------------------------------------------------------------------------

#[utoipa::path(
    delete,
    path = "/api/areas/{area_id}/folders/{folder_id}",
    tag = "area_folders",
    params(
        ("area_id" = String, Path, description = "Area id"),
        ("folder_id" = i64, Path, description = "Folder id"),
    ),
    responses(
        (status = 204, description = "Folder removed"),
        (status = 404, description = "Folder not found under this area", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_folder(
    State(s): State<RouteState>,
    Path((area_id, folder_id)): Path<(String, i64)>,
) -> Result<StatusCode> {
    // Verify the folder both exists and belongs to the area in the
    // URL. Mismatched area_id surfaces as 404 (not 403) — leaking
    // existence under a different area is the wrong answer here.
    match s.repo.area_folder_get(folder_id).await? {
        Some(f) if f.area_id.as_str() == area_id => {}
        _ => return Err(CalmError::NotFound(format!("area_folder {folder_id}"))),
    }
    s.repo.area_folder_delete(folder_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// GET /api/areas/resolve?path=<cwd>
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct ResolveQuery {
    /// Absolute filesystem path to resolve against every area's folder
    /// claims. Returns the claim that covers it, or `null` if no claim
    /// covers the path. At most one claim can cover a path: the create
    /// endpoint rejects ancestor/descendant overlap with a 409.
    pub path: String,
}

#[utoipa::path(
    get,
    path = "/api/areas/resolve",
    tag = "area_folders",
    params(ResolveQuery),
    responses(
        (status = 200, description = "The area + folder whose claim covers the path, or null when no claim covers it. Overlapping claims are rejected at create time, so at most one claim can match.", body = Option<AreaResolve>),
        (status = 400, description = "Path is not absolute", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn resolve_path(
    State(s): State<RouteState>,
    Query(q): Query<ResolveQuery>,
) -> Result<Json<Option<AreaResolve>>> {
    if !q.path.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "path must be absolute (start with `/`); got `{}`",
            q.path
        )));
    }
    let normalized = normalize_path(&q.path);
    let folders = s.repo.area_folders_list_all().await?;
    // `area_folder_create_checked` rejects ancestor/descendant overlap
    // atomically, so at most one row can be an ancestor of (or equal to)
    // the query. `find_owner` is therefore a uniqueness oracle and needs
    // no tiebreak — and it is the *same* function the wave-create owner
    // scan calls, so the two resolvers cannot disagree (issue #275).
    let best = find_owner(&folders, &normalized).cloned();
    Ok(Json(best.map(|f| AreaResolve {
        area_id: f.area_id,
        folder_id: f.id,
        folder_path: f.path,
    })))
}

// `normalize_path` / `is_descendant_of` / `find_owner` unit tests live
// next to their definitions in `calm_truth::area_folder_claim`.

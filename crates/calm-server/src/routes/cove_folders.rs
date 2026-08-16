//! `/api/coves/:cove_id/folders` + `/api/coves/resolve` — cove ↔ folder
//! mapping. **Issue #250 PR 1.**
//!
//! A `cove_folder` claims an absolute filesystem path for a cove and
//! transparently covers every descendant. Given a `cwd`, the kernel
//! resolves the owning cove by finding the row whose claim covers it.
//! Claims are exclusive: a path may be claimed by at most one cove, and
//! ancestor/descendant overlap is rejected at create time — *atomically*,
//! inside the same `BEGIN IMMEDIATE` transaction as the INSERT — so the
//! covering row is unique and needs no tiebreak (issue #275).
//!
//! The claim rules themselves live in [`calm_truth::cove_folder_claim`]
//! so this route and the wave-create `attach_folder` path in
//! [`crate::routes::waves`] cannot drift apart.
//!
//! These endpoints sit outside the event-sourced sync domain in PR 1
//! — folders are operational mapping state, not co-edit content. PR 2+
//! may revisit if a replication scenario emerges.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{CoveFolder, CoveResolve, FolderConflict, NewCoveFolder};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use calm_truth::cove_folder_claim::CoveFolderClaim;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

pub fn router() -> Router<AppState> {
    Router::new()
        // `/resolve` must be registered BEFORE `/{cove_id}/folders/...`
        // so axum's longest-match router doesn't capture `resolve` as
        // a cove id and fail with a path-param decode error. Mounting
        // here at the same Router level is sufficient — axum prefers
        // static path segments over `{param}` captures.
        .route("/api/coves/resolve", get(resolve_path))
        .route(
            "/api/coves/{cove_id}/folders",
            get(list_folders).post(create_folder),
        )
        .route(
            "/api/coves/{cove_id}/folders/{folder_id}",
            axum::routing::delete(delete_folder),
        )
}

// Path/overlap vocabulary is owned by calm-truth so the repo's atomic
// writer and both HTTP resolvers share one definition (#275).
pub(crate) use calm_truth::cove_folder_claim::{find_owner, is_descendant_of, normalize_path};

// ---------------------------------------------------------------------------
// GET /api/coves/:cove_id/folders
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/coves/{cove_id}/folders",
    tag = "cove_folders",
    params(("cove_id" = String, Path, description = "Cove id")),
    responses(
        (status = 200, description = "Folders claimed by this cove, sorted by path", body = Vec<CoveFolder>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_folders(
    State(s): State<RouteState>,
    Path(cove_id): Path<String>,
) -> Result<Json<Vec<CoveFolder>>> {
    let folders = s.repo.cove_folders_by_cove(&cove_id).await?;
    Ok(Json(folders))
}

// ---------------------------------------------------------------------------
// POST /api/coves/:cove_id/folders
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/coves/{cove_id}/folders",
    tag = "cove_folders",
    params(("cove_id" = String, Path, description = "Cove id")),
    request_body = NewCoveFolder,
    responses(
        (status = 201, description = "Folder claimed", body = CoveFolder),
        (status = 400, description = "Path is not absolute", body = ErrorBody),
        (status = 409, description = "Path overlaps with an existing claim", body = FolderConflict),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn create_folder(
    State(s): State<RouteState>,
    Path(cove_id): Path<String>,
    Json(body): Json<NewCoveFolder>,
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
    // two different pooled connections, and `UNIQUE(cove_folders.path)`
    // only rejects an *equal* path — so concurrent `/a` and `/a/b`
    // claims would both commit and leave two rows covering `/a/b/c`.
    //
    // The scan itself is still an in-memory pass over the whole table:
    // it is tiny (a handful of folders per workspace) and that keeps the
    // SQL free of LIKE-pattern subtleties around `_` / `%` in user paths.
    match s
        .repo
        .cove_folder_create_checked(&cove_id, &normalized)
        .await?
    {
        CoveFolderClaim::Conflict(body) => Ok((StatusCode::CONFLICT, Json(body)).into_response()),
        CoveFolderClaim::Created(folder) => Ok((StatusCode::CREATED, Json(folder)).into_response()),
    }
}

// ---------------------------------------------------------------------------
// DELETE /api/coves/:cove_id/folders/:folder_id
// ---------------------------------------------------------------------------

#[utoipa::path(
    delete,
    path = "/api/coves/{cove_id}/folders/{folder_id}",
    tag = "cove_folders",
    params(
        ("cove_id" = String, Path, description = "Cove id"),
        ("folder_id" = i64, Path, description = "Folder id"),
    ),
    responses(
        (status = 204, description = "Folder removed"),
        (status = 404, description = "Folder not found under this cove", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_folder(
    State(s): State<RouteState>,
    Path((cove_id, folder_id)): Path<(String, i64)>,
) -> Result<StatusCode> {
    // Verify the folder both exists and belongs to the cove in the
    // URL. Mismatched cove_id surfaces as 404 (not 403) — leaking
    // existence under a different cove is the wrong answer here.
    match s.repo.cove_folder_get(folder_id).await? {
        Some(f) if f.cove_id.as_str() == cove_id => {}
        _ => return Err(CalmError::NotFound(format!("cove_folder {folder_id}"))),
    }
    s.repo.cove_folder_delete(folder_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// GET /api/coves/resolve?path=<cwd>
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct ResolveQuery {
    /// Absolute filesystem path to resolve against every cove's folder
    /// claims. Returns the claim that covers it, or `null` if no claim
    /// covers the path. At most one claim can cover a path: the create
    /// endpoint rejects ancestor/descendant overlap with a 409.
    pub path: String,
}

#[utoipa::path(
    get,
    path = "/api/coves/resolve",
    tag = "cove_folders",
    params(ResolveQuery),
    responses(
        (status = 200, description = "The cove + folder whose claim covers the path, or null when no claim covers it. Overlapping claims are rejected at create time, so at most one claim can match.", body = Option<CoveResolve>),
        (status = 400, description = "Path is not absolute", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn resolve_path(
    State(s): State<RouteState>,
    Query(q): Query<ResolveQuery>,
) -> Result<Json<Option<CoveResolve>>> {
    if !q.path.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "path must be absolute (start with `/`); got `{}`",
            q.path
        )));
    }
    let normalized = normalize_path(&q.path);
    let folders = s.repo.cove_folders_list_all().await?;
    // `cove_folder_create_checked` rejects ancestor/descendant overlap
    // atomically, so at most one row can be an ancestor of (or equal to)
    // the query. `find_owner` is therefore a uniqueness oracle and needs
    // no tiebreak — and it is the *same* function the wave-create owner
    // scan calls, so the two resolvers cannot disagree (issue #275).
    let best = find_owner(&folders, &normalized).cloned();
    Ok(Json(best.map(|f| CoveResolve {
        cove_id: f.cove_id,
        folder_id: f.id,
        folder_path: f.path,
    })))
}

// `normalize_path` / `is_descendant_of` / `find_owner` unit tests live
// next to their definitions in `calm_truth::cove_folder_claim`.

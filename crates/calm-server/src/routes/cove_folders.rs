//! `/api/coves/:cove_id/folders` + `/api/coves/resolve` — cove ↔ folder
//! mapping. **Issue #250 PR 1.**
//!
//! A `cove_folder` claims an absolute filesystem path for a cove and
//! transparently covers every descendant. Given a `cwd`, the kernel
//! resolves the owning cove by longest-prefix matching against every
//! row in the table. Claims are exclusive: a path may be claimed by
//! at most one cove, and ancestor/descendant overlap is rejected at
//! create time.
//!
//! These endpoints sit outside the event-sourced sync domain in PR 1
//! — folders are operational mapping state, not co-edit content. PR 2+
//! may revisit if a replication scenario emerges.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{CoveFolder, CoveResolve, FolderConflict, FolderConflictKind, NewCoveFolder};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
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

/// Normalize an absolute filesystem path for storage / comparison.
///
/// * Trims exactly one trailing slash unless the entire string is the
///   root `/`.
/// * Does **not** validate that the path starts with `/` — that's a
///   separate concern surfaced as a 400 in the handler so the wire
///   error code is precise.
pub(crate) fn normalize_path(raw: &str) -> String {
    if raw == "/" {
        return "/".to_string();
    }
    if let Some(stripped) = raw.strip_suffix('/') {
        return stripped.to_string();
    }
    raw.to_string()
}

/// True when `candidate` is a descendant of `parent` (or equal).
/// Implementation: `parent == candidate` OR `candidate` starts with
/// `parent + "/"`. The `+ "/"` guard prevents `/abc` from matching
/// against parent `/ab`.
pub(crate) fn is_descendant_of(parent: &str, candidate: &str) -> bool {
    if parent == candidate {
        return true;
    }
    // Root `/` is a special case — every absolute path is a descendant
    // of it, but naive `candidate.starts_with("/")` is trivially true,
    // so the join below would still produce `"//..."`. Handle directly.
    if parent == "/" {
        return candidate.starts_with('/');
    }
    candidate.starts_with(&format!("{parent}/"))
}

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

    // Conflict detection: scan every existing folder and classify any
    // overlap. The expected table size is tiny (handful of folders
    // per workspace at most) so an in-memory pass keeps the SQL simple
    // and avoids LIKE-pattern subtleties around `_` / `%` in user paths.
    let existing = s.repo.cove_folders_list_all().await?;
    for f in &existing {
        let conflict_kind = if f.path == normalized {
            Some(FolderConflictKind::Equal)
        } else if is_descendant_of(&normalized, &f.path) {
            Some(FolderConflictKind::Ancestor)
        } else if is_descendant_of(&f.path, &normalized) {
            Some(FolderConflictKind::Descendant)
        } else {
            None
        };
        if let Some(kind) = conflict_kind {
            let body = FolderConflict {
                folder_id: f.id,
                cove_id: f.cove_id.clone(),
                conflict_path: f.path.clone(),
                conflict_kind: kind,
            };
            return Ok((StatusCode::CONFLICT, Json(body)).into_response());
        }
    }

    let folder = s.repo.cove_folder_create(&cove_id, &normalized).await?;
    Ok((StatusCode::CREATED, Json(folder)).into_response())
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
    /// claims. Returns the most-specific claim that covers it (longest
    /// prefix), or `null` if no claim covers the path.
    pub path: String,
}

#[utoipa::path(
    get,
    path = "/api/coves/resolve",
    tag = "cove_folders",
    params(ResolveQuery),
    responses(
        (status = 200, description = "Owning cove + folder, or null when no claim covers the path", body = Option<CoveResolve>),
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
    // Longest-prefix match: keep the folder whose `path` is an ancestor
    // (or equal to) the query AND has the longest `path` among matches.
    let best = folders
        .into_iter()
        .filter(|f| is_descendant_of(&f.path, &normalized))
        .max_by_key(|f| f.path.len());
    Ok(Json(best.map(|f| CoveResolve {
        cove_id: f.cove_id,
        folder_id: f.id,
        folder_path: f.path,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_trailing_slash() {
        assert_eq!(normalize_path("/a/b/"), "/a/b");
        assert_eq!(normalize_path("/a/b"), "/a/b");
    }

    #[test]
    fn normalize_preserves_root() {
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn descendant_match_basics() {
        assert!(is_descendant_of("/a", "/a"));
        assert!(is_descendant_of("/a", "/a/b"));
        assert!(is_descendant_of("/a", "/a/b/c"));
        assert!(!is_descendant_of("/a", "/ab"));
        assert!(!is_descendant_of("/a", "/b"));
    }

    #[test]
    fn descendant_root_special_case() {
        assert!(is_descendant_of("/", "/"));
        assert!(is_descendant_of("/", "/a"));
        assert!(is_descendant_of("/", "/a/b/c"));
    }
}

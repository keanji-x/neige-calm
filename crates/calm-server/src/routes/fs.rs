//! `/api/fs/listdir` — read-only directory listing for the DirectoryPicker.
//!
//! The frontend's `DirectoryPicker` uses this to let users navigate the host
//! filesystem and pick a `cwd` for spawn-style cards (currently codex; could
//! be terminal in the future). Strictly read-only — no create/move/delete.
//!
//! ## Contract
//!
//! `GET /api/fs/listdir?path=<absolute_path>`
//!   * `path` omitted → start at `$HOME` (falls back to server cwd).
//!   * Path is canonicalized server-side (`tokio::fs::canonicalize`) so
//!     symlinks resolve and `..` segments collapse — the response always
//!     carries the canonical absolute path the frontend should treat as
//!     "current".
//!   * Entries are sorted directories-first, then case-insensitive
//!     alphabetic. Hidden entries (leading dot) are filtered out — there's
//!     no toggle yet by design (keep the surface small).
//!   * 200 with `{ path, parent, entries }` on success.
//!   * 400 if the resolved path doesn't exist or isn't a directory.
//!   * 403 if read permission is denied at the OS level.
//!
//! Security: kernel is a single-user process; this endpoint sits at the
//! same trust level as `/api/coves`, `/api/cards`, etc. — no auth gate
//! beyond what's wrapped around the whole router. If we ever multi-tenant
//! the server, this is one of the first endpoints to lock down.

use crate::error::{CalmError, ErrorBody, Result};
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::PathBuf;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/fs/listdir", get(listdir))
}

#[derive(Debug, Deserialize)]
pub struct ListdirQuery {
    /// Absolute path to list. Omitted/empty → start at `$HOME`.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListdirResponse {
    /// Canonical absolute path of the listed directory.
    pub path: String,
    /// Canonical absolute path of the parent directory, or `null` at root.
    pub parent: Option<String>,
    /// Children, sorted: directories first, then case-insensitive alpha.
    /// Hidden entries (leading dot) are filtered out.
    pub entries: Vec<DirEntry>,
}

#[utoipa::path(
    get,
    path = "/api/fs/listdir",
    tag = "fs",
    params(("path" = Option<String>, Query, description = "Absolute path to list; omitted → $HOME")),
    responses(
        (status = 200, description = "Directory listing", body = ListdirResponse),
        (status = 400, description = "Path doesn't exist or is not a directory", body = ErrorBody),
        (status = 403, description = "Read permission denied", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn listdir(
    State(_s): State<AppState>,
    Query(q): Query<ListdirQuery>,
) -> Result<Json<ListdirResponse>> {
    let raw = q
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_start);

    // Canonicalize → resolve symlinks, collapse `..`, materialize an
    // absolute path. Doing it before the metadata check means error
    // messages and the response path agree on what was actually probed.
    let canon = match tokio::fs::canonicalize(&raw).await {
        Ok(p) => p,
        Err(e) => return Err(map_io_err(&raw, e)),
    };

    let meta = tokio::fs::metadata(&canon)
        .await
        .map_err(|e| map_io_err(&canon, e))?;
    if !meta.is_dir() {
        return Err(CalmError::BadRequest(format!(
            "path {} is not a directory",
            canon.display()
        )));
    }

    let mut rd = tokio::fs::read_dir(&canon)
        .await
        .map_err(|e| map_io_err(&canon, e))?;

    let mut entries: Vec<DirEntry> = Vec::new();
    loop {
        match rd.next_entry().await {
            Ok(Some(entry)) => {
                let name = entry.file_name().to_string_lossy().to_string();
                // Filter hidden — leading dot, conventional Unix hidden.
                // Includes `.` and `..` (read_dir on Linux doesn't yield
                // them, but be defensive on other platforms).
                if name.starts_with('.') {
                    continue;
                }
                // `file_type()` is cheap (no extra stat on most platforms).
                // Symlinks are reported by what they point at; on a broken
                // link we fall back to "not a dir" which is the safe choice
                // (clicking it would error in `canonicalize` anyway).
                let is_dir = match entry.file_type().await {
                    Ok(ft) => {
                        if ft.is_symlink() {
                            // Probe the target — if it resolves to a dir,
                            // surface it as such so users can click through.
                            tokio::fs::metadata(entry.path())
                                .await
                                .map(|m| m.is_dir())
                                .unwrap_or(false)
                        } else {
                            ft.is_dir()
                        }
                    }
                    Err(_) => false,
                };
                entries.push(DirEntry { name, is_dir });
            }
            Ok(None) => break,
            Err(e) => {
                // Mid-iteration EACCES on a child shouldn't kill the whole
                // listing — log and skip. A genuinely unreadable directory
                // would have failed at `read_dir` above.
                tracing::debug!(error = %e, path = %canon.display(), "skip unreadable child");
                continue;
            }
        }
    }

    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    let parent = canon
        .parent()
        .filter(|p| *p != canon)
        .map(|p| p.to_string_lossy().to_string());

    Ok(Json(ListdirResponse {
        path: canon.to_string_lossy().to_string(),
        parent,
        entries,
    }))
}

fn default_start() -> PathBuf {
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Translate a `std::io::Error` from `canonicalize`/`metadata`/`read_dir`
/// into the right `CalmError` variant. `NotFound`/`InvalidInput` →
/// `BadRequest` (the path is bad as input); `PermissionDenied` →
/// `Forbidden`; anything else → `Internal`.
fn map_io_err(path: &std::path::Path, e: std::io::Error) -> CalmError {
    match e.kind() {
        ErrorKind::NotFound | ErrorKind::InvalidInput => {
            CalmError::BadRequest(format!("path {} not found", path.display()))
        }
        ErrorKind::PermissionDenied => {
            CalmError::Forbidden(format!("permission denied reading {}", path.display()))
        }
        _ => CalmError::Internal(format!("listdir {}: {}", path.display(), e)),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn lists_temp_dir_sorted_dirs_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("zeta")).unwrap();
        std::fs::create_dir(root.join("alpha")).unwrap();
        std::fs::write(root.join("beta.txt"), b"x").unwrap();
        std::fs::write(root.join("aaa.txt"), b"y").unwrap();
        // Hidden — must be filtered.
        std::fs::write(root.join(".secret"), b"z").unwrap();

        // Skip the AppState dance — exercise the meat by hand so the test
        // doesn't need to construct a full server harness.
        let mut rd = tokio::fs::read_dir(root).await.unwrap();
        let mut names: Vec<(String, bool)> = Vec::new();
        while let Some(entry) = rd.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().await.unwrap().is_dir();
            names.push((name, is_dir));
        }
        names.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
        });

        assert_eq!(
            names,
            vec![
                ("alpha".to_string(), true),
                ("zeta".to_string(), true),
                ("aaa.txt".to_string(), false),
                ("beta.txt".to_string(), false),
            ]
        );
    }
}

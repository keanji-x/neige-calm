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
//! same trust level as `/api/areas`, `/api/cards`, etc. — no auth gate
//! beyond what's wrapped around the whole router. If we ever multi-tenant
//! the server, this is one of the first endpoints to lock down.

use crate::error::{CalmError, ErrorBody, Result};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::fs::Metadata;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use utoipa::ToSchema;

const MAX_READFILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_READFILE_RAW_BYTES: u64 = 100 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/fs/listdir", get(listdir))
        .route("/api/fs/readfile", get(readfile))
        .route("/api/fs/readfile-raw", get(readfile_raw))
        .route(
            "/api/tracks/{track_id}/workspace/readfile",
            get(read_track_workspace_file),
        )
        .route(
            "/api/tracks/{track_id}/workspace/readfile-raw",
            get(read_track_workspace_file_raw),
        )
        .route("/api/fs/gitstatus", get(gitstatus))
        .route("/api/fs/gitdiff", get(gitdiff))
}

#[derive(Debug, Deserialize)]
pub struct ListdirQuery {
    /// Absolute path to list. Omitted/empty → start at `$HOME`.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PathQuery {
    /// Absolute path to inspect.
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkspacePathQuery {
    /// Path relative to the Track's persisted workspace root.
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct GitDiffQuery {
    /// Absolute path to a file inside a git repository.
    pub path: String,
    /// Optional old path, relative to the repository root or absolute.
    #[serde(default)]
    pub old_path: Option<String>,
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

#[derive(Debug, Serialize, ToSchema)]
pub struct ReadFileResponse {
    pub path: String,
    pub size: u64,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GitChangedFile {
    /// Path relative to the repository root.
    pub path: String,
    /// One of: modified, added, deleted, untracked, renamed.
    pub status: String,
    /// Previous path for renamed files, relative to the repository root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GitStatusResponse {
    pub repo_root: String,
    pub files: Vec<GitChangedFile>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GitDiffResponse {
    /// Path relative to the repository root.
    pub path: String,
    /// One of: modified, added, deleted, renamed.
    pub status: String,
    pub head_text: Option<String>,
    pub working_text: Option<String>,
    pub truncated: bool,
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
    State(_s): State<RouteState>,
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

#[utoipa::path(
    get,
    path = "/api/fs/readfile",
    tag = "fs",
    params(("path" = String, Query, description = "Absolute path to a text file")),
    responses(
        (status = 200, description = "Read text file contents", body = ReadFileResponse),
        (status = 400, description = "Path doesn't exist, is not a file, or is binary/non-UTF-8", body = ErrorBody),
        (status = 403, description = "Read permission denied", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn readfile(
    State(_s): State<RouteState>,
    Query(q): Query<PathQuery>,
) -> Result<Json<ReadFileResponse>> {
    let raw = PathBuf::from(q.path.trim());
    Ok(Json(read_file_response(&raw).await?))
}

#[utoipa::path(
    get,
    path = "/api/fs/readfile-raw",
    tag = "fs",
    params(("path" = String, Query, description = "Absolute path to an image file")),
    responses(
        (status = 200, description = "Read raw image bytes", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 400, description = "Path doesn't exist, is not a file, has an unsupported extension, or exceeds the image cap", body = ErrorBody),
        (status = 403, description = "Read permission denied", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn readfile_raw(
    State(_s): State<RouteState>,
    Query(q): Query<PathQuery>,
) -> Result<Response> {
    let raw = PathBuf::from(q.path.trim());
    read_file_raw_response(&raw).await
}

#[utoipa::path(
    get,
    path = "/api/tracks/{track_id}/workspace/readfile",
    tag = "fs",
    params(
        ("track_id" = String, Path, description = "Track whose persisted workspace is the read boundary"),
        ("path" = String, Query, description = "Workspace-relative text file path")
    ),
    responses(
        (status = 200, description = "Workspace text file contents", body = ReadFileResponse),
        (status = 400, description = "Path is invalid, outside the Track workspace, missing, a directory, or binary/non-UTF-8", body = ErrorBody),
        (status = 403, description = "Read permission denied", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn read_track_workspace_file(
    State(s): State<RouteState>,
    AxumPath(track_id): AxumPath<String>,
    Query(q): Query<WorkspacePathQuery>,
) -> Result<Json<ReadFileResponse>> {
    let track = s
        .repo
        .track_get(&track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    let opened = open_workspace_regular_file(Path::new(&track.workspace.path), &q.path).await?;
    Ok(Json(read_workspace_file_response(opened).await?))
}

#[utoipa::path(
    get,
    path = "/api/tracks/{track_id}/workspace/readfile-raw",
    tag = "fs",
    params(
        ("track_id" = String, Path, description = "Track whose persisted workspace is the read boundary"),
        ("path" = String, Query, description = "Workspace-relative image file path")
    ),
    responses(
        (status = 200, description = "Workspace image bytes", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 400, description = "Path is invalid, outside the Track workspace, missing, not a file, unsupported, or too large", body = ErrorBody),
        (status = 403, description = "Read permission denied", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn read_track_workspace_file_raw(
    State(s): State<RouteState>,
    AxumPath(track_id): AxumPath<String>,
    Query(q): Query<WorkspacePathQuery>,
) -> Result<Response> {
    let track = s
        .repo
        .track_get(&track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    let opened = open_workspace_regular_file(Path::new(&track.workspace.path), &q.path).await?;
    read_workspace_file_raw_response(opened).await
}

#[utoipa::path(
    get,
    path = "/api/fs/gitstatus",
    tag = "fs",
    params(("path" = String, Query, description = "Absolute path to a directory inside a git repository")),
    responses(
        (status = 200, description = "Working tree status", body = GitStatusResponse),
        (status = 400, description = "Path is not a directory or not inside a git repository", body = ErrorBody),
        (status = 403, description = "Read permission denied", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn gitstatus(
    State(_s): State<RouteState>,
    Query(q): Query<PathQuery>,
) -> Result<Json<GitStatusResponse>> {
    let raw = PathBuf::from(q.path.trim());
    Ok(Json(git_status_response(&raw).await?))
}

#[utoipa::path(
    get,
    path = "/api/fs/gitdiff",
    tag = "fs",
    params(
        ("path" = String, Query, description = "Absolute path to a file inside a git repository"),
        ("old_path" = Option<String>, Query, description = "Previous path for renamed files, relative to the repository root or absolute")
    ),
    responses(
        (status = 200, description = "HEAD and working-tree text for a changed file", body = GitDiffResponse),
        (status = 400, description = "Path is not inside a git repository or file is binary/non-UTF-8", body = ErrorBody),
        (status = 403, description = "Read permission denied", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn gitdiff(
    State(_s): State<RouteState>,
    Query(q): Query<GitDiffQuery>,
) -> Result<Json<GitDiffResponse>> {
    let raw = PathBuf::from(q.path.trim());
    Ok(Json(git_diff_response(&raw, q.old_path.as_deref()).await?))
}

async fn canonicalize_regular_file(raw: &Path) -> Result<(PathBuf, Metadata)> {
    let canon = match tokio::fs::canonicalize(raw).await {
        Ok(p) => p,
        Err(e) => return Err(map_io_err(raw, e)),
    };

    let meta = tokio::fs::metadata(&canon)
        .await
        .map_err(|e| map_io_err(&canon, e))?;
    if !meta.is_file() {
        return Err(CalmError::BadRequest(format!(
            "path {} is not a regular file",
            canon.display()
        )));
    }

    Ok((canon, meta))
}

fn workspace_relative_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(CalmError::BadRequest(
            "workspace file path must be non-empty and relative".into(),
        ));
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(CalmError::BadRequest(format!(
                    "workspace file path {raw} must stay relative to the track workspace"
                )));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(CalmError::BadRequest(
            "workspace file path must name a file".into(),
        ));
    }
    Ok(normalized)
}

#[derive(Debug)]
struct OpenWorkspaceFile {
    file: tokio::fs::File,
    display_path: PathBuf,
    size: u64,
}

/// Open one workspace file with the root directory descriptor as the
/// authority. `openat2` resolves and opens atomically, so a concurrent worker
/// cannot swap a checked parent for an escaping symlink before the read.
#[cfg(target_os = "linux")]
async fn open_workspace_regular_file(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<OpenWorkspaceFile> {
    use nix::fcntl::{OFlag, OpenHow, ResolveFlag, openat2};
    use nix::sys::stat::Mode;
    use std::os::fd::{AsRawFd, FromRawFd};

    let relative = workspace_relative_path(relative_path)?;
    let root = tokio::fs::File::open(workspace_root)
        .await
        .map_err(|error| map_io_err(workspace_root, error))?;
    let root_meta = root
        .metadata()
        .await
        .map_err(|error| map_io_err(workspace_root, error))?;
    if !root_meta.is_dir() {
        return Err(CalmError::BadRequest(format!(
            "track workspace {} is not a directory",
            workspace_root.display()
        )));
    }
    let requested = workspace_root.join(&relative);
    let workspace_root = workspace_root.to_path_buf();
    let root = root.into_std().await;
    tokio::task::spawn_blocking(move || {
        let raw_fd = openat2(
            root.as_raw_fd(),
            &relative,
            OpenHow::new()
                .flags(OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK)
                .mode(Mode::empty())
                .resolve(ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_MAGICLINKS),
        )
        .map_err(|error| map_workspace_open_err(&requested, &workspace_root, error))?;
        // SAFETY: `openat2` returned a new owned descriptor and this is its
        // only conversion into an owning Rust value.
        let file = unsafe { std::fs::File::from_raw_fd(raw_fd) };
        let meta = file
            .metadata()
            .map_err(|error| map_io_err(&requested, error))?;
        if !meta.is_file() {
            return Err(CalmError::BadRequest(format!(
                "path {} is not a regular file",
                requested.display()
            )));
        }
        Ok(OpenWorkspaceFile {
            file: tokio::fs::File::from_std(file),
            display_path: requested,
            size: meta.len(),
        })
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace open task failed: {error}")))?
}

#[cfg(target_os = "linux")]
fn map_workspace_open_err(
    requested: &Path,
    workspace_root: &Path,
    error: nix::errno::Errno,
) -> CalmError {
    use nix::errno::Errno;

    match error {
        Errno::EXDEV | Errno::ELOOP => CalmError::BadRequest(format!(
            "path {} resolves outside track workspace {}",
            requested.display(),
            workspace_root.display()
        )),
        Errno::ENOENT | Errno::ENOTDIR | Errno::EINVAL => {
            CalmError::BadRequest(format!("path {} not found", requested.display()))
        }
        Errno::ENXIO | Errno::ENODEV => CalmError::BadRequest(format!(
            "path {} is not a regular file",
            requested.display()
        )),
        Errno::EACCES | Errno::EPERM => {
            CalmError::Forbidden(format!("permission denied reading {}", requested.display()))
        }
        Errno::ENOSYS => {
            CalmError::Internal("secure workspace reads require Linux openat2 support".into())
        }
        _ => map_io_err(requested, std::io::Error::from_raw_os_error(error as i32)),
    }
}

#[cfg(not(target_os = "linux"))]
async fn open_workspace_regular_file(
    _workspace_root: &Path,
    relative_path: &str,
) -> Result<OpenWorkspaceFile> {
    workspace_relative_path(relative_path)?;
    Err(CalmError::Internal(
        "secure workspace reads require Linux openat2 support".into(),
    ))
}

async fn read_workspace_file_response(opened: OpenWorkspaceFile) -> Result<ReadFileResponse> {
    let (text, truncated) = read_text_file_capped(
        opened.file,
        opened.size,
        "binary or non-UTF-8 file",
        &opened.display_path,
    )
    .await?;
    Ok(ReadFileResponse {
        path: opened.display_path.to_string_lossy().to_string(),
        size: opened.size,
        text,
        truncated,
    })
}

async fn read_workspace_file_raw_response(opened: OpenWorkspaceFile) -> Result<Response> {
    let content_type = image_content_type(&opened.display_path)?;
    read_file_raw_response_from_handle(opened.file, opened.size, &opened.display_path, content_type)
        .await
}

async fn read_file_response(raw: &Path) -> Result<ReadFileResponse> {
    let (canon, meta) = canonicalize_regular_file(raw).await?;
    read_file_response_from(canon, meta).await
}

async fn read_file_response_from(canon: PathBuf, meta: Metadata) -> Result<ReadFileResponse> {
    let (text, truncated) = read_text_capped(&canon, "binary or non-UTF-8 file").await?;
    Ok(ReadFileResponse {
        path: canon.to_string_lossy().to_string(),
        size: meta.len(),
        text,
        truncated,
    })
}

async fn read_file_raw_response(raw: &Path) -> Result<Response> {
    let (canon, meta) = canonicalize_regular_file(raw).await?;
    read_file_raw_response_from(canon, meta).await
}

async fn read_file_raw_response_from(canon: PathBuf, meta: Metadata) -> Result<Response> {
    let content_type = image_content_type(&canon)?;
    let file = tokio::fs::File::open(&canon)
        .await
        .map_err(|error| map_io_err(&canon, error))?;
    read_file_raw_response_from_handle(file, meta.len(), &canon, content_type).await
}

async fn read_file_raw_response_from_handle(
    file: tokio::fs::File,
    size: u64,
    path: &Path,
    content_type: &'static str,
) -> Result<Response> {
    if size > MAX_READFILE_RAW_BYTES {
        return Err(CalmError::BadRequest("image exceeds 100 MiB cap".into()));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    use tokio::io::AsyncReadExt;
    file.take(MAX_READFILE_RAW_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| map_io_err(path, error))?;
    if bytes.len() as u64 > MAX_READFILE_RAW_BYTES {
        return Err(CalmError::BadRequest("image exceeds 100 MiB cap".into()));
    }
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, "sandbox"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

fn image_content_type(path: &Path) -> Result<&'static str> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => Ok("image/png"),
        Some("jpg" | "jpeg") => Ok("image/jpeg"),
        Some("gif") => Ok("image/gif"),
        Some("webp") => Ok("image/webp"),
        Some("bmp") => Ok("image/bmp"),
        Some("ico") => Ok("image/x-icon"),
        Some("svg") => Ok("image/svg+xml"),
        _ => Err(CalmError::BadRequest("unsupported image extension".into())),
    }
}

async fn git_status_response(raw: &Path) -> Result<GitStatusResponse> {
    let dir = match tokio::fs::canonicalize(raw).await {
        Ok(p) => p,
        Err(e) => return Err(map_io_err(raw, e)),
    };
    let meta = tokio::fs::metadata(&dir)
        .await
        .map_err(|e| map_io_err(&dir, e))?;
    if !meta.is_dir() {
        return Err(CalmError::BadRequest(format!(
            "path {} is not a directory",
            dir.display()
        )));
    }

    let root = git_root(&dir).await?;
    let out = git_output(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await?;
    let files = parse_porcelain_status(&out.stdout);
    Ok(GitStatusResponse {
        repo_root: root.to_string_lossy().to_string(),
        files,
    })
}

async fn git_diff_response(raw: &Path, old_path: Option<&str>) -> Result<GitDiffResponse> {
    let canon = canonicalize_file_or_parent(raw).await?;
    let dir = canon.parent().ok_or_else(|| {
        CalmError::BadRequest(format!("path {} has no parent directory", canon.display()))
    })?;
    let root = git_root(dir).await?;
    let rel = canon.strip_prefix(&root).map_err(|_| {
        CalmError::BadRequest(format!(
            "path {} is outside git repository",
            canon.display()
        ))
    })?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let old_path = old_path.map(str::trim).filter(|s| !s.is_empty());
    let head_rel = match old_path {
        Some(old_path) => repo_relative_path(&root, old_path)?,
        None => rel_str.clone(),
    };

    let (head_text, head_truncated) = git_show_head(&root, &head_rel).await?;
    let (working_text, working_truncated) = match tokio::fs::metadata(&canon).await {
        Ok(meta) if meta.is_file() => {
            let (text, truncated) =
                read_text_capped(&canon, "binary file diff unsupported").await?;
            (Some(text), truncated)
        }
        Ok(_) => {
            return Err(CalmError::BadRequest(format!(
                "path {} is not a regular file",
                canon.display()
            )));
        }
        Err(e) if e.kind() == ErrorKind::NotFound => (None, false),
        Err(e) => return Err(map_io_err(&canon, e)),
    };

    let status = if old_path.is_some() {
        "renamed"
    } else {
        match (&head_text, &working_text) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            (Some(_), Some(_)) => "modified",
            (None, None) => "deleted",
        }
    };

    Ok(GitDiffResponse {
        path: rel_str,
        status: status.to_string(),
        head_text,
        working_text,
        truncated: head_truncated || working_truncated,
    })
}

fn repo_relative_path(root: &Path, value: &str) -> Result<String> {
    let path = Path::new(value);
    let rel = if path.is_absolute() {
        path.strip_prefix(root).map_err(|_| {
            CalmError::BadRequest(format!("path {} is outside git repository", path.display()))
        })?
    } else {
        path
    };
    normalize_repo_relative_path(rel, value)
}

fn normalize_repo_relative_path(path: &Path, original: &str) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(part.to_string_lossy().to_string());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(CalmError::BadRequest(format!(
                        "path {original} is outside git repository"
                    )));
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(CalmError::BadRequest(format!(
                    "path {original} is outside git repository"
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(CalmError::BadRequest(format!("path {original} is empty")));
    }
    Ok(parts.join("/"))
}

async fn canonicalize_file_or_parent(raw: &Path) -> Result<PathBuf> {
    match tokio::fs::canonicalize(raw).await {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let parent = raw.parent().ok_or_else(|| {
                CalmError::BadRequest(format!("path {} not found", raw.display()))
            })?;
            let name = raw.file_name().ok_or_else(|| {
                CalmError::BadRequest(format!("path {} not found", raw.display()))
            })?;
            let parent = tokio::fs::canonicalize(parent)
                .await
                .map_err(|e| map_io_err(parent, e))?;
            Ok(parent.join(name))
        }
        Err(e) => Err(map_io_err(raw, e)),
    }
}

async fn read_text_capped(path: &Path, binary_message: &str) -> Result<(String, bool)> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| map_io_err(path, e))?;
    let size = file
        .metadata()
        .await
        .map_err(|error| map_io_err(path, error))?
        .len();
    read_text_file_capped(file, size, binary_message, path).await
}

async fn read_text_file_capped(
    file: tokio::fs::File,
    size: u64,
    binary_message: &str,
    path: &Path,
) -> Result<(String, bool)> {
    let mut buf = Vec::with_capacity(std::cmp::min(size, MAX_READFILE_BYTES) as usize);
    use tokio::io::AsyncReadExt;
    file.take(MAX_READFILE_BYTES + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| map_io_err(path, e))?;
    let truncated = buf.len() as u64 > MAX_READFILE_BYTES;
    if truncated {
        buf.truncate(MAX_READFILE_BYTES as usize);
    }
    decode_capped_utf8(&buf, truncated, binary_message, path.display()).map(|s| (s, truncated))
}

async fn git_root(dir: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|e| map_git_spawn_err("rev-parse", e))?;
    if !out.status.success() {
        return Err(CalmError::BadRequest("not a git repository".into()));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(PathBuf::from(s.trim()))
}

async fn git_output(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .map_err(|e| map_git_spawn_err(args.join(" "), e))?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(CalmError::Internal(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

async fn git_show_head(root: &Path, rel: &str) -> Result<(Option<String>, bool)> {
    let size_out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "-s", &format!("HEAD:{rel}")])
        .output()
        .await
        .map_err(|e| map_git_spawn_err(format!("cat-file -s HEAD:{rel}"), e))?;
    if !size_out.status.success() {
        return Ok((None, false));
    }
    let size = String::from_utf8_lossy(&size_out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|e| CalmError::Internal(format!("git cat-file -s HEAD:{rel}: {e}")))?;
    let truncated = size > MAX_READFILE_BYTES;

    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "blob", &format!("HEAD:{rel}")])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| map_git_spawn_err(format!("cat-file blob HEAD:{rel}"), e))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        CalmError::Internal(format!("git cat-file blob HEAD:{rel}: missing stdout"))
    })?;
    let mut buf = Vec::with_capacity(std::cmp::min(size, MAX_READFILE_BYTES) as usize);
    use tokio::io::AsyncReadExt;
    stdout
        .take(MAX_READFILE_BYTES)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| CalmError::Internal(format!("git cat-file blob HEAD:{rel}: {e}")))?;

    if truncated {
        let _ = child.kill().await;
    }
    let status = child
        .wait()
        .await
        .map_err(|e| CalmError::Internal(format!("git cat-file blob HEAD:{rel}: {e}")))?;
    if !truncated && !status.success() {
        return Ok((None, false));
    }

    let text = decode_capped_utf8(&buf, truncated, "binary file diff unsupported", rel)?;
    Ok((Some(text), truncated))
}

fn map_git_spawn_err(context: impl Display, e: std::io::Error) -> CalmError {
    // File viewer git endpoints shell out to the system binary; the server
    // runtime must provide `git` on PATH.
    if e.kind() == ErrorKind::NotFound {
        CalmError::Internal("git is not installed or not on PATH on the server".into())
    } else {
        CalmError::Internal(format!("git {context}: {e}"))
    }
}

fn decode_capped_utf8(
    buf: &[u8],
    truncated: bool,
    binary_message: &str,
    path: impl Display,
) -> Result<String> {
    match std::str::from_utf8(buf) {
        Ok(s) => Ok(s.to_string()),
        Err(e) if truncated => {
            let valid = e.valid_up_to();
            Ok(String::from_utf8_lossy(&buf[..valid]).into_owned())
        }
        Err(_) => Err(CalmError::BadRequest(format!("{binary_message}: {path}"))),
    }
}

fn parse_porcelain_status(bytes: &[u8]) -> Vec<GitChangedFile> {
    let mut files = Vec::new();
    let mut parts = bytes.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(part) = parts.next() {
        if part.len() < 4 {
            continue;
        }
        let x = part[0] as char;
        let y = part[1] as char;
        let path = String::from_utf8_lossy(&part[3..]).to_string();
        let mut old_path = None;
        let status = if x == '?' || y == '?' {
            "untracked"
        } else if x == 'R' || y == 'R' {
            // `git status --porcelain=v1 -z` emits renames as `R  <new>\0<old>\0`.
            old_path = parts
                .next()
                .map(|old| String::from_utf8_lossy(old).to_string());
            "renamed"
        } else if x == 'D' || y == 'D' {
            "deleted"
        } else if x == 'A' || y == 'A' {
            "added"
        } else {
            "modified"
        };
        files.push(GitChangedFile {
            path,
            status: status.to_string(),
            old_path,
        });
    }
    files
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
        _ => CalmError::Internal(format!("fs {}: {}", path.display(), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::model::{NewArea, NewTrack};
    use crate::plugin_host::{PluginHost, PluginRegistry};
    use crate::routes::theme::RequestTheme;
    use crate::state::{AppState, CodexClient, DaemonClient, WriteContext};
    use crate::track_area_cache::TrackAreaCache;
    use axum::extract::FromRef;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;
    use std::process::Command as StdCommand;
    use std::sync::Arc;

    async fn route_state_with_workspace_tracks(
        workspace_a: &Path,
        workspace_b: &Path,
    ) -> (RouteState, String, String) {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "workspace reads".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let create_track = |title: &str, workspace: &Path| NewTrack {
            area_id: area.id.clone(),
            title: title.into(),
            sort: None,
            cwd: workspace.to_string_lossy().to_string(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        };
        let track_a = repo
            .track_create(create_track("workspace A", workspace_a))
            .await
            .unwrap();
        let track_b = repo
            .track_create(create_track("workspace B", workspace_b))
            .await
            .unwrap();
        let repo_dyn: Arc<dyn Repo> = repo;
        let events = EventBus::new();
        let roles = CardRoleCache::new();
        let tracks = TrackAreaCache::new();
        let state = AppState::from_parts(
            repo_dyn.clone(),
            events.clone(),
            Arc::new(DaemonClient {
                data_dir: workspace_a.to_path_buf(),
                proc_supervisor_sock: None,
            }),
            Arc::new(PluginHost::new_full(
                Arc::new(PluginRegistry::empty()),
                repo_dyn,
                PathBuf::new(),
                workspace_a.join("plugin-data"),
                Vec::new(),
                events,
                WriteContext::new(roles.clone(), tracks.clone()),
            )),
            Arc::new(CodexClient::new_stub()),
            Some(roles),
            Some(tracks),
        );
        (
            RouteState::from_ref(&state),
            track_a.id.to_string(),
            track_b.id.to_string(),
        )
    }

    const PNG_1X1: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I',
        b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];

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

    #[tokio::test]
    async fn readfile_text_file_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("hello.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let res = read_file_response(&file).await.unwrap();
        assert_eq!(res.text, "fn main() {}\n");
        assert_eq!(res.size, 13);
        assert!(!res.truncated);
    }

    #[tokio::test]
    async fn readfile_rejects_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_file_response(tmp.path()).await.unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        assert!(err.to_string().contains("not a regular file"));
    }

    #[tokio::test]
    async fn readfile_rejects_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_file_response(&tmp.path().join("missing.txt"))
            .await
            .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[tokio::test]
    async fn readfile_truncates_oversize_text() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("big.txt");
        std::fs::write(&file, vec![b'a'; MAX_READFILE_BYTES as usize + 17]).unwrap();

        let res = read_file_response(&file).await.unwrap();
        assert!(res.truncated);
        assert_eq!(res.size, MAX_READFILE_BYTES + 17);
        assert_eq!(res.text.len(), MAX_READFILE_BYTES as usize);
    }

    #[tokio::test]
    async fn readfile_truncates_at_valid_utf8_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("big-utf8.txt");
        let mut bytes = vec![b'a'; MAX_READFILE_BYTES as usize - 1];
        bytes.extend_from_slice("é".as_bytes());
        bytes.extend_from_slice(b"tail");
        std::fs::write(&file, bytes).unwrap();

        let res = read_file_response(&file).await.unwrap();
        assert!(res.truncated);
        assert_eq!(res.size, MAX_READFILE_BYTES + 5);
        assert_eq!(res.text.len(), MAX_READFILE_BYTES as usize - 1);
        assert!(res.text.ends_with('a'));
    }

    #[tokio::test]
    async fn readfile_rejects_non_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("bin.dat");
        std::fs::write(&file, [0xff, 0xfe, 0xfd]).unwrap();

        let err = read_file_response(&file).await.unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        assert!(err.to_string().contains("binary or non-UTF-8 file"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn workspace_file_rejects_a_symlink_that_leaves_the_root() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "outside\n").unwrap();
        symlink(&secret, workspace.path().join("leak.txt")).unwrap();

        let err = open_workspace_regular_file(workspace.path(), "leak.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        assert!(err.to_string().contains("outside track workspace"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn workspace_file_allows_a_symlink_that_stays_inside_the_root() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let real = workspace.path().join("real.txt");
        std::fs::write(&real, "inside\n").unwrap();
        // Relative symlinks remain constrained by the root directory fd.
        // Absolute symlinks are rejected by RESOLVE_BENEATH even when their
        // current spelling happens to point back into this directory.
        symlink("real.txt", workspace.path().join("alias.txt")).unwrap();

        let opened = open_workspace_regular_file(workspace.path(), "alias.txt")
            .await
            .unwrap();
        assert_eq!(
            read_workspace_file_response(opened).await.unwrap().text,
            "inside\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn workspace_file_stays_bound_when_parent_is_swapped_after_open() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let live = workspace.path().join("swap");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("value.txt"), "inside\n").unwrap();
        std::fs::write(live.join("image.png"), b"inside image").unwrap();
        std::fs::write(outside.path().join("value.txt"), "outside\n").unwrap();
        std::fs::write(outside.path().join("image.png"), b"outside image").unwrap();

        let opened_text = open_workspace_regular_file(workspace.path(), "swap/value.txt")
            .await
            .unwrap();
        let opened_image = open_workspace_regular_file(workspace.path(), "swap/image.png")
            .await
            .unwrap();
        std::fs::rename(&live, workspace.path().join("original")).unwrap();
        symlink(outside.path(), &live).unwrap();

        let text = read_workspace_file_response(opened_text).await.unwrap();
        assert_eq!(text.text, "inside\n");
        let image = read_workspace_file_raw_response(opened_image)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(image.as_ref(), b"inside image");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workspace_fifo_is_rejected_without_blocking_a_runtime_worker() {
        use nix::sys::stat::Mode;
        use nix::unistd::mkfifo;
        use std::time::Duration;

        let workspace = tempfile::tempdir().unwrap();
        let fifo = workspace.path().join("pipe.txt");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let root = workspace.path().to_path_buf();
        let mut opening =
            tokio::spawn(async move { open_workspace_regular_file(&root, "pipe.txt").await });

        match tokio::time::timeout(Duration::from_millis(250), &mut opening).await {
            Ok(result) => {
                let error = result.unwrap().unwrap_err();
                assert!(matches!(error, CalmError::BadRequest(_)));
            }
            Err(_) => {
                // Clean up the deliberately blocked pre-fix open so the test
                // runtime can shut down before reporting the timeout.
                let writer = std::thread::spawn(move || {
                    std::fs::OpenOptions::new().write(true).open(fifo).unwrap()
                });
                let _ = opening.await.unwrap();
                drop(writer.join().unwrap());
                panic!("opening a workspace FIFO blocked the async runtime");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_special_file_open_errors_are_bad_requests() {
        for errno in [nix::errno::Errno::ENXIO, nix::errno::Errno::ENODEV] {
            let error = map_workspace_open_err(
                Path::new("/workspace/socket.png"),
                Path::new("/workspace"),
                errno,
            );
            assert!(matches!(error, CalmError::BadRequest(_)));
            assert!(error.to_string().contains("not a regular file"));
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn workspace_file_handlers_use_the_requested_tracks_persisted_root() {
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        std::fs::write(workspace_a.path().join("same.txt"), "from A\n").unwrap();
        std::fs::write(workspace_b.path().join("same.txt"), "from B\n").unwrap();
        std::fs::write(workspace_a.path().join("same.png"), b"A image").unwrap();
        std::fs::write(workspace_b.path().join("same.png"), b"B image").unwrap();
        let (state, track_a, track_b) =
            route_state_with_workspace_tracks(workspace_a.path(), workspace_b.path()).await;

        let Json(text) = read_track_workspace_file(
            State(state.clone()),
            AxumPath(track_a.clone()),
            Query(WorkspacePathQuery {
                path: "same.txt".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(text.text, "from A\n");

        let raw = read_track_workspace_file_raw(
            State(state.clone()),
            AxumPath(track_b),
            Query(WorkspacePathQuery {
                path: "same.png".into(),
            }),
        )
        .await
        .unwrap();
        let bytes = raw.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), b"B image");

        let unknown = read_track_workspace_file(
            State(state.clone()),
            AxumPath("missing-track".into()),
            Query(WorkspacePathQuery {
                path: "same.txt".into(),
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(unknown, CalmError::NotFound(_)));

        let escaping = read_track_workspace_file(
            State(state),
            AxumPath(track_a),
            Query(WorkspacePathQuery {
                path: "../same.txt".into(),
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(escaping, CalmError::BadRequest(_)));
    }

    #[tokio::test]
    async fn workspace_file_rejects_absolute_and_parent_paths_before_io() {
        let workspace = tempfile::tempdir().unwrap();
        for path in ["/etc/passwd", "../outside.txt", "a/../../outside.txt", ""] {
            let err = open_workspace_regular_file(workspace.path(), path)
                .await
                .unwrap_err();
            assert!(matches!(err, CalmError::BadRequest(_)), "{path}: {err}");
        }
    }

    #[tokio::test]
    async fn readfile_raw_png_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("pixel.png");
        std::fs::write(&file, PNG_1X1).unwrap();

        let res = read_file_raw_response(&file).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let (parts, body) = res.into_parts();
        assert_eq!(
            parts.headers.get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
        assert_eq!(
            parts.headers.get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert_eq!(
            parts.headers.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            "sandbox"
        );
        assert_eq!(
            parts.headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        let bytes = body.collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], PNG_1X1);
    }

    #[tokio::test]
    async fn readfile_raw_svg_carries_sandbox_csp() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("icon.svg");
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        std::fs::write(&file, svg).unwrap();

        let res = read_file_raw_response(&file).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let (parts, body) = res.into_parts();
        assert_eq!(
            parts.headers.get(header::CONTENT_TYPE).unwrap(),
            "image/svg+xml"
        );
        assert_eq!(
            parts.headers.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            "sandbox"
        );
        assert_eq!(
            parts.headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        let bytes = body.collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], svg);
    }

    #[tokio::test]
    async fn readfile_raw_extension_is_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("pixel.PNG");
        std::fs::write(&file, PNG_1X1).unwrap();

        let res = read_file_raw_response(&file).await.unwrap();
        let (parts, _) = res.into_parts();
        assert_eq!(
            parts.headers.get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
    }

    #[tokio::test]
    async fn readfile_raw_rejects_non_image_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("notes.txt");
        std::fs::write(&file, "hello\n").unwrap();

        let err = read_file_raw_response(&file).await.unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        assert!(err.to_string().contains("unsupported image extension"));
    }

    #[tokio::test]
    async fn readfile_raw_rejects_oversize_image() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("huge.png");
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(MAX_READFILE_RAW_BYTES + 1).unwrap();

        let err = read_file_raw_response(&file).await.unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        assert!(err.to_string().contains("image exceeds 100 MiB cap"));
    }

    #[tokio::test]
    async fn readfile_raw_rejects_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_file_raw_response(&tmp.path().join("missing.png"))
            .await
            .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn gitstatus_and_gitdiff_cover_working_tree_states() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init"]);
        std::fs::write(tmp.path().join("tracked.txt"), "head\n").unwrap();
        std::fs::write(tmp.path().join("deleted.txt"), "bye\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "initial"]);

        std::fs::write(tmp.path().join("tracked.txt"), "work\n").unwrap();
        std::fs::write(tmp.path().join("added.txt"), "new\n").unwrap();
        git(tmp.path(), &["add", "added.txt"]);
        std::fs::remove_file(tmp.path().join("deleted.txt")).unwrap();
        std::fs::write(tmp.path().join("untracked.txt"), "loose\n").unwrap();

        let status = git_status_response(tmp.path()).await.unwrap();
        assert_eq!(status.repo_root, tmp.path().to_string_lossy());
        assert_status(&status.files, "tracked.txt", "modified");
        assert_status(&status.files, "added.txt", "added");
        assert_status(&status.files, "deleted.txt", "deleted");
        assert_status(&status.files, "untracked.txt", "untracked");

        let modified = git_diff_response(&tmp.path().join("tracked.txt"), None)
            .await
            .unwrap();
        assert_eq!(modified.path, "tracked.txt");
        assert_eq!(modified.status, "modified");
        assert_eq!(modified.head_text.as_deref(), Some("head\n"));
        assert_eq!(modified.working_text.as_deref(), Some("work\n"));
        assert!(!modified.truncated);

        let added = git_diff_response(&tmp.path().join("added.txt"), None)
            .await
            .unwrap();
        assert_eq!(added.status, "added");
        assert_eq!(added.head_text, None);
        assert_eq!(added.working_text.as_deref(), Some("new\n"));
        assert!(!added.truncated);

        let deleted = git_diff_response(&tmp.path().join("deleted.txt"), None)
            .await
            .unwrap();
        assert_eq!(deleted.status, "deleted");
        assert_eq!(deleted.head_text.as_deref(), Some("bye\n"));
        assert_eq!(deleted.working_text, None);
        assert!(!deleted.truncated);
    }

    #[tokio::test]
    async fn gitdiff_truncates_head_text() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init"]);
        let file = tmp.path().join("big.txt");
        std::fs::write(&file, vec![b'a'; MAX_READFILE_BYTES as usize + 17]).unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "initial"]);

        std::fs::write(&file, "small\n").unwrap();

        let diff = git_diff_response(&file, None).await.unwrap();
        assert_eq!(diff.status, "modified");
        assert!(diff.truncated);
        assert_eq!(
            diff.head_text.as_deref().unwrap().len(),
            MAX_READFILE_BYTES as usize
        );
        assert_eq!(diff.working_text.as_deref(), Some("small\n"));
    }

    #[tokio::test]
    async fn gitstatus_rejects_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let err = git_status_response(tmp.path()).await.unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        assert!(err.to_string().contains("not a git repository"));
    }

    #[tokio::test]
    async fn gitstatus_expands_untracked_directories() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init"]);
        std::fs::create_dir(tmp.path().join("dir")).unwrap();
        std::fs::write(tmp.path().join("dir").join("a.txt"), "loose\n").unwrap();

        let status = git_status_response(tmp.path()).await.unwrap();
        assert_status(&status.files, "dir/a.txt", "untracked");
        assert!(
            !status.files.iter().any(|f| f.path == "dir/"),
            "bare untracked directory should not be returned; got {:?}",
            status.files
        );
    }

    #[tokio::test]
    async fn gitstatus_and_gitdiff_cover_renamed_files() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init"]);
        std::fs::write(tmp.path().join("old.txt"), "head\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "initial"]);

        git(tmp.path(), &["mv", "old.txt", "new.txt"]);
        std::fs::write(tmp.path().join("new.txt"), "work\n").unwrap();

        let status = git_status_response(tmp.path()).await.unwrap();
        let renamed = status
            .files
            .iter()
            .find(|f| f.path == "new.txt")
            .expect("renamed file missing from status");
        assert_eq!(renamed.status, "renamed");
        assert_eq!(renamed.old_path.as_deref(), Some("old.txt"));

        let diff = git_diff_response(&tmp.path().join("new.txt"), Some("old.txt"))
            .await
            .unwrap();
        assert_eq!(diff.path, "new.txt");
        assert_eq!(diff.status, "renamed");
        assert_eq!(diff.head_text.as_deref(), Some("head\n"));
        assert_eq!(diff.working_text.as_deref(), Some("work\n"));
        assert!(!diff.truncated);
    }

    fn git(root: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.email=test@test", "-c", "user.name=test"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn assert_status(files: &[GitChangedFile], path: &str, status: &str) {
        assert!(
            files.iter().any(|f| f.path == path && f.status == status),
            "missing {status} status for {path}; got {files:?}"
        );
    }
}

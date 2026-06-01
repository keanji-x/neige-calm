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
    State(_s): State<AppState>,
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
    State(_s): State<AppState>,
    Query(q): Query<PathQuery>,
) -> Result<Response> {
    let raw = PathBuf::from(q.path.trim());
    read_file_raw_response(&raw).await
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
    State(_s): State<AppState>,
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
    State(_s): State<AppState>,
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

async fn read_file_response(raw: &Path) -> Result<ReadFileResponse> {
    let (canon, meta) = canonicalize_regular_file(raw).await?;
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
    let content_type = image_content_type(&canon)?;
    if meta.len() > MAX_READFILE_RAW_BYTES {
        return Err(CalmError::BadRequest("image exceeds 100 MiB cap".into()));
    }

    let bytes = tokio::fs::read(&canon)
        .await
        .map_err(|e| map_io_err(&canon, e))?;
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
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| map_io_err(path, e))?;
    let truncated = meta.len() > MAX_READFILE_BYTES;
    let limit = std::cmp::min(meta.len(), MAX_READFILE_BYTES) as usize;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| map_io_err(path, e))?;
    let mut buf = Vec::with_capacity(limit);
    use tokio::io::AsyncReadExt;
    file.take(MAX_READFILE_BYTES)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| map_io_err(path, e))?;
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
    use axum::http::StatusCode;
    use http_body_util::BodyExt;
    use std::process::Command as StdCommand;

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

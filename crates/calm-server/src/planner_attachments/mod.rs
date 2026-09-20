//! Planner conversation attachments: `<workspace>/.neige/attachments/<card_id>/{staging,bound}/<attachment_id>`.
//! A file moves `staging/ -> bound/` exactly once, at bind time; `bound/` is never swept because codex may hold its path.

use std::path::{Path, PathBuf};

use calm_types::planner_attachment::{AttachmentFormat, AttachmentId};

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::model::{TrackWorkspace, TrackWorkspaceKind};

pub mod bind;
pub mod dir;
pub mod gc;
pub mod routes;
pub mod sniff;
pub mod store;

/// Server-owned subtree inside a managed workspace; excluded via `.git/info/exclude`, never a `.gitignore`.
pub const NEIGE_DIR: &str = ".neige";

/// The line `ensure_git_exclude_entry` keeps in `.git/info/exclude`.
pub const NEIGE_GIT_EXCLUDE_ENTRY: &str = ".neige/";

/// The most [`used_bytes`] may report before an upload is refused. Bound bytes are never reclaimed, so for a card whose attachments were all sent this is a lifetime total.
/// Enforced under the card's upload lock so concurrent uploads cannot both measure the same "before".
pub const PER_CARD_ATTACHMENT_BUDGET: u64 = 64 * 1024 * 1024;

/// Largest single upload, enforced by `http_body_util::Limited` while the body streams.
pub const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;

/// How long the body read of one upload may run: the per-card lock is held across the body read and nothing else bounds a request body's duration, so a stalled client would hold the lane until the socket dies.
/// Covers only `stream_into`; the filesystem work on either side is outside the clock.
pub const UPLOAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// The one way this module reports a fault whose detail is a host path: every `CalmError` built here is rendered into an HTTP body, so paths go to the log and the returned sentence carries none.
/// Audit: every `.display()` under `planner_attachments/` (excluding `tests.rs`) must sit inside a `tracing::` macro.
fn server_side_fault(summary: &str, paths: &[(&str, &Path)]) -> CalmError {
    for (label, path) in paths {
        tracing::error!(
            target: "planner_attachments",
            path_kind = %label,
            path = %path.display(),
            %summary,
            "planner attachment fault"
        );
    }
    CalmError::Internal(format!("planner attachments: {summary}"))
}

/// `<workspace>/.neige/attachments` for a managed workspace; `Attached` workspaces are user-owned and refused, which also keeps this module free of any recursive delete.
pub fn attachment_root(workspace: &TrackWorkspace, workspace_root: &Path) -> Result<PathBuf> {
    if workspace.kind != TrackWorkspaceKind::Managed {
        return Err(CalmError::BadRequest(
            "attachments require a managed workspace; this track is attached to a directory you \
             own, and neige never writes into one"
                .into(),
        ));
    }
    let path = Path::new(&workspace.path);
    if !path.is_absolute() {
        return Err(server_side_fault(
            "the track's managed workspace path is not absolute",
            &[("workspace", path)],
        ));
    }
    if !path.starts_with(workspace_root) {
        return Err(server_side_fault(
            "the track's managed workspace lies outside the workspace root",
            &[("workspace", path), ("workspace_root", workspace_root)],
        ));
    }
    Ok(path.join(NEIGE_DIR).join("attachments"))
}

const STAGING: &str = "staging";
const BOUND: &str = "bound";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentLocation {
    /// Uploaded, never named by a queue entry. Reclaimable by the sweep.
    Staging,
    /// Named by a queue entry at least once. Permanent.
    Bound,
}

/// An attachment the server has already opened: a descriptor, not a path, so nothing downstream re-opens by name.
#[derive(Debug)]
pub struct OpenAttachment {
    pub file: tokio::fs::File,
    pub size: u64,
    pub format: AttachmentFormat,
    pub location: AttachmentLocation,
}

/// `<root>/<card_id>/bound/<id>` — the path handed to codex. Pure string work, not evidence that anything exists there.
pub fn bound_file_path(root: &Path, card_id: &CardId, id: &AttachmentId) -> PathBuf {
    root.join(card_id.as_str()).join(BOUND).join(id.as_str())
}

/// The one place an [`AttachmentId`] becomes bytes: resolved and opened atomically with `openat2` under `RESOLVE_BENEATH` with every symlink refused — `RESOLVE_BENEATH` alone still follows `card-a/staging -> ../card-b/bound`.
/// `bound/` first, then `staging/`. The opener's errors name host paths, so they are logged and replaced.
pub async fn open_attachment(
    root: &Path,
    card_id: &CardId,
    id: &AttachmentId,
) -> Result<OpenAttachment> {
    for (dir, location) in [
        (BOUND, AttachmentLocation::Bound),
        (STAGING, AttachmentLocation::Staging),
    ] {
        let relative = format!("{}/{dir}/{}", card_id.as_str(), id.as_str());
        match crate::routes::fs::open_workspace_regular_file(
            root,
            &relative,
            crate::routes::fs::WorkspaceSymlinks::Refused,
        )
        .await
        {
            Ok(opened) => {
                return Ok(OpenAttachment {
                    file: opened.file,
                    size: opened.size,
                    format: id.format(),
                    location,
                });
            }
            // No `openat2` on this platform is not "this card does not have that attachment" and must not be reported as one.
            Err(CalmError::Internal(error)) => {
                tracing::error!(
                    target: "planner_attachments",
                    %error,
                    "the workspace opener is unavailable; attachments cannot be read"
                );
                return Err(CalmError::Internal(
                    "planner attachment read: the secure workspace open path is unavailable".into(),
                ));
            }
            Err(error) => {
                tracing::debug!(
                    target: "planner_attachments",
                    %relative,
                    %error,
                    "attachment did not open here"
                );
            }
        }
    }
    Err(CalmError::BadRequest(format!(
        "attachment `{id}` does not belong to card {card_id}"
    )))
}

/// Bytes already spent by this card, across both directories. A missing directory is zero (a fresh card); a broken filesystem is an error, but entries that are not ours (symlinks, sockets, subdirectories) contribute zero rather than latching every later upload.
pub fn used_bytes(dirs: &dir::CardDirs) -> Result<u64> {
    Ok(directory_bytes(dirs.staging())? + directory_bytes(dirs.bound())?)
}

fn directory_bytes<D: dir::DirFd>(directory: &D) -> Result<u64> {
    match dir::regular_entries(directory) {
        Ok(entries) => Ok(entries
            .iter()
            .fold(0u64, |total, entry| total.saturating_add(entry.len))),
        Err(error) => Err(unmeasurable(&error)),
    }
}

/// The one refusal this measurement produces; the host path goes to the log, never into the returned message.
fn unmeasurable(error: &std::io::Error) -> CalmError {
    tracing::warn!(
        target: "planner_attachments",
        %error,
        "could not measure a card's attachment budget"
    );
    // `BadRequest`, not [`server_side_fault`]: the caller can act on it (free
    // space, remove the planted entry), so it is a refusal rather than a fault.
    CalmError::BadRequest(format!(
        "cannot measure this card's attachment budget: {error}"
    ))
}

/// The placeholder a redacted `localImage` path is replaced with.
pub const REDACTED_LOCAL_IMAGE_PATH: &str = "[redacted]";

/// Strip absolute host paths out of a stored harness-item `params` blob before it is put on the wire.
/// A reduction, not an invariant: the same route ships whatever else codex put in a notification. Walks the whole document because codex decides the nesting.
pub fn redact_local_image_paths(params: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(params) else {
        // Not JSON we can walk: stored opaque, goes out opaque; only serde output is ever stored, so no `localImage` item came from it.
        return params.to_string();
    };
    if !redact_in_place(&mut value) {
        return params.to_string();
    }
    serde_json::to_string(&value).unwrap_or_else(|_| params.to_string())
}

/// Returns whether anything was rewritten, so an untouched document keeps its
/// exact original bytes rather than being re-serialized.
fn redact_in_place(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let mut changed = false;
            if map.get("type").and_then(serde_json::Value::as_str) == Some("localImage")
                && let Some(path) = map.get_mut("path")
                && path.is_string()
            {
                *path = serde_json::Value::String(REDACTED_LOCAL_IMAGE_PATH.to_string());
                changed = true;
            }
            for nested in map.values_mut() {
                changed |= redact_in_place(nested);
            }
            changed
        }
        serde_json::Value::Array(items) => {
            let mut changed = false;
            for nested in items.iter_mut() {
                changed |= redact_in_place(nested);
            }
            changed
        }
        _ => false,
    }
}

/// REST path the browser reads an attachment back from; re-exported rather than re-derived so every surface uses one builder.
pub use calm_types::planner_attachment::attachment_url;

#[cfg(test)]
mod tests;

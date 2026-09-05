//! #1505 S6 — planner conversation attachments: where the bytes live and how a
//! request names one.
//!
//! ```text
//! <workspace>/.neige/attachments/<card_id>/staging/<attachment_id>
//! <workspace>/.neige/attachments/<card_id>/bound/<attachment_id>
//! ```
//!
//! `staging/` holds bytes that have been uploaded and never referenced by a
//! queue entry; `bound/` holds bytes a queue entry has referenced at least
//! once. A file moves `staging/ -> bound/` exactly once, at bind time, and its
//! path is final from that instant — nothing in the harness run loop ever
//! touches the disk, so a re-queued message's attachment cannot go missing.
//!
//! # The two directories are different types on purpose
//!
//! `bound/` has no deletion path at all: once codex may have been handed a
//! path, that path has to keep resolving, and there is no reader anywhere that
//! could tell us it is safe to remove. [`StagingDir`] and [`BoundDir`] are
//! separate newtypes with no conversion between them, and
//! [`gc::remove_expired_staged_files`] accepts only the former. A grep over the
//! source cannot show that a `PathBuf` returned by `bound_dir` never reaches a
//! delete call; the type system can, and
//! `tests/ui/bound_dir_cannot_be_deleted.rs` pins it.

use std::path::{Path, PathBuf};

use calm_types::planner_attachment::{AttachmentFormat, AttachmentId};

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::model::{TrackWorkspace, TrackWorkspaceKind};

pub mod gc;
pub mod routes;
pub mod sniff;
pub mod store;

/// Server-owned subtree inside a managed workspace. Excluded from git through
/// `.git/info/exclude`; writing a `.gitignore` is illegal on this path (a
/// tracked file in a repository the user may later inspect).
pub const NEIGE_DIR: &str = ".neige";

/// The line `ensure_git_exclude_entry` keeps in `.git/info/exclude`.
pub const NEIGE_GIT_EXCLUDE_ENTRY: &str = ".neige/";

/// Total bytes one card's attachments may occupy, across `staging/` and
/// `bound/` together.
///
/// Attachments are never reclaimed automatically (see the module docs), so this
/// is the only bound on the subtree. Exceeding it is a refusal the user can
/// see, not a silent eviction of bytes codex may still be asked to read.
pub const PER_CARD_ATTACHMENT_BUDGET: u64 = 64 * 1024 * 1024;

/// Largest single upload. One gate, enforced by
/// `http_body_util::Limited` while the body streams.
pub const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;

/// `<workspace>/.neige/attachments` for a managed workspace.
///
/// `Attached` workspaces are directories the *user* owns — never created, never
/// written to — so attachments are refused there rather than routed to some
/// bypass directory. That refusal is also what keeps this slice free of any
/// recursive delete: a managed workspace is carried away wholesale by
/// `recycle_track_workspace`, an attached one would need its own removal code.
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
        return Err(CalmError::Internal(format!(
            "managed workspace path `{}` is not absolute",
            workspace.path
        )));
    }
    if !path.starts_with(workspace_root) {
        return Err(CalmError::Internal(format!(
            "managed workspace path `{}` is outside the workspace root `{}`",
            workspace.path,
            workspace_root.display()
        )));
    }
    Ok(path.join(NEIGE_DIR).join("attachments"))
}

/// Uploaded, not yet referenced by any queue entry. The only directory anything
/// is ever deleted from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagingDir(PathBuf);

/// Referenced by a queue entry at least once. Nothing deletes from here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundDir(PathBuf);

impl StagingDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl BoundDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// `<root>/<card_id>/staging`.
pub fn staging_dir(root: &Path, card_id: &CardId) -> StagingDir {
    StagingDir(root.join(card_id.as_str()).join("staging"))
}

/// `<root>/<card_id>/bound`.
pub fn bound_dir(root: &Path, card_id: &CardId) -> BoundDir {
    BoundDir(root.join(card_id.as_str()).join("bound"))
}

/// The one place an [`AttachmentId`] becomes a path.
///
/// `bound/` first, then `staging/`. An id that stats in neither is a
/// `BadRequest` — which is also the whole cross-card forgery answer: the
/// directory comes from the card in the URL, so another card's id simply is not
/// there. No comparison, no ownership column, no second check.
pub fn resolve(
    root: &Path,
    card_id: &CardId,
    id: &AttachmentId,
) -> Result<(PathBuf, AttachmentFormat)> {
    let bound = bound_dir(root, card_id).path().join(id.as_str());
    if bound.is_file() {
        return Ok((bound, id.format()));
    }
    let staged = staging_dir(root, card_id).path().join(id.as_str());
    if staged.is_file() {
        return Ok((staged, id.format()));
    }
    Err(CalmError::BadRequest(format!(
        "attachment `{id}` does not belong to card {card_id}"
    )))
}

/// Bytes already spent by this card, across both directories.
///
/// Fail-closed: a directory that exists but cannot be read, or an entry whose
/// metadata cannot be taken, is an error. This number gates a write, so an
/// unknown value must refuse rather than admit. A directory that does not exist
/// yet is zero — that is a fresh card, not an unreadable one.
pub fn used_bytes(root: &Path, card_id: &CardId) -> Result<u64> {
    let staging = staging_dir(root, card_id);
    let bound = bound_dir(root, card_id);
    Ok(directory_bytes(staging.path())? + directory_bytes(bound.path())?)
}

fn directory_bytes(dir: &Path) -> Result<u64> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(CalmError::BadRequest(format!(
                "cannot measure the attachment budget for {}: {error}",
                dir.display()
            )));
        }
    };
    let mut total = 0u64;
    for entry in entries {
        let entry = entry.map_err(|error| {
            CalmError::BadRequest(format!(
                "cannot measure the attachment budget for {}: {error}",
                dir.display()
            ))
        })?;
        // Follows symlinks, unlike `DirEntry::metadata`: an entry whose size
        // cannot be established must refuse the upload, not be skipped over.
        let meta = std::fs::metadata(entry.path()).map_err(|error| {
            CalmError::BadRequest(format!(
                "cannot measure the attachment budget entry {}: {error}",
                entry.path().display()
            ))
        })?;
        if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

/// REST path the browser reads an attachment back from. Built here so no client
/// ever composes one.
pub fn attachment_url(card_id: &CardId, id: &AttachmentId) -> String {
    format!(
        "/api/cards/{}/planner/attachments/{}",
        card_id.as_str(),
        id.as_str()
    )
}

#[cfg(test)]
mod tests;

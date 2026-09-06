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
//! # This slice ships no bind path — uploads expire
//!
//! S6-PR1 is the disk half only. Nothing in this slice writes into `bound/`:
//! the bind path (a queue entry taking a reference, and the `staging/ ->
//! bound/` move) arrives in S6-PR2. Until it lands, every uploaded attachment
//! stays in `staging/`, so [`gc::sweep_staging`] removes it once it is older
//! than [`gc::ORPHAN_TTL`] and the URL in the upload response then answers
//! `400`. That is a declared boundary of the slice, restated on
//! [`calm_types::planner_attachment::UploadAttachmentResponse::url`], not a
//! retention guarantee.
//!
//! # The two directories are different types on purpose
//!
//! `bound/` is not swept: once codex may have been handed a path, that path has
//! to keep resolving, and there is no reader anywhere that could tell us it is
//! safe to remove. [`StagingDir`] and [`BoundDir`] are separate newtypes, and
//! [`gc::remove_staged_file`] — the only `remove_file` in this module, called
//! by [`gc::sweep_staging_at`] and by the upload's abandon path — takes only
//! the former.
//!
//! What the trybuild fence in `tests/ui/bound_dir_cannot_be_deleted.rs` proves
//! is exactly one statement and no more: **there is no conversion from
//! [`BoundDir`] into [`StagingDir`]**, so the fence turns red the moment an
//! `impl From<BoundDir> for StagingDir` is added. It does not prove that a
//! `BoundDir`'s path never reaches a delete — [`BoundDir::path`] is `pub`, so
//! `std::fs::remove_dir_all(bound_dir(root, &card).path())` compiles anywhere,
//! and the fence would stay green. Keeping `bound/` undeleted is a property of
//! the call sites in this module, not of the type system.

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

/// Total bytes one card's uploads may add to `staging/` and `bound/` together.
///
/// Not a bound on the subtree's size: [`used_bytes`] counts the regular files
/// directly in those two directories, so bytes parked in a subdirectory, or
/// behind a symlink, by anything else with write access to the workspace are
/// invisible to it and keep being invisible however many there are.
///
/// Bound bytes are never reclaimed, so uploads have to be refused rather than
/// evicted: exceeding the budget is an error the user can see, not a silent
/// eviction of bytes codex may still be asked to read. What the number bounds
/// is the regular files [`used_bytes`] counts — i.e. what this store wrote,
/// plus anything else that happens to be a regular file in the two
/// directories. It is enforced under the card's upload lock (see
/// [`store::store_upload`]), so concurrent uploads cannot each measure the same
/// "before" and both fit.
pub const PER_CARD_ATTACHMENT_BUDGET: u64 = 64 * 1024 * 1024;

/// Largest single upload. One gate, enforced by
/// `http_body_util::Limited` while the body streams.
pub const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;

/// How long one upload may hold its card's turn, measured from the first byte
/// read to the rename.
///
/// #1515 review round 2. The per-card lock that makes the budget honest is also
/// a lane one client can sit in: the guard is held across the body read, and
/// nothing else in this server bounds a request body's duration (there is no
/// `TimeoutLayer`). Without this, a connection that sends a 12-byte PNG header
/// and then stops holds the lane until the socket dies, and every later upload
/// on that card waits behind it.
///
/// Generous on purpose — 8 MiB over a bad mobile link is minutes, and a refusal
/// the user did not earn is worse than a lane held a while — but finite, which
/// is the property the lock needs.
pub const UPLOAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

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

/// A path this subtree is willing to serve: a *regular* file, stat'd with
/// `symlink_metadata` so the link itself is described rather than followed.
///
/// Nothing in this module ever creates a symlink here, so an entry that is one
/// was planted by something else with write access to the workspace — an agent,
/// say — and serving its target would turn this endpoint into a reader for a
/// path the server never chose. `false` for every non-regular entry, and for an
/// entry that cannot be stat'd at all.
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
}

/// The one place an [`AttachmentId`] becomes a path.
///
/// `bound/` first, then `staging/`. An id that names no regular file in either
/// is a `BadRequest` — which is also the whole cross-card forgery answer: the
/// directory comes from the card in the URL, so another card's id simply is not
/// there. No comparison, no ownership column, no second check.
pub fn resolve(
    root: &Path,
    card_id: &CardId,
    id: &AttachmentId,
) -> Result<(PathBuf, AttachmentFormat)> {
    let bound = bound_dir(root, card_id).path().join(id.as_str());
    if is_regular_file(&bound) {
        return Ok((bound, id.format()));
    }
    let staged = staging_dir(root, card_id).path().join(id.as_str());
    if is_regular_file(&staged) {
        return Ok((staged, id.format()));
    }
    Err(CalmError::BadRequest(format!(
        "attachment `{id}` does not belong to card {card_id}"
    )))
}

/// Bytes already spent by this card, across both directories.
///
/// Counts the regular files this store writes. A directory that does not exist
/// yet is zero — that is a fresh card, not an unreadable one.
///
/// # What refuses and what does not
///
/// This number gates a write, so a *broken filesystem* — a directory that
/// exists but cannot be enumerated, an entry that cannot be stat'd for any
/// reason other than having vanished — is an error, and the upload is refused.
///
/// An entry that simply is not one of ours is a different thing and must not
/// refuse: symlinks, sockets and subdirectories contribute zero and are stepped
/// over. Anything with write access to the workspace can create one, and if a
/// single planted entry made this function `Err`, every later upload on that
/// card would be refused forever — a latch, not a budget. The stat is
/// `symlink_metadata`, which describes a dangling link instead of failing on
/// it, so that classification is available at all.
pub fn used_bytes(root: &Path, card_id: &CardId) -> Result<u64> {
    let staging = staging_dir(root, card_id);
    let bound = bound_dir(root, card_id);
    Ok(directory_bytes(staging.path())? + directory_bytes(bound.path())?)
}

fn directory_bytes(dir: &Path) -> Result<u64> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(unmeasurable(dir, &error)),
    };
    let mut total = 0u64;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return Err(unmeasurable(dir, &error)),
        };
        let meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            // Gone between `read_dir` and the stat — a concurrent sweep, or a
            // hand deleting a file. Absent bytes are zero bytes.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(unmeasurable(dir, &error)),
        };
        // `file_type()` from `symlink_metadata` is `is_file()` only for a
        // regular file: a symlink is a symlink here, whatever it points at.
        if meta.file_type().is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

/// The one refusal this measurement produces.
///
/// #1515 review round 2. The host path goes to the log, never into the returned
/// message: every `CalmError` built in this module is rendered into an HTTP
/// error body, and the workspace's layout on the server's disk is not something
/// a client asked for or can act on. The same rule holds for
/// [`store::store_upload`]'s failures and for the read-back's.
fn unmeasurable(dir: &Path, error: &std::io::Error) -> CalmError {
    tracing::warn!(
        target: "planner_attachments",
        dir = %dir.display(),
        %error,
        "could not measure a card's attachment budget"
    );
    CalmError::BadRequest(format!(
        "cannot measure this card's attachment budget: {error}"
    ))
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

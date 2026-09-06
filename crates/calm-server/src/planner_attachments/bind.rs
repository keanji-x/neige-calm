//! The one writer of `bound/`: turning an uploaded attachment into a permanent
//! one.
//!
//! # Where this runs, and why not in the run loop
//!
//! Binding happens on the REST side, in the same request that writes the queue
//! entry naming the attachment, and **before** that entry is written. The run
//! loop never touches the attachment subtree at all.
//!
//! The alternative — promote when a batch is drained and issued — was
//! considered and is wrong for a reason that is mechanical rather than
//! aesthetic. [`crate::harness::run_loop`] can put a drained batch back:
//! `turn/start` failing, or a missing thread id, both re-buffer the entries at
//! the head of the queue. So "an entry that is still queued has its attachment
//! in `staging/`" is not true, and any scheme keyed on it would leave a
//! re-queued message pointing at a file the sweep is still entitled to remove.
//! Promoting before the entry exists gives a single failure direction: a bind
//! that succeeds and is then followed by a failed enqueue leaks bytes into
//! `bound/`, and a reference is never left dangling. Leaked bytes cost budget
//! (#1505 GAP-A12); a dangling reference costs a silently degraded turn, since
//! codex replaces an unreadable `localImage` with placeholder text and reports
//! nothing.
//!
//! # Copy, not rename, and the source is a descriptor
//!
//! The bytes are copied out of the descriptor
//! [`super::open_attachment`] returned, not read back from the path. That
//! matters because the path is the thing that cannot be trusted twice: the
//! workspace is writable by the agent working in it, so between a check on a
//! name and a later use of that name a component can be replaced. The opener
//! resolves with `openat2` under `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` and
//! hands back a descriptor, and a descriptor cannot be re-pointed.
//!
//! What that leaves uncovered is the *destination*: `bound/` is a directory
//! name this module joins, and nothing above stops something else from having
//! replaced it with a symlink first. So the bind does not assert its way past
//! that — after the rename it re-opens through the same guarded opener and
//! requires the attachment to come back as [`AttachmentLocation::Bound`]. A
//! tampered subtree therefore ends in a refusal with the staged file still in
//! place, not in bytes written somewhere unexpected.

use std::path::Path;

use calm_types::planner_attachment::{AttachmentId, PlannerAttachment};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::per_card_lock::{PerCardLocks, lock_card};

use super::{AttachmentLocation, bound_dir, bound_file_path, gc, open_attachment, staging_dir};

/// The most attachments one message may carry.
///
/// A product limit, and also the thing that keeps a fold bounded: two adjacent
/// queued messages merging under backpressure concatenate their attachment
/// lists, so without a cap at both the entry point and the fold a long enough
/// backlog could collapse into one entry naming arbitrarily many images.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 8;

/// A bound attachment as the **server** holds it.
///
/// Distinct from the wire [`PlannerAttachment`] by exactly one field, and that
/// field is the reason the two types are not one: `path` is an absolute host
/// path and must not reach a browser. It is here because the harness needs it
/// at drain time and has no other way to get it — a harness knows its card and
/// its runtime, not its workspace directory — and because recording it at bind
/// time is the literal statement the design makes: the path is decided once,
/// before the entry exists, and nothing later recomputes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundAttachment {
    pub id: AttachmentId,
    pub size: u64,
    /// Absolute path under `<workspace>/.neige/attachments/<card>/bound/`.
    /// Server-side only.
    pub path: String,
}

impl BoundAttachment {
    /// The half of this that is safe to send to a client.
    ///
    /// The card is a parameter because the read-back url is derived from it,
    /// and this type does not carry one: an attachment's card is a fact about
    /// where it is being read, and the two call sites (the pending-queue page
    /// and the transcript segments) both already know it.
    pub fn wire(&self, card_id: &CardId) -> PlannerAttachment {
        PlannerAttachment::new(card_id, self.id.clone(), self.size)
    }
}

/// Refuse a list that is too long, or that names the same attachment twice.
///
/// Duplicates are refused rather than de-duplicated: a message naming one
/// image twice is a client that lost track of its own state, and quietly
/// collapsing it would make the cap depend on how the client happened to
/// repeat itself.
pub fn validate_attachment_list(ids: &[AttachmentId]) -> Result<()> {
    if ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(CalmError::BadRequest(format!(
            "a message may carry at most {MAX_ATTACHMENTS_PER_MESSAGE} attachments; got {}",
            ids.len()
        )));
    }
    for (index, id) in ids.iter().enumerate() {
        if ids[..index].contains(id) {
            return Err(CalmError::BadRequest(format!(
                "attachment `{id}` is named twice in the same message"
            )));
        }
    }
    Ok(())
}

/// Make every named attachment permanent, and report where its bytes now live.
///
/// Idempotent: an id that is already bound is checked and returned unchanged,
/// which is what makes a re-send of the same attachment list safe.
///
/// Takes the card's turn from [`crate::per_card_lock`] — the same lock the
/// upload takes — for the whole batch. The upload measures the card's budget
/// and then writes under it, and a bind moving bytes between the two counted
/// directories in the middle of that would make the measurement describe a
/// state that no longer exists.
pub async fn bind_attachments(
    root: &Path,
    card_id: &CardId,
    ids: &[AttachmentId],
    locks: &PerCardLocks,
) -> Result<Vec<BoundAttachment>> {
    validate_attachment_list(ids)?;
    if ids.is_empty() {
        // No lock for the overwhelmingly common case. Taking one would be
        // harmless but would put every text-only message behind any upload in
        // flight on the same card.
        return Ok(Vec::new());
    }
    let _turn = lock_card(locks, card_id.as_str()).await;
    let mut bound = Vec::with_capacity(ids.len());
    for id in ids {
        bound.push(bind_one(root, card_id, id).await?);
    }
    Ok(bound)
}

async fn bind_one(root: &Path, card_id: &CardId, id: &AttachmentId) -> Result<BoundAttachment> {
    let opened = open_attachment(root, card_id, id).await?;
    let path = bound_file_path(root, card_id, id);
    let recorded = |size: u64| -> Result<BoundAttachment> {
        let path = path.to_str().ok_or_else(|| {
            super::server_side_fault(
                "the attachment path is not valid UTF-8 and cannot be sent to codex",
                &[("attachment", &path)],
            )
        })?;
        Ok(BoundAttachment {
            id: id.clone(),
            size,
            path: path.to_string(),
        })
    };
    if opened.location == AttachmentLocation::Bound {
        return recorded(opened.size);
    }

    let size = opened.size;
    copy_into_bound(root, card_id, id, opened.file).await?;

    // The destination directory is a name, not a descriptor, so the only
    // honest way to know the bytes ended up where this function claims is to
    // ask the guarded opener again. A `Staging` answer here means the rename
    // did not produce a readable `bound/<id>` — a replaced `bound` component
    // is the way that happens — and the staged file is still there, so
    // refusing loses nothing.
    let verified = open_attachment(root, card_id, id).await?;
    if verified.location != AttachmentLocation::Bound {
        return Err(CalmError::BadRequest(format!(
            "attachment `{id}` could not be made permanent on card {card_id}; it was left staged"
        )));
    }

    // Only now, and only through the one deletion door this module has.
    let staging = staging_dir(root, card_id);
    if let Err(error) = gc::remove_staged_file(&staging, id.as_str()) {
        // The copy is committed and verified; the message may be sent. A
        // surviving staged twin costs the card its bytes twice against the
        // budget until the sweep reaches it, which is a cost, not a fault.
        tracing::warn!(
            target: "planner_attachments::bind",
            card_id = %card_id,
            attachment_id = %id,
            %error,
            "bound an attachment but could not remove its staged copy"
        );
    }
    recorded(size)
}

/// Stream the verified descriptor into `bound/<id>`, via a temporary that
/// lives in `staging/`.
///
/// The temporary is in `staging/` rather than beside its destination for one
/// reason: `staging/` is the only directory this module is allowed to delete
/// from. A partial file left by a crash has to be reclaimable, and
/// [`gc::sweep_staging`] already reclaims exactly this shape. Putting it in
/// `bound/` would create permanent garbage that no code path may remove.
async fn copy_into_bound(
    root: &Path,
    card_id: &CardId,
    id: &AttachmentId,
    mut source: tokio::fs::File,
) -> Result<()> {
    let bound = bound_dir(root, card_id);
    if let Err(error) = tokio::fs::create_dir_all(bound.path()).await {
        return Err(super::server_side_fault(
            &format!("the bound directory could not be created: {error}"),
            &[("bound", bound.path())],
        ));
    }
    let staging = staging_dir(root, card_id);
    let part_name = format!("{}.bind.part", id.as_str());
    let part = staging.path().join(&part_name);

    let mut file = match create_new(&part).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // A previous bind of this same id died between creating the
            // temporary and renaming it. Removing it goes through the same
            // door as every other deletion here.
            gc::remove_staged_file(&staging, &part_name).map_err(|error| {
                super::server_side_fault(
                    &format!("a stale bind temporary could not be removed: {error}"),
                    &[("part", &part)],
                )
            })?;
            create_new(&part).await.map_err(|error| {
                super::server_side_fault(
                    &format!("the bind temporary could not be created: {error}"),
                    &[("part", &part)],
                )
            })?
        }
        Err(error) => {
            return Err(super::server_side_fault(
                &format!("the bind temporary could not be created: {error}"),
                &[("part", &part)],
            ));
        }
    };

    let copy = async {
        tokio::io::copy(&mut source, &mut file).await?;
        file.flush().await?;
        // Durable under the temporary name before it takes the final one, so
        // a crash can only leave a `.part` the sweep removes — never a
        // truncated file under a name a queue entry already points at.
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&part, bound.path().join(id.as_str())).await
    }
    .await;
    if let Err(error) = copy {
        let _ = gc::remove_staged_file(&staging, &part_name);
        return Err(super::server_side_fault(
            &format!("the attachment could not be copied into the bound directory: {error}"),
            &[("part", &part)],
        ));
    }
    Ok(())
}

/// `O_CREAT | O_EXCL` — which is also what stops the destination from being a
/// symlink somebody else planted: `open` with both flags refuses to follow a
/// final-component symlink rather than writing through it.
async fn create_new(path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
}

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
//! # Where the guarantee lives now
//!
//! It does not live in this file. Every filesystem operation a bind performs
//! goes through [`super::dir`], which exposes descriptors and single-component
//! names and nothing that can be joined into a path — see its module docs for
//! the mechanism and for the grep that keeps it true.
//!
//! This file used to end its header with a sentence about what the guard
//! "buys": that *neige* never writes outside the directory it derived. That
//! sentence was false when it was written, in the same subsystem — the upload
//! path had had none of this treatment and was still creating and renaming
//! through `staging.path().join(..)` — and it is the second header sentence in
//! this module to have read as "the class is closed" while half the class was
//! untouched. There is no third one here. What the class is, and what closing
//! it rests on, is stated once in `dir`, next to the code that does it.

use std::path::Path;

use calm_types::planner_attachment::{AttachmentId, PlannerAttachment};
use serde::{Deserialize, Serialize};

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::per_card_lock::{PerCardLocks, lock_card};

use super::dir;
use super::{AttachmentLocation, bound_file_path, open_attachment};

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
    let turn = lock_card(locks, card_id.as_str()).await;
    // Resolve everything first, publish second, and let the publish own the
    // lock.
    //
    // #1505 S6 review. Consolidating the writes into `spawn_blocking` moved
    // the card's turn out from under them: dropping the handle does not cancel
    // the closure, but it DOES drop the guard, so a client disconnecting
    // mid-copy left the write running with the lane open. That mattered
    // because the same round taught the browser's retry to carry the same
    // attachment ids, so the retry's bind could start on the same id while the
    // first was still copying.
    //
    // Two things close it, and both are wanted. The guard moves INTO the
    // blocking closure below, so it is released when the write finishes rather
    // than when the caller stops waiting; and every temporary now has a name
    // unique to its attempt ([`dir::Name::temporary`]), so two attempts on one
    // id cannot name the same file even if the lock were somehow lost.
    let mut resolved = Vec::with_capacity(ids.len());
    for id in ids {
        // Reads first, and all of them.
        //
        // Ordering, not taste: a card that has never had an upload has no
        // attachment directories at all, and opening them before this loop
        // turned "that id belongs to another card" — a 400 the client can act
        // on — into a 500 about a directory. A cancellation here also writes
        // nothing, so the guard being dropped by it costs nothing.
        let opened = open_attachment(root, card_id, id).await?;
        resolved.push(Resolved {
            id: id.clone(),
            size: opened.size,
            already_bound: opened.location == AttachmentLocation::Bound,
            source: opened.file.into_std().await,
        });
    }

    // Only now, and still before anything is written.
    let dirs = dir::open_card_dirs(root, card_id).await?;

    // Kept out of the closure, which consumes `resolved`.
    let sizes = resolved.iter().map(|item| item.size).collect::<Vec<_>>();

    let published: Result<()> = tokio::task::spawn_blocking(move || -> Result<()> {
        // Held until every publish is done, whatever the caller is doing.
        let _turn = turn;
        for item in &resolved {
            if item.already_bound {
                // Nothing to publish — but there may be a staged twin to
                // retire. A bind that died between its rename and its unlink
                // leaves the same bytes in both counted directories, and
                // `used_bytes` counts both, so the card is charged twice.
                // Retiring it here closes that at the first re-bind instead of
                // waiting for the orphan TTL.
                let _ = dir::unlink_staged(dirs.staging(), &dir::Name::of(&item.id));
                continue;
            }
            publish(&dirs, item)?;
        }
        Ok(())
    })
    .await
    .map_err(|error| {
        CalmError::Internal(format!(
            "planner attachment bind: a filesystem step did not complete: {error}"
        ))
    })?;
    published?;

    ids.iter()
        .zip(sizes.iter())
        .map(|(id, size)| recorded(root, card_id, id, *size))
        .collect()
}

/// One attachment, read and classified, ready to publish.
struct Resolved {
    id: AttachmentId,
    size: u64,
    already_bound: bool,
    source: std::fs::File,
}

/// The absolute path a queue entry records, as a `String`.
///
/// A name handed to another process, not a path this module ever calls a
/// filesystem function with — see [`super::dir`] for why that distinction is
/// the whole design.
fn recorded(
    root: &Path,
    card_id: &CardId,
    id: &AttachmentId,
    size: u64,
) -> Result<BoundAttachment> {
    let path = bound_file_path(root, card_id, id);
    let text = path.to_str().ok_or_else(|| {
        super::server_side_fault(
            "the attachment path is not valid UTF-8 and cannot be sent to codex",
            &[("attachment", &path)],
        )
    })?;
    Ok(BoundAttachment {
        id: id.clone(),
        size,
        path: text.to_string(),
    })
}

/// Write the bytes under a temporary name in `staging/`, publish them into
/// `bound/` with `renameat`, and retire the staged original.
///
/// The temporary lives in `staging/` rather than beside its destination for
/// one reason: `staging/` is the only directory this module may delete from,
/// and a partial file left by a crash has to be reclaimable.
/// [`super::gc::sweep_staging`] already reclaims exactly this shape.
///
/// Every step is a descriptor and one name; nothing here builds a path.
fn publish(dirs: &dir::CardDirs, item: &Resolved) -> Result<()> {
    let temporary = dir::Name::temporary();
    let mut file = dir::create_new(dirs.staging(), &temporary).map_err(|error| {
        super::server_side_fault(
            &format!("the bind temporary could not be created: {error}"),
            &[],
        )
    })?;

    let mut source = &item.source;
    let moved = (|| -> std::io::Result<()> {
        std::io::copy(&mut source, &mut file)?;
        // Durable under the temporary name before it takes the final one, so a
        // crash can only leave a temporary the sweep removes — never a
        // truncated file under a name a queue entry already points at.
        file.sync_all()?;
        drop(file);
        dir::rename_into_bound(
            dirs.staging(),
            &temporary,
            dirs.bound(),
            &dir::Name::of(&item.id),
        )?;
        // `sync_all` on the FILE makes its contents durable; it says nothing
        // about the directory ENTRY the rename created. Without this, a power
        // failure after the queue entry commits can leave a durable reference
        // to a name that is not in `bound/` — and codex answers a missing
        // `localImage` with placeholder text and no error. Both directories:
        // the rename changed an entry in each.
        dir::sync_bound(dirs.bound())?;
        dir::sync_staging(dirs.staging())?;
        Ok(())
    })();

    if let Err(error) = moved {
        let _ = dir::unlink_staged(dirs.staging(), &temporary);
        return Err(super::server_side_fault(
            &format!("the attachment could not be published into the bound directory: {error}"),
            &[],
        ));
    }

    // The original is redundant now, and leaving it would charge the card for
    // the same bytes twice. Best-effort: the publish above is committed, so a
    // failure here is a cost the sweep reclaims, not a reason to refuse a
    // message whose image is already permanent.
    if let Err(error) = dir::unlink_staged(dirs.staging(), &dir::Name::of(&item.id)) {
        tracing::warn!(
            target: "planner_attachments::bind",
            attachment = %item.id,
            %error,
            "bound an attachment but could not remove its staged copy"
        );
    }
    let _ = dir::sync_staging(dirs.staging());
    Ok(())
}

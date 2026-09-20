//! The one writer of `bound/`: turning an uploaded attachment into a permanent one.
//! Binding happens on the REST side, before the queue entry naming the attachment is written; the run loop never touches the attachment subtree.

use std::path::Path;

use calm_types::planner_attachment::{AttachmentId, PlannerAttachment};
use serde::{Deserialize, Serialize};

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::per_card_lock::{PerCardLocks, lock_card};

use super::dir;
use super::{AttachmentLocation, bound_file_path, open_attachment};

/// The most attachments one message may carry; also bounds the backpressure fold that concatenates adjacent queued messages' attachment lists.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 8;

/// A bound attachment as the server holds it: `path` is an absolute host path and must not reach a browser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundAttachment {
    pub id: AttachmentId,
    pub size: u64,
    /// Absolute path under `<workspace>/.neige/attachments/<card>/bound/`; server-side only.
    pub path: String,
}

impl BoundAttachment {
    pub fn wire(&self, card_id: &CardId) -> PlannerAttachment {
        PlannerAttachment::new(card_id, self.id.clone(), self.size)
    }
}

/// Refuse a list that is too long, or that names the same attachment twice.
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

/// Make every named attachment permanent and report where its bytes now live. Idempotent: an already-bound id is returned unchanged.
/// Holds the card's turn for the whole batch: the upload measures the card's budget under the same lock, and a bind moving bytes between the counted directories mid-measurement would invalidate it.
pub async fn bind_attachments(
    root: &Path,
    card_id: &CardId,
    ids: &[AttachmentId],
    locks: &PerCardLocks,
) -> Result<Vec<BoundAttachment>> {
    validate_attachment_list(ids)?;
    if ids.is_empty() {
        // No lock for the empty case: it would put every text-only message behind any upload in flight on the same card.
        return Ok(Vec::new());
    }
    let turn = lock_card(locks, card_id.as_str()).await;
    // Resolve everything first, publish second, and let the publish own the lock: the guard moves into the blocking closure so it is released when the write finishes, not when the caller stops waiting.
    let mut resolved = Vec::with_capacity(ids.len());
    for id in ids {
        // Reads first, all of them: a card with no upload has no attachment directories, and opening them before this loop turns "that id belongs to another card" (a 400) into a 500 about a directory.
        let opened = open_attachment(root, card_id, id).await?;
        resolved.push(Resolved {
            id: id.clone(),
            size: opened.size,
            already_bound: opened.location == AttachmentLocation::Bound,
            source: opened.file.into_std().await,
        });
    }

    let dirs = dir::open_card_dirs(root, card_id).await?;

    let sizes = resolved.iter().map(|item| item.size).collect::<Vec<_>>();

    let published: Result<()> = tokio::task::spawn_blocking(move || -> Result<()> {
        // Held until every publish is done, whatever the caller is doing.
        let _turn = turn;
        for item in &resolved {
            if item.already_bound {
                // A bind that died between its rename and its unlink leaves the same bytes in both counted directories; retire the staged twin here instead of waiting for the orphan TTL.
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

struct Resolved {
    id: AttachmentId,
    size: u64,
    already_bound: bool,
    source: std::fs::File,
}

/// The absolute path a queue entry records: a name handed to another process, never a path this module calls a filesystem function with.
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

/// Write the bytes under a temporary name in `staging/`, publish them into `bound/` with `renameat`, and retire the staged original.
/// The temporary lives in `staging/` because that is the only directory this module may delete from, so a partial file left by a crash is reclaimable.
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
        // Durable under the temporary name before the rename, so a crash can only leave a temporary the sweep removes.
        file.sync_all()?;
        drop(file);
        dir::rename_into_bound(
            dirs.staging(),
            &temporary,
            dirs.bound(),
            &dir::Name::of(&item.id),
        )?;
        // `sync_all` on the file says nothing about the directory entry the rename created, and codex answers a missing `localImage` with placeholder text and no error. Both directories: the rename changed an entry in each.
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

    // Best-effort: the publish is committed, so a failure here is a cost the sweep reclaims.
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

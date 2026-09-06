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
//! # Every path this module touches is a descriptor plus one name
//!
//! The workspace is writable by the agent working in it, so a *name* is
//! something that can mean a different file between one syscall and the next.
//! `openat2` under `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` is how this
//! repository answers that for reads, and a descriptor it returns cannot be
//! re-pointed.
//!
//! **The first version of this file applied that to the source only.** The
//! bytes were copied out of a guarded descriptor and then written to
//! `bound_dir(root, card).join(id)` — a plain path — with `create_dir_all` and
//! `rename`, both of which resolve every component with no `RESOLVE_*` flags
//! at all. `ln -s ../card-b/bound <root>/card-a/bound` was enough to land card
//! A's attachment in card B's never-swept `bound/`, and pointing the link
//! anywhere on the same filesystem wrote attacker-named bytes there. The
//! re-open that was supposed to catch it ran AFTER the rename and returned a
//! refusal that deleted nothing, so the bytes stayed. The sentence that used
//! to be here — "a tampered subtree ends in a refusal with the staged file
//! still in place, not in bytes written somewhere unexpected" — was false in
//! its most important half.
//!
//! What replaces it is structural rather than a second check.
//! [`open_card_dirs`] resolves BOTH of a card's directories through
//! [`crate::routes::fs::open_workspace_directory`] — the same `openat2`, the
//! same flags — and hands back two descriptors. Every write then goes through
//! a syscall that takes a descriptor plus a single NAME component with no `/`
//! in it: `openat` with `O_EXCL | O_NOFOLLOW`, `renameat`, `unlinkat`,
//! `fsync`. There is no intermediate component left for anything to swap, so
//! there is nothing to re-verify afterwards; a replaced `bound` or `staging`
//! is `ELOOP` from the opener, **before** a byte is written.
//!
//! ## What this does not cover, stated rather than implied
//!
//! The absolute path recorded on the queue entry is read later by codex, in a
//! different process, which resolves it as a path. Nothing here can stop a
//! component being replaced between the bind and that read. That is not a
//! capability this feature grants: codex's working directory IS this
//! workspace, so a workspace-writing agent redirecting a file codex reads is
//! something it can do without any of this. What the guard above buys is that
//! *neige* never writes outside the directory it derived — #1505 GAP-A16.

use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;

use calm_types::planner_attachment::{AttachmentId, PlannerAttachment};
use serde::{Deserialize, Serialize};

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::per_card_lock::{PerCardLocks, lock_card};
use crate::routes::fs::{WorkspaceSymlinks, open_workspace_directory};

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
    let size = opened.size;
    let already_bound = opened.location == AttachmentLocation::Bound;
    // Both directories are resolved before anything is written, and both are
    // resolved the same way the read path resolves a file. A `bound` or
    // `staging` component that is not a real directory beneath this root is
    // `ELOOP` or `EXDEV` here, with no bytes moved.
    let dirs = open_card_dirs(root, card_id).await?;
    let source = opened.file.into_std().await;
    let name = id.as_str().to_string();

    let published = blocking(move || {
        if already_bound {
            // Nothing to publish — but there may be a staged twin to retire.
            //
            // A bind that died between its rename and its unlink leaves the
            // same bytes in both directories, and `used_bytes` counts both, so
            // the card is charged twice. Left alone that is not merely
            // untidy: a card pushed over its budget refuses every upload, and
            // the sweep that would reclaim the twin runs inside an upload, so
            // the card cannot recover by itself. Retiring it here closes it at
            // the first re-bind instead of waiting for the orphan TTL, and
            // `store_upload` now sweeps before it measures so the TTL path
            // works too.
            retire_staged(&dirs.staging, &name);
            return Ok(());
        }
        publish(&dirs, &name, source)
    })
    .await;

    match published {
        Ok(()) => recorded(size),
        Err(error) => Err(error),
    }
}

/// One card's two directories, as descriptors.
///
/// [`StagingFd`] and [`BoundFd`] are separate newtypes for the same reason
/// [`super::StagingDir`] and [`super::BoundDir`] are: deletion takes only the
/// former, and there is no conversion from the latter into it. The rename
/// takes both, which is correct — moving a file INTO `bound/` is a write, not
/// a deletion.
struct CardDirs {
    staging: StagingFd,
    bound: BoundFd,
}

struct StagingFd(OwnedFd);
struct BoundFd(OwnedFd);

/// Resolve `<card>/staging` and `<card>/bound`, creating the latter if it does
/// not exist yet.
///
/// `bound/` is created with `mkdirat` relative to the card's own descriptor
/// rather than with `create_dir_all` on a joined path. The difference is the
/// whole point: `create_dir_all` walks and follows every component, so a
/// symlinked `<card>` would have it create — and later write into — a
/// directory somewhere else entirely. `mkdirat` against a descriptor resolves
/// exactly one name, and `EEXIST` is not an error here because two requests on
/// the same card can race to create it.
async fn open_card_dirs(root: &Path, card_id: &CardId) -> Result<CardDirs> {
    use nix::sys::stat::{Mode, mkdirat};

    // The opener's own errors name host paths, exactly as `open_attachment`
    // found; they are logged and replaced with one sentence that names only
    // the card. A symlinked or missing component is not something the client
    // did or can fix, so it is a fault rather than a refusal.
    let guarded = |what: &'static str| {
        move |error: CalmError| {
            tracing::error!(
                target: "planner_attachments::bind",
                directory = what,
                %error,
                "an attachment directory did not resolve beneath the attachment root"
            );
            CalmError::Internal(format!(
                "planner attachment bind: this card's `{what}` directory is not a directory \
                 beneath its attachment root; nothing was written"
            ))
        }
    };
    let card = open_workspace_directory(root, card_id.as_str(), WorkspaceSymlinks::Refused)
        .await
        .map_err(guarded("card"))?;
    let card_fd = card.as_raw_fd();
    tokio::task::spawn_blocking(move || {
        match mkdirat(Some(card_fd), "bound", Mode::from_bits_truncate(0o700)) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => Ok(()),
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(|error| {
        CalmError::Internal(format!(
            "planner attachment bind: a filesystem step did not complete: {error}"
        ))
    })?
    .map_err(|error| {
        super::server_side_fault(
            &format!("the bound directory could not be created: {error}"),
            &[("root", root)],
        )
    })?;
    // `card` is held until here so the descriptor `mkdirat` used stays open.
    drop(card);

    let staging = open_workspace_directory(
        root,
        &format!("{}/staging", card_id.as_str()),
        WorkspaceSymlinks::Refused,
    )
    .await
    .map_err(guarded("staging"))?;
    let bound = open_workspace_directory(
        root,
        &format!("{}/bound", card_id.as_str()),
        WorkspaceSymlinks::Refused,
    )
    .await
    .map_err(guarded("bound"))?;
    Ok(CardDirs {
        staging: StagingFd(staging),
        bound: BoundFd(bound),
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
/// Every step names a descriptor and one component. Nothing here builds a
/// path.
fn publish(dirs: &CardDirs, name: &str, mut source: std::fs::File) -> Result<()> {
    let part = format!("{name}.bind.part");
    let mut file = create_new(&dirs.staging, &part)
        .or_else(|error| {
            if error == nix::errno::Errno::EEXIST {
                // A previous bind of this same id died between creating the
                // temporary and renaming it.
                retire_staged(&dirs.staging, &part);
                create_new(&dirs.staging, &part)
            } else {
                Err(error)
            }
        })
        .map_err(|error| {
            super::server_side_fault(
                &format!("the bind temporary could not be created: {error}"),
                &[("staging", Path::new(name))],
            )
        })?;

    let moved = (|| -> std::io::Result<()> {
        std::io::copy(&mut source, &mut file)?;
        // Durable under the temporary name before it takes the final one, so a
        // crash can only leave a `.part` the sweep removes — never a truncated
        // file under a name a queue entry already points at.
        file.sync_all()?;
        drop(file);
        nix::fcntl::renameat(
            Some(dirs.staging.0.as_raw_fd()),
            part.as_str(),
            Some(dirs.bound.0.as_raw_fd()),
            name,
        )?;
        // #1505 S6 review. `sync_all` on the FILE makes its contents durable;
        // it says nothing about the directory ENTRY the rename created. Without
        // this, a power failure after the queue entry commits can leave a
        // durable reference to a name that is not in `bound/` — and codex
        // answers a missing `localImage` with placeholder text and no error.
        // Both directories: the rename changed an entry in each.
        nix::unistd::fsync(dirs.bound.0.as_raw_fd())?;
        nix::unistd::fsync(dirs.staging.0.as_raw_fd())?;
        Ok(())
    })();

    if let Err(error) = moved {
        retire_staged(&dirs.staging, &part);
        return Err(super::server_side_fault(
            &format!("the attachment could not be published into the bound directory: {error}"),
            &[("staging", Path::new(name))],
        ));
    }

    // The original is redundant now, and leaving it would charge the card for
    // the same bytes twice. Best-effort: the publish above is committed, so a
    // failure here is a cost the sweep reclaims, not a reason to refuse a
    // message whose image is already permanent.
    retire_staged(&dirs.staging, name);
    let _ = nix::unistd::fsync(dirs.staging.0.as_raw_fd());
    Ok(())
}

/// The one deletion door, and it takes a [`StagingFd`].
///
/// Best-effort by signature: every caller has already committed something that
/// makes this file redundant, so a failure is a reclaimable cost rather than a
/// fault. It is logged, never returned.
fn retire_staged(staging: &StagingFd, name: &str) {
    use nix::unistd::{UnlinkatFlags, unlinkat};
    match unlinkat(
        Some(staging.0.as_raw_fd()),
        name,
        UnlinkatFlags::NoRemoveDir,
    ) {
        Ok(()) | Err(nix::errno::Errno::ENOENT) => {}
        Err(error) => tracing::warn!(
            target: "planner_attachments::bind",
            name,
            %error,
            "a staged attachment copy could not be retired; the sweep will reclaim it"
        ),
    }
}

/// `O_CREAT | O_EXCL | O_NOFOLLOW` relative to the staging descriptor.
///
/// `O_EXCL` with `O_CREAT` refuses to follow a final-component symlink rather
/// than writing through it, and there is no other component to follow.
fn create_new(
    staging: &StagingFd,
    name: &str,
) -> std::result::Result<std::fs::File, nix::errno::Errno> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    use std::os::fd::FromRawFd;

    let raw = openat(
        Some(staging.0.as_raw_fd()),
        name,
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_WRONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o600),
    )?;
    // SAFETY: `openat` returned a new owned descriptor and this is its only
    // conversion into an owning Rust value.
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}

/// Run one synchronous filesystem sequence off the runtime.
///
/// `store`'s module docs state the rule for this subtree: every synchronous
/// filesystem step goes through `spawn_blocking`. This file is where the whole
/// publish sequence — open, copy, fsync, rename, unlink — is charged to it as
/// one unit.
async fn blocking<F>(work: F) -> Result<()>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(CalmError::Internal(format!(
            "planner attachment bind: a filesystem step did not complete: {error}"
        ))),
    }
}

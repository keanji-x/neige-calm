//! Writing one uploaded attachment to disk: git-exclude, sweep, measure the budget, stream to `<id>.part`, `sync_all`, rename, and only then return the id.
//! Sweep through rename run under the card's turn: the budget is a read-then-write, and the sweep deletes by age so it could unlink a still-streaming `.part`.

use std::path::Path;

use axum::body::{Body, Bytes};
use calm_types::planner_attachment::{AttachmentFormat, AttachmentId};
use http_body_util::{BodyExt, Limited};
use tokio::io::AsyncWriteExt;

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::operation::workspace_lease::ensure_git_exclude_entry;
use crate::per_card_lock::{PerCardLocks, lock_card};

use super::dir;
use super::sniff::{SNIFF_PREFIX_BYTES, sniff};
use super::{
    MAX_ATTACHMENT_BYTES, NEIGE_GIT_EXCLUDE_ENTRY, PER_CARD_ATTACHMENT_BUDGET, used_bytes,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredAttachment {
    pub id: AttachmentId,
    pub size: u64,
}

/// Run the whole upload sequence. `repo_root` is the managed workspace's git work tree; `root` is `<repo_root>/.neige/attachments`.
pub async fn store_upload(
    root: &Path,
    repo_root: &Path,
    card_id: &CardId,
    locks: &PerCardLocks,
    deadline: std::time::Duration,
    body: Body,
) -> Result<StoredAttachment> {
    // Fail-closed, and first: a failure here means the file would be visible to git, so it must mean no file exists.
    let repo = repo_root.to_path_buf();
    blocking(move || {
        ensure_git_exclude_entry(&repo, NEIGE_GIT_EXCLUDE_ENTRY).map_err(|error| {
            // Both the repository path and the underlying error carry host paths, so both go to the log and neither is returned.
            tracing::error!(
                target: "planner_attachments::store",
                repo = %repo.display(),
                %error,
                "could not exclude the attachment subtree from git"
            );
            CalmError::Internal(format!(
                "planner attachment upload: {NEIGE_GIT_EXCLUDE_ENTRY} could not be excluded from \
                 this workspace's git, so no bytes were written"
            ))
        })
    })
    .await?;

    // Everything from the measurement to the rename is one card's turn.
    let _turn = lock_card(locks, card_id.as_str()).await;

    // `staging/` is created here because the upload is the one caller that can arrive before the card has any directories at all.
    let dirs = std::sync::Arc::new(dir::create_card_dirs(repo_root, root, card_id).await?);

    // Reclaim before measuring: a card at or over its ceiling refuses every upload, so a sweep that ran only after success could never free it. Safe on every request because the sweep is handed a descriptor for THIS card's `staging/`.
    let swept = std::sync::Arc::clone(&dirs);
    blocking(move || {
        super::gc::sweep_staging(swept.staging());
        Ok(())
    })
    .await?;

    // (3) Cheap refusal before a single byte is read off the socket.
    let measured = std::sync::Arc::clone(&dirs);
    let already_used = blocking(move || used_bytes(&measured)).await?;
    if already_used >= PER_CARD_ATTACHMENT_BUDGET {
        return Err(budget_exhausted());
    }

    write_body(&dirs, already_used, deadline, body).await
}

/// Run one synchronous step off the runtime; a `JoinError` is a panic or a runtime shutdown and must not be mistaken for success.
async fn blocking<T, F>(work: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(CalmError::Internal(format!(
            "planner attachment upload: a filesystem step did not complete: {error}"
        ))),
    }
}

fn budget_exhausted() -> CalmError {
    CalmError::BadRequest(format!(
        "attachment budget exhausted for this card ({} MiB). Attachments are never reclaimed \
         automatically; remove files under `.neige/attachments/` in the track workspace to free \
         space.",
        PER_CARD_ATTACHMENT_BUDGET / (1024 * 1024)
    ))
}

/// Stream the body into `staging/`, sniffing the format from the leading bytes; the temporary is only created once the format is known.
/// Cleanup is [`OpenPart`]'s destructor, which runs on every exit including a dropped handler. The deadline wraps only `stream_into`, not `finish`: a `spawn_blocking` rename is not cancelled by dropping its future, so a deadline firing mid-`finish` could publish after a refusal.
async fn write_body(
    dirs: &std::sync::Arc<dir::CardDirs>,
    already_used: u64,
    deadline: std::time::Duration,
    body: Body,
) -> Result<StoredAttachment> {
    let mut open: Option<OpenPart> = None;
    // `open` lives here rather than inside the timed future so the part
    // survives the cancellation and is unlinked by its destructor.
    let written = match tokio::time::timeout(
        deadline,
        stream_into(&mut open, dirs, already_used, body),
    )
    .await
    {
        Ok(written) => written?,
        Err(_elapsed) => return Err(upload_timed_out(deadline)),
    };
    let mut part = open
        .take()
        .expect("a successful stream leaves an open part");
    let id = part.finish().await?;
    Ok(StoredAttachment { id, size: written })
}

fn upload_timed_out(deadline: std::time::Duration) -> CalmError {
    // 400 rather than 408: this crate has no 408 variant, and the cause — a
    // body that stopped arriving — is the client's, which is what 4xx says.
    CalmError::BadRequest(format!(
        "the attachment body stopped arriving; an upload has {} seconds to \
         finish because it holds this card's upload turn while it runs",
        deadline.as_secs()
    ))
}

/// Consume the body, returning the number of bytes written. On `Ok`, `open`
/// holds the `.part` still awaiting its fsync and rename.
async fn stream_into(
    open: &mut Option<OpenPart>,
    dirs: &std::sync::Arc<dir::CardDirs>,
    already_used: u64,
    body: Body,
) -> Result<u64> {
    // One size gate, not two: `Limited` counts data frames as they arrive, so a second lower threshold would make this one unreachable.
    let mut limited = Limited::new(body, MAX_ATTACHMENT_BYTES as usize);

    let mut prefix: Vec<u8> = Vec::with_capacity(SNIFF_PREFIX_BYTES);
    let mut pending: Vec<Bytes> = Vec::new();
    let mut written: u64 = 0;

    loop {
        let frame = match limited.frame().await {
            Some(frame) => frame.map_err(|error| map_body_error(error.as_ref()))?,
            None => break,
        };
        let Ok(chunk) = frame.into_data() else {
            continue;
        };
        if chunk.is_empty() {
            continue;
        }

        written = written.saturating_add(chunk.len() as u64);
        if already_used.saturating_add(written) > PER_CARD_ATTACHMENT_BUDGET {
            return Err(budget_exhausted());
        }

        if let Some(part) = open.as_mut() {
            part.write(&chunk).await?;
            continue;
        }

        if prefix.len() < SNIFF_PREFIX_BYTES {
            let wanted = SNIFF_PREFIX_BYTES - prefix.len();
            prefix.extend_from_slice(&chunk[..wanted.min(chunk.len())]);
        }
        pending.push(chunk);
        if prefix.len() < SNIFF_PREFIX_BYTES {
            // Not enough bytes to decide yet. Bounded: at most
            // SNIFF_PREFIX_BYTES frames can be this small.
            continue;
        }
        flush_pending(open, dirs, &prefix, &mut pending).await?;
    }

    if open.is_none() {
        // Fewer than SNIFF_PREFIX_BYTES bytes arrived; PNG, JPEG and GIF are still recognisable from a short prefix, WebP correctly is not.
        flush_pending(open, dirs, &prefix, &mut pending).await?;
    }
    Ok(written)
}

/// Sniff, create the `.part`, and write everything buffered so far.
async fn flush_pending(
    open: &mut Option<OpenPart>,
    dirs: &std::sync::Arc<dir::CardDirs>,
    prefix: &[u8],
    pending: &mut Vec<Bytes>,
) -> Result<()> {
    let format = sniff(prefix).ok_or_else(unsupported_format)?;
    *open = Some(OpenPart::create(dirs, format).await?);
    let part = open.as_mut().expect("just assigned");
    for buffered in pending.drain(..) {
        part.write(&buffered).await?;
    }
    Ok(())
}

fn unsupported_format() -> CalmError {
    CalmError::BadRequest(
        "attachment must be a PNG, JPEG, GIF or WebP image. The declared Content-Type is not \
         consulted: the file's own leading bytes are. SVG is refused."
            .into(),
    )
}

/// `413` when the single size gate trips, `400` for any other body failure.
fn map_body_error(error: &(dyn std::error::Error + 'static)) -> CalmError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if current.is::<http_body_util::LengthLimitError>() {
            return CalmError::PayloadTooLarge(format!(
                "attachment exceeds the {} MiB per-file limit",
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            ));
        }
        source = current.source();
    }
    CalmError::BadRequest(format!("attachment upload body failed: {error}"))
}

/// The `<id>.part` file, before it earns its final name. The destructor is the cleanup: it runs on `return`, `?`, a cancelled future and a dropped handler, and does nothing once `published` is set.
/// `file` is an `Option` so `finish` can close the descriptor before the rename while still taking `&mut self`.
struct OpenPart {
    id: AttachmentId,
    file: Option<tokio::fs::File>,
    /// So the destructor can unlink through a descriptor rather than reconstructing a path.
    dirs: std::sync::Arc<dir::CardDirs>,
    /// This attempt's own temporary name, unique per attempt.
    temporary: dir::Name,
    published: bool,
}

impl Drop for OpenPart {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        drop(self.file.take());
        if let Err(error) = dir::unlink_staged(self.dirs.staging(), &self.temporary) {
            tracing::warn!(
                target: "planner_attachments::store",
                attachment = %self.id,
                %error,
                "could not remove an abandoned attachment part"
            );
        }
    }
}

impl OpenPart {
    async fn create(
        dirs: &std::sync::Arc<dir::CardDirs>,
        format: AttachmentFormat,
    ) -> Result<Self> {
        let raw = format!("{}.{}", uuid::Uuid::new_v4(), format.ext());
        let id = AttachmentId::parse(&raw).map_err(|error| {
            CalmError::Internal(format!(
                "planner attachment upload: minted a bad id: {error}"
            ))
        })?;
        let temporary = dir::Name::part_of(&id);
        let opened = {
            let dirs = std::sync::Arc::clone(dirs);
            let temporary = temporary.clone();
            let id = id.clone();
            blocking(move || {
                dir::create_new(dirs.staging(), &temporary).map_err(|error| {
                    CalmError::Internal(format!(
                        "planner attachment upload: cannot create the staged file for `{id}`: \
                         {error}"
                    ))
                })
            })
            .await?
        };
        Ok(OpenPart {
            id,
            file: Some(tokio::fs::File::from_std(opened)),
            dirs: std::sync::Arc::clone(dirs),
            temporary,
            published: false,
        })
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.file
            .as_mut()
            .expect("an open part still holds its descriptor")
            .write_all(bytes)
            .await
            .map_err(|error| {
                CalmError::Internal(format!("planner attachment upload: write: {error}"))
            })
    }

    /// `sync_all` then `rename`. Takes `&mut self` so a failure leaves the part owned by its destructor; `published` is set only after the rename returned `Ok`.
    async fn finish(&mut self) -> Result<AttachmentId> {
        let mut file = self
            .file
            .take()
            .expect("an open part still holds its descriptor");
        file.flush().await.map_err(|error| {
            CalmError::Internal(format!("planner attachment upload: flush: {error}"))
        })?;
        file.sync_all().await.map_err(|error| {
            CalmError::Internal(format!("planner attachment upload: fsync: {error}"))
        })?;
        drop(file);
        let dirs = std::sync::Arc::clone(&self.dirs);
        let temporary = self.temporary.clone();
        let final_name = dir::Name::of(&self.id);
        let id = self.id.clone();
        blocking(move || {
            dir::rename_within_staging(dirs.staging(), &temporary, &final_name).map_err(
                move |error| {
                    // Names the attachment, not the host paths: this string reaches the client.
                    CalmError::Internal(format!(
                        "planner attachment upload: `{id}` could not be published under its \
                         final name: {error}"
                    ))
                },
            )?;
            // The rename created a directory ENTRY, and the file's own
            // `sync_all` says nothing about that.
            let _ = dir::sync_staging(dirs.staging());
            Ok(())
        })
        .await?;
        self.published = true;
        Ok(self.id.clone())
    }
}

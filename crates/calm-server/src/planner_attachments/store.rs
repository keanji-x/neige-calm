//! Writing one uploaded attachment to disk.
//!
//! # The order is the correctness argument
//!
//! codex reads an attachment by path and, when the read or the decode fails,
//! substitutes placeholder text without telling anyone. There is no receipt and
//! no retry, so every guarantee has to be established before the id leaves this
//! function:
//!
//! 1. `.neige/` is in `.git/info/exclude` — **before** any byte is written, so
//!    a worker running `git add -A` in the same workspace can never see the
//!    file at all;
//! 2. the staging sweep runs, so the measurement below is taken against what
//!    the card actually still holds rather than against bytes that have
//!    already expired;
//! 3. the card's byte budget is measured once, and every frame is checked
//!    against that number plus the bytes read so far — the card's turn, taken
//!    just before the measurement, is what keeps the number true for the whole
//!    stream;
//! 4. the bytes go to `<id>.part`;
//! 5. `sync_all`;
//! 6. `rename(<id>.part -> <id>)`;
//! 7. only now is the id returned.
//!
//! A client therefore cannot reference an attachment that is not yet durable
//! under its final name, and a crash between 4 and 5 leaves a `.part` that the
//! staging sweep removes.
//!
//! # One upload per card at a time
//!
//! Steps 2-5 run under the card's entry in [`crate::per_card_lock`]. Two things
//! need that, and neither is a probability argument:
//!
//! * the budget is a read-then-write. Without the lock, N concurrent uploads on
//!   a fresh card all measure `used_bytes == 0` before any of them renames, and
//!   N * [`MAX_ATTACHMENT_BYTES`] lands however small the budget is;
//! * the staging sweep deletes by age, and an upload still streaming has a
//!   `.part` whose mtime stopped advancing when its last frame arrived. A
//!   stalled client's `.part` would age past [`super::gc::ORPHAN_TTL`], a
//!   second upload's sweep would unlink it, and the first upload's `rename`
//!   would then fail on a file it still holds open.
//!
//! The cost is that one card's uploads are serial, and the lane has to have an
//! end: a client that opens a POST and stalls would otherwise hold it for as
//! long as its socket lives, and nothing else in this server bounds a request
//! body's duration. So the stream-and-publish runs under
//! [`super::UPLOAD_DEADLINE`], after which the body is refused, the `.part` is
//! removed and the turn is released. That is the bound — 120 seconds, not
//! "until the client gives up". Every other card is unaffected either way; the
//! map is keyed by card id.
//!
//! # Blocking work runs on `spawn_blocking`
//!
//! `ensure_git_exclude_entry` forks `git rev-parse` and reads and appends a
//! file; [`super::used_bytes`] and the sweep walk directories. All three are
//! synchronous, and this function is `async`, so each goes through
//! `spawn_blocking` rather than parking a runtime worker for the duration —
//! concurrent uploads would otherwise stall unrelated requests on the same
//! runtime.

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

/// What an accepted upload became.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredAttachment {
    pub id: AttachmentId,
    pub size: u64,
}

/// Run the whole upload sequence. See the module docs for why the order is what
/// it is.
///
/// `repo_root` is the managed workspace's git work tree; `root` is
/// `<repo_root>/.neige/attachments`, already derived by
/// [`super::attachment_root`].
pub async fn store_upload(
    root: &Path,
    repo_root: &Path,
    card_id: &CardId,
    locks: &PerCardLocks,
    deadline: std::time::Duration,
    body: Body,
) -> Result<StoredAttachment> {
    // (1) Fail-closed, and first: a failure here means the file would be
    // visible to git, so it must mean no file exists.
    let repo = repo_root.to_path_buf();
    blocking(move || {
        ensure_git_exclude_entry(&repo, NEIGE_GIT_EXCLUDE_ENTRY).map_err(|error| {
            // Both the repository path and the underlying error carry host
            // paths, so both go to the log and neither is returned. See
            // `super::server_side_fault` for why this is a rule and not a
            // judgement call.
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

    // Everything from the measurement to the rename is one card's turn. See
    // the module docs: the budget and the sweep both need it.
    let _turn = lock_card(locks, card_id.as_str()).await;

    // Both directories, resolved through the `openat2` primitive, before any
    // byte is written. `staging/` is created here because the upload is the one
    // caller that can arrive before the card has any directories at all.
    let dirs = std::sync::Arc::new(dir::create_card_dirs(repo_root, root, card_id).await?);

    // (2) Reclaim before measuring.
    //
    // #1505 S6 review. This used to run only AFTER a successful upload, which
    // made the budget a one-way door: a card at or over its ceiling refuses
    // every upload at the check below, so the sweep that would free space
    // never ran, so the card stayed over its ceiling forever. The orphan TTL
    // was supposed to be the way out and structurally could not be.
    //
    // The over-budget state is reachable without any abuse: a bind that dies
    // between its rename and its unlink leaves the same bytes in both counted
    // directories. Sweeping first means an expired staged file is gone before
    // the number that gates this request is taken.
    //
    // Reachability changed with the reordering — the sweep used to run only
    // after a successful upload, and now runs on every request before the
    // refusal. That is safe for one reason and it is not the ordering: the
    // sweep is handed a descriptor for THIS card's `staging/`, so there is no
    // path for it to walk out of. Before the descriptors, this reordering was
    // the difference between a deletion primitive an attacker had to get past
    // sniff, size and budget to reach, and one every POST reached.
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

/// Run one synchronous step off the runtime.
///
/// A `JoinError` here is a panic inside the closure or a runtime shutdown;
/// neither can be reported as anything but an internal failure, and neither may
/// be mistaken for the step having succeeded.
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

/// Stream the body into `staging/`, sniffing the format from the leading bytes.
///
/// The temporary file is only created once the format is known, so a body that
/// is not one of the four formats never produces a file at all.
///
/// # Cleanup is the destructor's, not an arm's
///
/// Round 2 wrote "the cleanup arm below is the only one, and it sees every
/// failure" here. It was false: an arm runs only when this function *returns*,
/// and the common ending for an upload is the handler future being **dropped**
/// — a client reset mid-body — which left a `.part` behind. Cleanup therefore
/// hangs off [`OpenPart`]'s `Drop`, which is the one thing that does run on
/// every way a value leaves scope: `return`, `?`, the timeout cancelling
/// `stream_into`, and the whole handler being dropped. There is no explicit
/// cleanup call left to forget, so there is no longer a claim to get wrong.
///
/// # The deadline covers the body read, and nothing else
///
/// [`super::UPLOAD_DEADLINE`] wraps `stream_into`, the one step whose duration
/// a client controls. It deliberately does **not** wrap `finish`.
///
/// `tokio::fs::rename` is `spawn_blocking` underneath, and dropping the future
/// that awaits a `spawn_blocking` handle does not cancel the closure. With the
/// publish inside the clock, the deadline could fire mid-`finish`: the client
/// would be told 400, the `.part` would be unlinked, and the detached rename
/// would then land, leaving the attachment published under its final name after
/// a refusal. Keeping `finish` outside the clock is what makes "refused" and
/// "published" exclusive. The cost is stated where the constant is defined: the
/// deadline bounds the client's half of the turn, not a hung filesystem.
///
/// One accepted consequence, reviewed and left as is: if the *handler* is
/// dropped while `finish` awaits the rename, the detached rename still lands,
/// while the [`OpenPart`] is dropped with `published == false`. The destructor
/// then unlinks `<id>.part`, which by then may already be gone. Either order is
/// safe and exclusivity still holds — no refusal was sent to anyone — but the
/// published file can be left in `staging/` under a name no client was ever
/// told, where the orphan sweep collects it after
/// [`super::gc::ORPHAN_TTL`].
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
    // One size gate, not two. `Limited` has no `Content-Length` pre-check — it
    // counts data frames as they arrive — so a second, lower business-level
    // threshold would short-circuit first and this one would be unreachable.
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
        // Fewer than SNIFF_PREFIX_BYTES bytes arrived. PNG, JPEG and GIF are
        // still recognisable from a short prefix; a WebP header cannot fit in
        // fewer than 12 bytes, so it correctly is not.
        flush_pending(open, dirs, &prefix, &mut pending).await?;
    }
    Ok(written)
}

/// Sniff, create the `.part`, and write everything buffered so far.
///
/// The part is moved into `open` before the first write only so the caller can
/// go on using it; the unlink on failure is [`OpenPart`]'s destructor either
/// way, so there is no arm here to forget.
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

/// The `<id>.part` file, before it earns its final name.
///
/// # The destructor is the cleanup
///
/// #1505 review round 3. Every earlier shape put the unlink in an error arm,
/// and every earlier shape missed a way out: a `?` past the arm (round 1), the
/// handler future being dropped so the arm never ran at all (round 2). A
/// destructor has no such gap — it runs on `return`, on `?`, on a cancelled
/// future and on a dropped handler — so it is where the unlink belongs.
///
/// Two consequences, both deliberate:
///
/// * `Drop` cannot await, so the unlink is blocking: a direct
///   [`dir::unlink_staged`], which is `unlinkat` against `staging/`'s own
///   descriptor. A guarded `tokio::runtime::Handle::try_current()` +
///   `spawn_blocking` would also work whenever a runtime exists, so this is a
///   choice and not a constraint: one `unlinkat(2)` on a local file is cheaper
///   than a task.
/// * once [`OpenPart::finish`] has renamed the file, `published` is set and the
///   destructor does nothing. Without it a late destructor would unlink a name
///   that a later upload could legitimately have recreated.
///
/// `file` is an `Option` so `finish` can close the descriptor before the rename
/// while still taking `&mut self`, which is what keeps a failing `finish` on
/// the cleanup path.
struct OpenPart {
    id: AttachmentId,
    file: Option<tokio::fs::File>,
    /// The directories this part lives in, so the destructor can unlink
    /// through a descriptor rather than reconstructing a path.
    dirs: std::sync::Arc<dir::CardDirs>,
    /// This attempt's own temporary name. Unique per attempt — see
    /// [`dir::Name::temporary`] for why a shared `<id>.part` was a
    /// corruption channel rather than merely a collision.
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
        // `O_CREAT | O_EXCL | O_NOFOLLOW` relative to `staging/`'s descriptor:
        // it refuses an existing name, refuses to follow a symlink sitting on
        // it, and has no other component to resolve.
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

    /// `sync_all` then `rename` — steps 4 and 5.
    ///
    /// Takes `&mut self` so a failure here leaves the part alive, and therefore
    /// still owned by its destructor. `published` is set only after the rename
    /// has returned `Ok`, which is what stops the destructor from unlinking a
    /// name that is now a real attachment.
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
                    // Names the attachment, not the host paths: this string
                    // reaches the client, and the workspace layout is not the
                    // client's business.
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

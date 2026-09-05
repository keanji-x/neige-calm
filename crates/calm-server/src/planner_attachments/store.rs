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
//! 2. the card's byte budget is measured, and re-measured as the body streams;
//! 3. the bytes go to `<id>.part`;
//! 4. `sync_all`;
//! 5. `rename(<id>.part -> <id>)`;
//! 6. only now is the id returned.
//!
//! A client therefore cannot reference an attachment that is not yet durable
//! under its final name, and a crash between 4 and 5 leaves a `.part` that the
//! staging sweep removes.

use std::path::Path;

use axum::body::{Body, Bytes};
use calm_types::planner_attachment::{AttachmentFormat, AttachmentId};
use http_body_util::{BodyExt, Limited};
use tokio::io::AsyncWriteExt;

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::operation::workspace_lease::ensure_git_exclude_entry;

use super::sniff::{SNIFF_PREFIX_BYTES, sniff};
use super::{
    MAX_ATTACHMENT_BYTES, NEIGE_GIT_EXCLUDE_ENTRY, PER_CARD_ATTACHMENT_BUDGET, StagingDir,
    staging_dir, used_bytes,
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
    body: Body,
) -> Result<StoredAttachment> {
    // (1) Fail-closed, and first: a failure here means the file would be
    // visible to git, so it must mean no file exists.
    ensure_git_exclude_entry(repo_root, NEIGE_GIT_EXCLUDE_ENTRY).map_err(|error| {
        CalmError::Internal(format!(
            "planner attachment upload: cannot exclude {NEIGE_GIT_EXCLUDE_ENTRY} from git in {}: \
             {error}",
            repo_root.display()
        ))
    })?;

    // (2) Cheap refusal before a single byte is read off the socket.
    let already_used = used_bytes(root, card_id)?;
    if already_used >= PER_CARD_ATTACHMENT_BUDGET {
        return Err(budget_exhausted());
    }

    let staging = staging_dir(root, card_id);
    tokio::fs::create_dir_all(staging.path())
        .await
        .map_err(|error| {
            CalmError::Internal(format!(
                "planner attachment upload: create {}: {error}",
                staging.path().display()
            ))
        })?;

    let stored = write_body(&staging, already_used, body).await;
    if stored.is_ok() {
        // The sweep is here rather than on a timer: this is the only moment the
        // directory is known to have changed, its cardinality is tiny, and it
        // holds no lock that the harness run loop needs.
        super::gc::sweep_staging(&staging);
    }
    stored
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
/// is not one of the four formats never produces a file at all. Every failure
/// after that point removes the `.part` it created — there is exactly one
/// cleanup site, below, so no error arm can forget it.
async fn write_body(
    staging: &StagingDir,
    already_used: u64,
    body: Body,
) -> Result<StoredAttachment> {
    let mut open: Option<OpenPart> = None;
    match stream_into(&mut open, staging, already_used, body).await {
        Ok(written) => {
            let part = open
                .take()
                .expect("a successful stream leaves an open part");
            let id = part.finish(staging).await?;
            Ok(StoredAttachment { id, size: written })
        }
        Err(error) => {
            if let Some(part) = open.take() {
                part.abandon(staging).await;
            }
            Err(error)
        }
    }
}

/// Consume the body, returning the number of bytes written. On `Ok`, `open`
/// holds the `.part` still awaiting its fsync and rename.
async fn stream_into(
    open: &mut Option<OpenPart>,
    staging: &StagingDir,
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
        *open = Some(flush_pending(staging, &prefix, &mut pending).await?);
    }

    if open.is_none() {
        // Fewer than SNIFF_PREFIX_BYTES bytes arrived. PNG, JPEG and GIF are
        // still recognisable from a short prefix; a WebP header cannot fit in
        // fewer than 12 bytes, so it correctly is not.
        *open = Some(flush_pending(staging, &prefix, &mut pending).await?);
    }
    Ok(written)
}

/// Sniff, create the `.part`, and write everything buffered so far.
async fn flush_pending(
    staging: &StagingDir,
    prefix: &[u8],
    pending: &mut Vec<Bytes>,
) -> Result<OpenPart> {
    let format = sniff(prefix).ok_or_else(unsupported_format)?;
    let mut part = OpenPart::create(staging, format).await?;
    for buffered in pending.drain(..) {
        if let Err(error) = part.write(&buffered).await {
            part.abandon(staging).await;
            return Err(error);
        }
    }
    Ok(part)
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
struct OpenPart {
    id: AttachmentId,
    file: tokio::fs::File,
}

impl OpenPart {
    async fn create(staging: &StagingDir, format: AttachmentFormat) -> Result<Self> {
        let raw = format!("{}.{}", uuid::Uuid::new_v4(), format.ext());
        let id = AttachmentId::parse(&raw).map_err(|error| {
            CalmError::Internal(format!(
                "planner attachment upload: minted a bad id: {error}"
            ))
        })?;
        let path = staging.path().join(part_name(&id));
        let file = tokio::fs::File::create(&path).await.map_err(|error| {
            CalmError::Internal(format!(
                "planner attachment upload: create {}: {error}",
                path.display()
            ))
        })?;
        Ok(OpenPart { id, file })
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.write_all(bytes).await.map_err(|error| {
            CalmError::Internal(format!("planner attachment upload: write: {error}"))
        })
    }

    /// `sync_all` then `rename` — steps 4 and 5.
    async fn finish(mut self, staging: &StagingDir) -> Result<AttachmentId> {
        self.file.flush().await.map_err(|error| {
            CalmError::Internal(format!("planner attachment upload: flush: {error}"))
        })?;
        self.file.sync_all().await.map_err(|error| {
            CalmError::Internal(format!("planner attachment upload: fsync: {error}"))
        })?;
        drop(self.file);
        let from = staging.path().join(part_name(&self.id));
        let to = staging.path().join(self.id.as_str());
        tokio::fs::rename(&from, &to).await.map_err(|error| {
            CalmError::Internal(format!(
                "planner attachment upload: rename {} -> {}: {error}",
                from.display(),
                to.display()
            ))
        })?;
        Ok(self.id)
    }

    /// Refusal path: the bytes are dropped, so the `.part` goes too. Best
    /// effort — a leftover `.part` is swept later and is never referenceable.
    async fn abandon(self, staging: &StagingDir) {
        let name = part_name(&self.id);
        drop(self.file);
        if let Err(error) = super::gc::remove_staged_file(staging, &name) {
            tracing::warn!(
                target: "planner_attachments::store",
                file = %name,
                %error,
                "could not remove an abandoned attachment part"
            );
        }
    }
}

fn part_name(id: &AttachmentId) -> String {
    format!("{}.part", id.as_str())
}

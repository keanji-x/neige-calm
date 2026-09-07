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
//! # Binding, and what still expires
//!
//! [`bind::bind_attachments`] is the one writer of `bound/`. It runs on the
//! REST side, before a queue entry that names the attachment is written, so an
//! entry in the queue always refers to a file that is already out of the
//! sweep's reach. Nothing in the harness run loop touches the disk: the drain
//! path builds `{"type":"localImage","path":...}` from a path recorded at bind
//! time, so a re-buffered batch cannot find its attachment gone.
//!
//! What still expires is an attachment that is uploaded and never sent:
//! [`gc::sweep_staging`] removes it once it is older than [`gc::ORPHAN_TTL`]
//! and the upload response's url then answers `400`. That window is stated on
//! [`calm_types::planner_attachment::UploadAttachmentResponse::url`].
//!
//! # The two directories are different types on purpose
//!
//! `bound/` is not swept: once codex may have been handed a path, that path has
//! to keep resolving, and there is no reader anywhere that could tell us it is
//! safe to remove. [`dir::StagingFd`] and [`dir::BoundFd`] are separate
//! newtypes over open descriptors, and [`dir::unlink_staged`] — the only
//! deletion this module can express at all — takes only the former.
//!
//! The trybuild fence in `tests/ui/bound_fd_cannot_be_deleted.rs` proves one
//! statement: **there is no conversion from [`dir::BoundFd`] into
//! [`dir::StagingFd`]**, so it turns red the moment an
//! `impl From<BoundFd> for StagingFd` is added.
//!
//! What used to stand here was a paragraph explaining that the fence proved
//! much less than it looked like — because `BoundDir::path` was `pub`, so
//! `std::fs::remove_dir_all(bound_dir(root, &card).path())` compiled anywhere.
//! That escape hatch is gone: [`dir::BoundFd`] exposes no path, no descriptor
//! and no conversion, so there is nothing to hand to a path-based delete, and
//! [`dir`]'s own module docs carry the mechanism and the audit that keeps it
//! true.

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

/// Server-owned subtree inside a managed workspace. Excluded from git through
/// `.git/info/exclude`; writing a `.gitignore` is illegal on this path (a
/// tracked file in a repository the user may later inspect).
pub const NEIGE_DIR: &str = ".neige";

/// The line `ensure_git_exclude_entry` keeps in `.git/info/exclude`.
pub const NEIGE_GIT_EXCLUDE_ENTRY: &str = ".neige/";

/// The most [`used_bytes`] may report before an upload is refused.
///
/// Part ceiling, part lifetime quota, and which part depends on where the
/// bytes are. [`gc::sweep_staging`] reclaims *staged* files older than
/// [`gc::ORPHAN_TTL`], so budget spent on uploads that were never sent comes
/// back after a day. Budget spent on **bound** bytes never comes back: nothing
/// deletes from `bound/`, so for a card whose attachments were all actually
/// sent this constant is a lifetime total, not a ceiling. Binding does not
/// change the number in the ordinary case — [`used_bytes`] counts both
/// directories, and a bind moves bytes between them — it changes whether that
/// number can ever go down again.
///
/// "In the ordinary case" is doing real work in that sentence and is not a
/// hedge. A bind that publishes into `bound/` and then fails, or is killed,
/// before retiring the staged original leaves the same bytes in both
/// directories, and this counts them twice until either the next bind of that
/// id retires the twin or [`gc::sweep_staging`] reaches it. [`bind`] logs that
/// branch rather than hiding it, and [`store::store_upload`] sweeps BEFORE it
/// measures so a card double-charged past the ceiling can still recover.
///
/// The residual gap (an attachment removed from a message before it was sent,
/// or a deleted queue entry, still costs its bytes forever) is #1505 GAP-A12,
/// and this refusal is its only backstop.
///
/// Nor is it a bound on the subtree's size: [`used_bytes`] counts the regular
/// files directly in the two directories, so bytes parked in a subdirectory, or
/// behind a symlink, by anything else with write access to the workspace are
/// invisible to it and keep being invisible however many there are.
///
/// Bound bytes are not reclaimed by anything, which is why exceeding the
/// ceiling has to be a refusal the user can see rather than a silent eviction
/// of bytes codex may still be asked to read. It is enforced under the card's
/// upload lock (see
/// [`store::store_upload`]), so concurrent uploads cannot each measure the same
/// "before" and both fit.
pub const PER_CARD_ATTACHMENT_BUDGET: u64 = 64 * 1024 * 1024;

/// Largest single upload. One gate, enforced by
/// `http_body_util::Limited` while the body streams.
pub const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;

/// How long the *body read* of one upload may run.
///
/// #1505 review round 2. The per-card lock that makes the budget honest is also
/// a lane one client can sit in: the guard is held across the body read, and
/// nothing else in this server bounds a request body's duration (there is no
/// `TimeoutLayer`). Without this, a connection that sends a 12-byte PNG header
/// and then stops holds the lane until the socket dies, and every later upload
/// on that card waits behind it.
///
/// # What it does not bound
///
/// Round 2 called this "how long an upload may hold its card's turn". That is
/// wider than the code and is corrected here. The turn is taken before the
/// budget measurement and released after the staging sweep, and the clock
/// covers only `stream_into` — the client-controlled step — between them. The
/// filesystem work on either side (`used_bytes`, the directory opens, `finish`,
/// the sweep) is outside it, so a workspace on a wedged mount can hold the turn
/// past this deadline with it never firing. Bounding that would mean bounding
/// local filesystem calls, which this server does nowhere; it is recorded as a
/// known gap rather than implied away. `finish` is outside the clock for a
/// second, deliberate reason — see [`store`]'s `write_body`.
///
/// The clock starts when the timeout is constructed, i.e. once the exclude
/// entry, the budget and the staging directory are already done.
///
/// Generous on purpose — 8 MiB over a bad mobile link is minutes, and a refusal
/// the user did not earn is worse than a lane held a while — but finite, which
/// is the property the lock needs.
pub const UPLOAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// The one way this module reports a fault whose detail is a host path.
///
/// #1505 review round 3. Every `CalmError` built anywhere under
/// `planner_attachments` is rendered into an HTTP error body, so a path
/// interpolated into a message is a path handed to the client — and rounds 1
/// and 2 each fixed one message and left the rest of the class. There is now
/// exactly one constructor that takes paths, it puts them in the log, and the
/// returned sentence carries none of them.
///
/// # The scope of this rule, said explicitly because it has been over-read
///
/// #1505 S6 review. This is a rule about **error bodies built by this
/// module**. It is NOT the sentence "no host path reaches a client", and that
/// wider sentence is false in this repository: a track's `cwd` is on the wire
/// by design, and `GET /api/cards/{id}/harness/items` returns each stored
/// `params` blob verbatim — including, once this slice landed, codex's own
/// `{"type":"localImage","path":…}` item. That surface is reduced by
/// [`redact_local_image_paths`], which is a reduction and not a guarantee; the
/// route still carries whatever else codex put in a notification. Anyone
/// citing the rule below for a claim about a route rather than about an error
/// message is citing it for something it never said.
///
/// The class-level check is a grep, and it is the reason this is stated as a
/// rule rather than as a claim about particular messages: **every `.display()`
/// under `crates/calm-server/src/planner_attachments/`, excluding `tests.rs`,
/// sits inside a `tracing::` macro.** The exact audit is
/// `grep -n '\.display()' planner_attachments/*.rs | grep -v tests.rs`; test
/// code builds assertion messages and is not a client-facing surface. A path reaching a `CalmError` would have to appear
/// outside one, so `grep -n '\.display()' planner_attachments/*.rs` and reading
/// the enclosing call is the whole audit. `{error}` interpolations are safe
/// alongside it because `std::fs` I/O errors carry no path of their own — the
/// one place that was false, `ensure_git_exclude_entry`, formats its own paths
/// in, and `store.rs` logs that error instead of returning it.
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

/// The two directory names, single-sourced: [`dir::open_card_dirs`] resolves
/// these components relative to a descriptor and [`open_attachment`] spells
/// the same ones into the relative path it hands the workspace opener.
const STAGING: &str = "staging";
const BOUND: &str = "bound";

/// Which of a card's two directories an attachment was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentLocation {
    /// Uploaded, never named by a queue entry. Reclaimable by the sweep.
    Staging,
    /// Named by a queue entry at least once. Permanent.
    Bound,
}

/// An attachment the server has already opened.
///
/// A descriptor, not a path: nothing downstream re-opens by name, so there is
/// no second resolution for anything to race.
#[derive(Debug)]
pub struct OpenAttachment {
    pub file: tokio::fs::File,
    pub size: u64,
    pub format: AttachmentFormat,
    /// Where it was found. [`bind::bind_attachments`] needs this to tell an
    /// already-bound attachment (nothing to do) from a staged one (copy it
    /// across); it is not otherwise read.
    pub location: AttachmentLocation,
}

/// `<root>/<card_id>/bound/<id>` — the path handed to codex, and the path an
/// attachment keeps for the life of the card.
///
/// Pure string work. It is not evidence that anything exists there; the only
/// thing that establishes that is [`open_attachment`], which is what
/// [`bind::bind_attachments`] runs before it records this path.
pub fn bound_file_path(root: &Path, card_id: &CardId, id: &AttachmentId) -> PathBuf {
    root.join(card_id.as_str()).join(BOUND).join(id.as_str())
}

/// The one place an [`AttachmentId`] becomes bytes.
///
/// # Why this delegates instead of checking
///
/// The obvious shape — `lstat` the name, decide, then `open` it — is wrong, and
/// #1505 review rounds 2 and 3 each caught a different way it is wrong: the
/// `open` is a second resolution, so a rename between the two decides what is
/// served; `O_NOFOLLOW` closes only the final component, leaving an
/// intermediate directory swapped for a symlink to another card's subtree; and
/// without `O_NONBLOCK` a FIFO on the path parks a blocking thread forever.
///
/// Every one of those is already answered by
/// [`crate::routes::fs::open_workspace_regular_file`], which resolves and opens
/// atomically with `openat2` under `RESOLVE_BENEATH`. So this calls it rather
/// than re-deriving its checks: the root descriptor is `<workspace>/.neige/
/// attachments`, and `<card_id>/<dir>/<id>` is resolved beneath it.
///
/// `bound/` first, then `staging/`. What answers cross-card forgery is two
/// things together, and round 3 shipped only the first: the relative path is
/// built from the card in the URL, **and** the resolution refuses every symlink
/// on that path. `RESOLVE_BENEATH` on its own is not enough — the root is
/// `attachments/`, so a sibling card is still beneath it, and a relative link
/// `card-a/staging -> ../card-b/bound` resolves and serves card B's bytes. That
/// was measured, not argued. `RESOLVE_NO_SYMLINKS` is what makes the sentence
/// true.
///
/// The opener's own errors name host paths, so they are logged and replaced
/// with one refusal that names only the attachment and the card.
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
            // The strict set. Every card is a separate trust domain and they
            // are siblings under this root, so "beneath the root" is far wider
            // than "inside this card's directory": `RESOLVE_BENEATH` alone
            // happily follows `card-a/staging -> ../card-b/bound`. Nothing in
            // this module ever creates a symlink in the subtree, so refusing
            // all of them costs nothing.
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
            // The platform cannot do a bounded, root-anchored open at all
            // (no `openat2`). That is not "this card does not have that
            // attachment", and must not be reported as one.
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
/// [`dir::regular_entries`]' `fstatat` with `AT_SYMLINK_NOFOLLOW`, which
/// describes a link rather than following it — and describes a DANGLING one
/// instead of failing on it, so that classification is available at all.
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

/// The one refusal this measurement produces.
///
/// #1515 review round 2. The host path goes to the log, never into the returned
/// message: every `CalmError` built in this module is rendered into an HTTP
/// error body, and the workspace's layout on the server's disk is not something
/// a client asked for or can act on. The same rule holds for
/// [`store::store_upload`]'s failures and for the read-back's.
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

/// Strip absolute host paths out of a stored harness-item `params` blob before
/// it is put on the wire.
///
/// # Why this exists, and what it is NOT claiming
///
/// #1505 S6 review. `GET /api/cards/{id}/harness/items` returns each row's
/// `params` verbatim, and this slice made codex put
/// `{"type":"localImage","path":"/abs/host/path"}` in there — the very item it
/// is handed. That path is not something a browser asked for, can act on, or
/// needs: the transcript renders attachments from
/// [`calm_types::model::HarnessInputSegment`], which carries an id and a REST
/// url and no path at all.
///
/// **This does not establish "no host path reaches a client".** That sentence
/// is false in this repository and was false before this slice: the same route
/// ships whatever else codex put in a notification, and a track's `cwd` is on
/// the wire by design. What #1515 established is narrower and is restated on
/// [`server_side_fault`]: no path this MODULE builds reaches an error body.
/// This function removes one path this slice would otherwise have added to a
/// different surface; it is a reduction, not an invariant.
///
/// Total over shape rather than over spelling: it walks the whole document and
/// rewrites `path` on every object whose `type` is `localImage`, wherever it
/// sits, because codex decides that nesting and we do not.
pub fn redact_local_image_paths(params: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(params) else {
        // Not JSON we can walk. It is stored opaque and goes out opaque; a
        // blob this cannot parse is also one no `localImage` item came from,
        // since we only ever store what serde produced.
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

/// REST path the browser reads an attachment back from.
///
/// Re-exported rather than re-derived: the same path has to appear in the
/// upload response, in every queued message and in every transcript segment,
/// and two of those three are built inside `calm-types`. One builder, in the
/// crate both sides can reach.
pub use calm_types::planner_attachment::attachment_url;

#[cfg(test)]
mod tests;

//! Sweeping `staging/`. Nothing here can reach `bound/`.
//!
//! # Why the sweep needs no knowledge of the queue
//!
//! Binding an attachment to a queue entry *moves* it out of `staging/` before
//! the entry is written. So a file still in `staging/` is, by construction, one
//! that no queue entry references — there is no window in which a referenced
//! file is here, and therefore no lock to take and no queue to consult. That is
//! a structural exclusion, not a probability argument about how long a bind
//! takes.
//!
//! # Fail-closed means "delete nothing", for a broken filesystem
//!
//! A sweep that cannot enumerate the directory, or hits an entry that cannot be
//! stat'd for any reason other than having vanished, deletes **nothing at all**
//! — not "skips that one and carries on". The `?`-propagating shape of
//! `collect_expired` is load-bearing there: a filesystem that will not answer
//! means the ages here are unknown, and the safe answer to an unknown age is to
//! keep the bytes.
//!
//! An entry that is simply *not one of ours* is the other case and must not
//! abort: a symlink, a socket, a subdirectory. The stat is `symlink_metadata`,
//! so a dangling link is described rather than followed, and such an entry is
//! stepped over with nothing deleted from it. Aborting on one instead would let
//! anything with write access to the workspace park a dangling link in
//! `staging/` and permanently disable the sweep for that card — which, paired
//! with the same latch in `used_bytes`, is how one planted entry used to wedge
//! both halves of the channel at once.

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::StagingDir;

/// How long an unbound upload survives. Long enough that a person who uploads
/// an image, gets distracted and comes back keeps it; short enough that a
/// browser tab closed mid-compose does not cost the card's budget forever.
pub const ORPHAN_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Remove one file from `staging/`.
///
/// The parameter is a [`StagingDir`], and there is no conversion from
/// [`super::BoundDir`] into one — `tests/ui/bound_dir_cannot_be_deleted.rs`
/// fails to compile if one is ever added. That fence proves the absence of the
/// conversion and nothing wider; see the module docs on
/// [`super`] for what it does not prove.
///
/// `remove_file` unlinks the name, never a symlink's target, so an entry
/// planted as a link to somewhere outside this subtree loses only the link.
pub fn remove_staged_file(dir: &StagingDir, file_name: &str) -> std::io::Result<()> {
    match std::fs::remove_file(dir.path().join(file_name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Delete every `staging/` entry older than [`ORPHAN_TTL`].
///
/// Never fails the caller: it runs beside an upload that already succeeded, and
/// a stale file is not a reason to refuse a fresh one. It warns and gives up
/// instead — see the module docs for why "gives up" means zero deletions.
pub fn sweep_staging(dir: &StagingDir) {
    sweep_staging_at(dir, SystemTime::now(), ORPHAN_TTL);
}

/// The testable core. Returns the names it removed.
pub fn sweep_staging_at(dir: &StagingDir, now: SystemTime, ttl: Duration) -> Vec<String> {
    match collect_expired(dir.path(), now, ttl) {
        Ok(expired) => {
            let mut removed = Vec::new();
            for name in expired {
                match remove_staged_file(dir, &name) {
                    Ok(()) => removed.push(name),
                    Err(error) => tracing::warn!(
                        target: "planner_attachments::gc",
                        file = %name,
                        %error,
                        "could not remove an expired staged attachment"
                    ),
                }
            }
            removed
        }
        Err(error) => {
            tracing::warn!(
                target: "planner_attachments::gc",
                dir = %dir.path().display(),
                %error,
                "staging sweep could not enumerate the directory; removed nothing"
            );
            Vec::new()
        }
    }
}

/// Enumerate first, delete second. Any read failure aborts the whole
/// enumeration with `Err`, so the caller deletes nothing.
fn collect_expired(dir: &Path, now: SystemTime, ttl: Duration) -> std::io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut expired = Vec::new();
    for entry in entries {
        let entry = entry?;
        let meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            // Gone already. Nothing to expire, and nothing broken.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            // Anything else is the filesystem refusing to answer: abandon the
            // whole sweep, deleting nothing.
            Err(error) => return Err(error),
        };
        // `is_file()` on a `symlink_metadata` file type is true only for a
        // regular file. A symlink, a socket or a directory is not ours to age
        // out; step over it.
        if !meta.file_type().is_file() {
            continue;
        }
        let modified = meta.modified()?;
        let age = match now.duration_since(modified) {
            Ok(age) => age,
            // Clock skew, or a file stamped in the future. Unknown age keeps
            // the bytes.
            Err(_) => continue,
        };
        if age > ttl {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                // A non-UTF-8 name cannot have been minted here. Leave it.
                continue;
            };
            expired.push(name);
        }
    }
    Ok(expired)
}

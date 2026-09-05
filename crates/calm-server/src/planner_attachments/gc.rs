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
//! # Fail-closed means "delete nothing"
//!
//! A sweep that cannot enumerate the directory, or cannot take an entry's
//! metadata, deletes **nothing at all** — not "skips that one and carries on".
//! The `?`-propagating shape of this function is load-bearing: an unreadable
//! entry means the age of the files here is unknown, and the safe answer to an
//! unknown age is to keep the bytes.

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
/// [`super::BoundDir`] into one. That is the whole enforcement of "`bound/` has
/// no deletion path": a caller holding a bound directory cannot reach this
/// function, and `tests/ui/bound_dir_cannot_be_deleted.rs` fails to compile if
/// a conversion is ever added.
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
        // `std::fs::metadata` follows symlinks where `DirEntry::metadata` would
        // not. Following is what makes an entry we cannot stat — a dangling
        // link, a vanished file, a directory we cannot descend — an `Err` that
        // abandons the whole sweep instead of a silently skipped entry beside
        // deletions that still happen.
        let meta = std::fs::metadata(entry.path())?;
        if !meta.is_file() {
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

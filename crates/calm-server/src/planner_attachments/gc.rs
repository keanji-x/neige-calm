//! Sweeping `staging/`.
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
//! # Where the "it cannot reach `bound/`" claim went
//!
//! This file used to open with "Nothing here can reach `bound/`", and that was
//! false. The sweep walked `<root>/<card>/staging` with `std::fs::read_dir` and
//! unlinked `<root>/<card>/staging/<name>` with `std::fs::remove_file` —
//! neither resolving under any `RESOLVE_*` flag. A relative link
//! (`ln -s ../card-b/bound <root>/card-a/staging`, which `RESOLVE_BENEATH`
//! would not have caught either) made every upload on card A walk into card
//! B's `bound/` and unlink everything older than [`ORPHAN_TTL`], taking card
//! B's already-issued `localImage` paths with it — and codex answers a missing
//! one with placeholder text and no error.
//!
//! There is no replacement sentence here about what this file can and cannot
//! reach. The sweep takes a [`dir::StagingFd`], every operation it performs is
//! a descriptor plus one component, and the reason that is enough is stated
//! once, in [`super::dir`], where the mechanism lives.
//!
//! # Fail-closed means "delete nothing", for a broken filesystem
//!
//! A sweep that cannot enumerate the directory deletes **nothing at all** —
//! not "skips that one and carries on". [`dir::regular_entries`] aborts with
//! `Err` on any read failure that is not "this entry vanished", and this file
//! turns that into zero deletions: a filesystem that will not answer means the
//! ages here are unknown, and the safe answer to an unknown age is to keep the
//! bytes.
//!
//! An entry that is simply *not one of ours* is the other case and must not
//! abort — a symlink, a socket, a subdirectory. `regular_entries` steps over
//! those, because aborting on one instead would let anything with write access
//! to the workspace park a dangling link in `staging/` and permanently disable
//! the sweep for that card.

use std::time::{Duration, SystemTime};

use super::dir::{self, Name, StagingFd};

/// How long an unbound upload survives. Long enough that a person who uploads
/// an image, gets distracted and comes back keeps it; short enough that a
/// browser tab closed mid-compose does not cost the card's budget forever.
pub const ORPHAN_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Delete every `staging/` entry older than [`ORPHAN_TTL`].
///
/// Never fails the caller: a stale file is not a reason to refuse a fresh
/// upload. It warns and gives up instead — see the module docs for why "gives
/// up" means zero deletions.
pub fn sweep_staging(staging: &StagingFd) {
    sweep_staging_at(staging, SystemTime::now(), ORPHAN_TTL);
}

/// The testable core. Returns the names it removed.
pub fn sweep_staging_at(staging: &StagingFd, now: SystemTime, ttl: Duration) -> Vec<Name> {
    let entries = match dir::regular_entries(staging) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                target: "planner_attachments::gc",
                %error,
                "staging sweep could not enumerate the directory; removed nothing"
            );
            return Vec::new();
        }
    };
    let mut removed = Vec::new();
    for entry in entries {
        let age = match now.duration_since(entry.modified) {
            Ok(age) => age,
            // Clock skew, or a file stamped in the future. Unknown age keeps
            // the bytes.
            Err(_) => continue,
        };
        if age <= ttl {
            continue;
        }
        match dir::unlink_staged(staging, &entry.name) {
            Ok(()) => removed.push(entry.name),
            Err(error) => tracing::warn!(
                target: "planner_attachments::gc",
                file = %entry.name,
                %error,
                "could not remove an expired staged attachment"
            ),
        }
    }
    removed
}

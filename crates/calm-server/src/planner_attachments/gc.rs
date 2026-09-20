//! Sweeping `staging/`: a bind moves an attachment out of `staging/` before its queue entry is written, so anything still here is unreferenced and needs no lock or queue lookup.
//! A sweep that cannot enumerate the directory deletes nothing at all: an unknown age keeps the bytes.

use std::time::{Duration, SystemTime};

use super::dir::{self, Name, StagingFd};

/// How long an unbound upload survives: long enough for a distracted person, short enough that a tab closed mid-compose does not cost the card's budget forever.
pub const ORPHAN_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Delete every `staging/` entry older than [`ORPHAN_TTL`]. Never fails the caller: a stale file is not a reason to refuse a fresh upload.
pub fn sweep_staging(staging: &StagingFd) {
    sweep_staging_at(staging, SystemTime::now(), ORPHAN_TTL);
}

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
            // Clock skew, or a file stamped in the future: unknown age keeps the bytes.
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

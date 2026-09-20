//! Shared process-liveness probes for the grandchild locks.
//! `alive()` means schedulable (exists and not a zombie): `kill(pid, 0) == 0` is true for a zombie, and a supervised pty leader stays a zombie this process owns for its whole registry lifetime.
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// Fields of `/proc/<pid>/stat` after `comm`, starting at field 3 (`state`); `comm` can contain spaces and parentheses, so split at the last `')'`.
fn stat_fields_after_comm(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    Some(rest.split_whitespace().map(str::to_owned).collect())
}

/// Whether `pid` is a live, non-zombie process (missing `/proc/<pid>` is dead; state `Z` is dead).
pub fn alive(pid: u32) -> bool {
    match stat_fields_after_comm(pid) {
        // Field 3 (`state`) is the first entry after comm.
        Some(fields) => fields.first().map(|s| s != "Z").unwrap_or(false),
        None => false,
    }
}

/// The process group id of `pid` (`/proc/<pid>/stat` field 5, 1-indexed):
/// after `comm` come `state`, `ppid`, `pgrp`.
pub fn process_group_of(pid: u32) -> Option<u32> {
    stat_fields_after_comm(pid)?.get(2)?.parse().ok()
}

/// Polls until `pid` is no longer a live, non-zombie process.
pub fn await_death(pid: u32, budget: Duration) -> bool {
    poll_until(budget, || !alive(pid))
}

fn poll_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

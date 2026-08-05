//! Shared process-liveness probes for the two #993 grandchild locks.
//!
//! Included by both `terminate_all_kills_grandchild.rs` and
//! `terminate_all_kills_grandchild_in_drain_grace.rs`. Each test target
//! compiles its own copy, so not every item is used by both.
//!
//! # Why not `kill(pid, 0)`
//!
//! `kill(pid, 0) == 0` is true for a **zombie**. Both tests wait for a
//! grandchild to die after a process-group SIGTERM, and that grandchild is
//! reparented when its leader goes away — so between "SIGTERM delivered, the
//! process is gone" and "the new parent (init / a subreaper) got round to
//! `wait()`ing", `kill(pid, 0)` keeps answering "alive" for reasons that have
//! nothing to do with whether `terminate_all_process_groups_sync` worked. That
//! is a pure flake source, and widening the poll budget would only make it
//! rarer, not absent.
//!
//! So `alive()` here means *schedulable*: the process exists **and** is not a
//! zombie. `/proc/<pid>` being absent is dead; state `Z` is dead.
//!
//! `awaited_reaped()` is the deliberately different predicate — see its doc.
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// Splits `/proc/<pid>/stat` into the fields *after* `comm`, i.e. starting at
/// field 3 (`state`). `comm` is the raw executable name and can contain spaces
/// and parentheses, so the only safe split point is the **last** `')'`.
fn stat_fields_after_comm(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    Some(rest.split_whitespace().map(str::to_owned).collect())
}

/// Whether `pid` is a live, non-zombie process.
///
/// A missing `/proc/<pid>` is dead; state `Z` is dead. See the module doc for
/// why the zombie case must not count as alive.
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

/// Polls until `pid` has been **reaped** — the pid released, not merely the
/// process stopped.
///
/// This is intentionally *not* `!alive()`: it is the externally observable form
/// of "our waiter thread's `child.wait()` returned", which is what the
/// drain-grace test needs in order to establish that it is standing inside the
/// post-reap drain window. A zombie leader has not been reaped yet, and
/// `kill(pid, 0)` correctly still answers "yes, that pid is mine".
pub fn await_reaped(pid: u32, budget: Duration) -> bool {
    poll_until(budget, || unsafe { libc::kill(pid as libc::pid_t, 0) != 0 })
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

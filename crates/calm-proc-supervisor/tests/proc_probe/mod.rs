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
//! # Why there is no `await_reaped()` any more (#1013 PR-B)
//!
//! This module used to export `await_reaped(pid)` = `kill(pid, 0) != 0`, i.e.
//! "the pid has been released". **Do not reintroduce it for a supervised pty
//! leader.** Under #1013 the supervisor observes the leader's exit with
//! `waitid(.., WNOWAIT)` and reaps only in `Drop for ProcEntry`, so the leader
//! is a zombie *this process owns* for the entry's whole registry lifetime:
//! `kill(zombie, 0) == 0` for all of it, and any test polling for the opposite
//! hard-fails on its deadline instead of proceeding to the behaviour it meant
//! to check. That is exactly what happened to
//! `terminate_all_kills_grandchild_in_drain_grace.rs`, whose migration to a
//! state probe (`debug_entry_stats(..).exit_observed`) shipped with the pin.
//!
//! If you need "the waiter has seen the exit", read that bit. If you need "the
//! process is over", use `await_death` / `alive` — a zombie counts as dead
//! there, which is the right answer for a *grandchild* nobody in this test is
//! pinning.
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

//! Process-stat observation shared by child cleanup and server supervision.

/// Read `starttime` (clock-ticks since boot) for `pid` from `/proc/<pid>/stat`; `None` if the process is gone, the file can't be parsed, or on non-Linux.
/// `(pid, start_time, boot_id)` is the canonical identity token: `start_time` restarts from 0 after a reboot, so `boot_id` is what makes the triple race-free across reboots.
#[cfg(target_os = "linux")]
pub fn read_proc_start_time(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_starttime_from_stat(&stat)
}

/// Pure parser for `/proc/<pid>/stat` field 22 (`starttime`); cfg-gate-free so unit tests run on every host.
pub fn parse_starttime_from_stat(content: &str) -> Option<u64> {
    parse_proc_stat_fields(content).map(|fields| fields.start_time)
}

/// The identity-relevant subset of `/proc/<pid>/stat`: `state` (field 3), `pgrp` (field 5) and `starttime` (field 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStatFields {
    /// Single-char process state (`R`/`S`/`Z`/…) — field 3.
    pub state: char,
    /// Process-group id — field 5.
    pub pgrp: i32,
    /// `starttime` in clock-ticks since boot — field 22; the same value
    /// [`read_proc_start_time`] returns and owned-process verification checks.
    pub start_time: u64,
}

/// Pure parser for the [`ProcStatFields`] subset of `/proc/<pid>/stat`.
pub fn parse_proc_stat_fields(content: &str) -> Option<ProcStatFields> {
    // `comm` may contain `)` — split on the LAST `)`; the remainder starts with `state`.
    let after = content.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    // Post-comm indices: state(0) ppid(1) pgrp(2) … starttime is index 19.
    let state = fields.next()?.chars().next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse::<i32>().ok()?;
    // `nth(16)` consumes 3..=19 and yields index 19 (`starttime`).
    let start_time = fields.nth(16)?.parse::<u64>().ok()?;
    Some(ProcStatFields {
        state,
        pgrp,
        start_time,
    })
}

/// Non-Linux stub: `/proc` identity verification is Linux-only.
#[cfg(not(target_os = "linux"))]
pub fn read_proc_start_time(_pid: i32) -> Option<u64> {
    None
}

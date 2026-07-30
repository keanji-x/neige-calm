//! Owned-process identity helpers shared by app-server supervision paths.

/// #318 INV-5 (R3-B1) — read the `starttime` field (clock-ticks since
/// boot) for `pid` from `/proc/<pid>/stat`. Returns `None` if the entry
/// doesn't exist (the process is gone), the file can't be parsed, or
/// we are running on a non-Linux target.
///
/// **Why it matters.** `(pid, start_time, boot_id)` is the canonical
/// Linux identity token for a live process across reboots:
/// `start_time` is jiffies-since-boot for the creation of THAT pid
/// (invariant within a boot), and `boot_id` (a per-boot UUID from
/// `/proc/sys/kernel/random/boot_id`) distinguishes "same boot, pid
/// recycled" from "different boot entirely". After a reboot ALL
/// `start_time` values restart from 0, so the captured stamp alone
/// could in principle coincide with a fresh post-reboot pid's stamp
/// (probability is small but nonzero, especially right after boot
/// when starttime is small). The `boot_id` companion check makes the
/// triple race-free across reboots — a different boot ⇒ skip the
/// kill regardless of pid/start_time. The triple is read at spawn,
/// persisted alongside the pgid, and verified before signaling on
/// boot recovery — see [`verify_owned_pid`].
///
/// `/proc/<pid>/stat` layout (proc(5)): space-separated fields after the
/// `comm` blob (which can contain spaces/parens and is always wrapped in
/// `(…)` — split on the **last** `)` to skip it safely). `starttime` is
/// field 22 (1-indexed); after the comm-wrap split, that's index 19 of
/// the remaining tokens (we drop the first three fields `state ppid
/// pgrp` … `state` is index 0 of the post-comm split). Concretely: pid,
/// `(comm)`, state, ppid, pgrp, session, tty_nr, tpgid, flags, minflt,
/// cminflt, majflt, cmajflt, utime, stime, cutime, cstime, priority,
/// nice, num_threads, itrealvalue, **starttime** — that's index 19 in
/// the post-comm split.
#[cfg(target_os = "linux")]
pub fn read_proc_start_time(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_starttime_from_stat(&stat)
}

/// Pure parser for `/proc/<pid>/stat` field 22 (`starttime`).
///
/// Split out from [`read_proc_start_time`] so the load-bearing
/// `rsplit_once(')')` — needed because `comm` can contain `)` (e.g.
/// `(name with paren)`, `(weird)name)`, etc.) — is exercised by unit
/// tests using synthetic stat content. Production callers go through
/// [`read_proc_start_time`] which reads the file + delegates here;
/// tests can feed arbitrary strings without spawning processes whose
/// `comm` they don't control.
///
/// The cross-platform stub above this in non-Linux builds doesn't need
/// this helper (it returns `None` unconditionally), but the parser is
/// cfg-gate-free so unit tests run on every host.
pub fn parse_starttime_from_stat(content: &str) -> Option<u64> {
    parse_proc_stat_fields(content).map(|fields| fields.start_time)
}

/// #954 review r2 D1 — the identity-relevant subset of
/// `/proc/<pid>/stat`: `state` (field 3), `pgrp` (field 5) and
/// `starttime` (field 22). Shares the load-bearing comm-wrap handling
/// with [`parse_starttime_from_stat`] (which now delegates here): `comm`
/// can contain `)`, so split on the LAST `)` and index the remainder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStatFields {
    /// Single-char process state (`R`/`S`/`Z`/…) — field 3.
    pub state: char,
    /// Process-group id — field 5.
    pub pgrp: i32,
    /// `starttime` in clock-ticks since boot — field 22; the same value
    /// [`read_proc_start_time`] returns and [`verify_owned_pid`] checks.
    pub start_time: u64,
}

/// Pure parser for the [`ProcStatFields`] subset of `/proc/<pid>/stat`.
pub fn parse_proc_stat_fields(content: &str) -> Option<ProcStatFields> {
    // `comm` may contain `)` — strip everything up to and including the
    // LAST `)`. The remainder starts with the `state` field.
    let after = content.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    // Post-comm indices: state(0) ppid(1) pgrp(2) session(3) tty_nr(4)
    // tpgid(5) flags(6) minflt(7) cminflt(8) majflt(9) cmajflt(10)
    // utime(11) stime(12) cutime(13) cstime(14) priority(15) nice(16)
    // num_threads(17) itrealvalue(18) → starttime is index 19.
    let state = fields.next()?.chars().next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse::<i32>().ok()?;
    // Indices 0..=2 consumed above; `nth(16)` consumes 3..=19 and yields
    // index 19 (`starttime`).
    let start_time = fields.nth(16)?.parse::<u64>().ok()?;
    Some(ProcStatFields {
        state,
        pgrp,
        start_time,
    })
}

/// Non-Linux stub. Identity verification via `/proc` is Linux-specific;
/// on macOS / BSD the file does not exist. The kernel only spawns
/// `codex app-server` on Linux production hosts (the boot-recovery path
/// is Linux-only by design), but cross-platform builds still need this
/// to compile.
#[cfg(not(target_os = "linux"))]
pub fn read_proc_start_time(_pid: i32) -> Option<u64> {
    None
}

/// #318 INV-5 (R3-B1) — read the kernel's per-boot UUID
/// (`/proc/sys/kernel/random/boot_id`). The kernel generates this once
/// at boot and it survives in `/proc` for the lifetime of the running
/// kernel; every reboot rerolls it. Returns `None` on a non-Linux
/// target or a read failure (treated by [`verify_owned_pid`] as
/// "can't prove identity → skip the kill").
///
/// The value is a 36-char canonical UUID + trailing newline; we strip
/// the newline and store the canonical form on the spec card payload.
/// Equality is byte-for-byte (no UUID parsing required — both writer
/// and reader are this same fn, and the kernel never changes the
/// format mid-boot).
#[cfg(target_os = "linux")]
pub fn read_boot_id() -> Option<String> {
    let raw = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Non-Linux stub for [`read_boot_id`].
/// Acceptable for the current Linux deployment target; off-Linux cross-boot reclaim is a no-op.
#[cfg(not(target_os = "linux"))]
pub fn read_boot_id() -> Option<String> {
    None
}

/// #318 INV-5 (R3-B1) — verify that the live process at `pid` is the
/// SAME process whose `(start_time, boot_id)` triple we captured at
/// spawn.
///
/// Returns `true` iff ALL of:
///   * the current `/proc/sys/kernel/random/boot_id` matches
///     `expected_boot_id` (i.e. no reboot since spawn — without this,
///     a coincidentally-equal cross-boot `start_time` would slip
///     through),
///   * `/proc/<pid>/stat` exists,
///   * its `starttime` (field 22) matches `expected_start_time`.
///
/// Returns `false` otherwise. The cross-reboot case is short-circuited
/// before the `/proc/<pid>/stat` read — a `boot_id` mismatch means
/// every pid in the prior boot is gone, regardless of stamp.
///
/// **Why we need this on top of
/// [`crate::spec_appserver::socket_owned_by_appserver`].** The socket
/// probe (`UnixStream::connect` succeeds → trust the pgid) is a good
/// cheap proxy but suffers a TOCTOU window between the probe and the
/// subsequent `signal_process_group(pgid, …)`. Between those two
/// syscalls the kernel can reap the listener, recycle its pid/pgid to
/// an unrelated user process, and our SIGTERM/SIGKILL then lands on
/// that innocent process. `(pid, start_time, boot_id)` is race-free
/// identity:
///
///   * Cross-reboot pid recycle: `boot_id` mismatch ⇒ reject.
///   * Same-boot pid recycle: the recycled process has a strictly
///     later `start_time` (it started AFTER our stamp), so the
///     stamp comparison rejects.
///   * Liveness-only mismatch (we crashed before persisting →
///     `/proc/<pid>` is gone): the `read_proc_start_time` ENOENT
///     short-circuits to `None` ⇒ reject.
///
/// On a non-Linux target (no `/proc`) this returns `false`
/// unconditionally — the caller's fallback (skip the kill, cleanup the
/// stale socket, let the respawn rebind) is correct in that environment.
pub fn verify_owned_pid(pid: i32, expected_start_time: u64, expected_boot_id: &str) -> bool {
    // Reboot check FIRST — cheapest, and short-circuits the post-reboot
    // case (the entire prior boot's pid namespace is dead, regardless
    // of any individual pid's stamp).
    let Some(live_boot) = read_boot_id() else {
        return false;
    };
    if live_boot != expected_boot_id {
        return false;
    }
    let Some(live) = read_proc_start_time(pid) else {
        return false;
    };
    live == expected_start_time
}

/// Send `signal` to the owned process **group** `pgid` (`kill(-pgid, signal)`).
///
/// This is the load-bearing helper for owned process-group reap paths. Callers
/// persist a process group id for a child they spawned; one group signal reaches
/// the group leader and descendants that share that `pgid`. Best-effort: a
/// non-positive `pgid` (never expected — the child is always a real positive
/// pid) is refused so we can't accidentally signal our own group or every
/// process; `ESRCH` (group already gone) is swallowed.
///
/// Returns `true` if the signal was delivered to at least one process,
/// `false` on `ESRCH`/refused.
pub fn signal_process_group(pgid: i32, signal: libc::c_int) -> bool {
    if pgid <= 1 {
        // Guard against persistence corruption / a 0 pgid: kill(-1, …)
        // would signal every process we can reach, kill(0, …)/kill(-0, …)
        // would hit our own group. Never legitimate for a spawned child.
        tracing::warn!(
            pgid,
            "spec push: refusing to signal non-positive process group"
        );
        return false;
    }
    // SAFETY: `kill(2)` with a negative pid targets the process group
    // `pgid`. No memory is shared; the call is async-signal-safe.
    let rc = unsafe { libc::kill(-pgid, signal) };
    if rc == 0 {
        true
    } else {
        let err = std::io::Error::last_os_error();
        // ESRCH (no such process group) is the expected terminal state.
        tracing::debug!(pgid, signal, error = %err, "spec push: kill(-pgid) returned error (likely already gone)");
        false
    }
}

/// #954 review r2 D1 — one member of process group `pgid` observed by
/// [`scan_process_group_members`], with the identity stamp captured AT
/// scan time. `boot_id` is deliberately not part of the stamp: the scan
/// and the subsequent verify-then-signal happen within one live process
/// (nothing is persisted across reboots), so `(pid, start_time)` is a
/// complete same-boot identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupMember {
    pub pid: i32,
    /// `starttime` captured at scan; re-verified before any signal.
    pub start_time: u64,
    /// `/proc/<pid>/stat` state was `Z` at scan: already dead, only
    /// awaiting its parent's `wait()` — never signaled by the sweep.
    pub is_zombie: bool,
}

/// #954 review r2 D1 — enumerate the CURRENT members of process group
/// `pgid` by scanning `/proc/*/stat` for `pgrp == pgid`. Snapshot
/// semantics: a process that forks into the group after the scan is not
/// seen (recorded residual — see `terminate_group_with_grace`), and any
/// member may die between the scan and whatever the caller does next,
/// which is why each entry carries its scan-time `start_time` for a
/// verify-then-signal re-check. Refuses `pgid <= 1` for the same reason
/// as [`signal_process_group`].
#[cfg(target_os = "linux")]
pub fn scan_process_group_members(pgid: i32) -> Vec<GroupMember> {
    let mut members = Vec::new();
    if pgid <= 1 {
        return members;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return members;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        // The entry can vanish between readdir and this read; skip.
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(fields) = parse_proc_stat_fields(&stat) else {
            continue;
        };
        if fields.pgrp != pgid {
            continue;
        }
        members.push(GroupMember {
            pid,
            start_time: fields.start_time,
            is_zombie: fields.state == 'Z',
        });
    }
    members
}

/// Non-Linux stub for [`scan_process_group_members`] — no `/proc`, no
/// enumerable members (callers then have nothing to signal, which is the
/// fail-closed posture).
#[cfg(not(target_os = "linux"))]
pub fn scan_process_group_members(_pgid: i32) -> Vec<GroupMember> {
    Vec::new()
}

/// Result of a [`sigkill_verified_group_members`] sweep, for logging and
/// tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GroupSweepOutcome {
    /// Members whose scan-time `start_time` re-verified immediately
    /// before an individually delivered SIGKILL that returned 0.
    pub killed: Vec<i32>,
    /// Members observed as zombies at scan: already dead, awaiting their
    /// parent's `wait()`; signaling them is useless and they cannot be
    /// distinguished from an imminently-reaped (recyclable) pid, so the
    /// sweep never touches them.
    pub skipped_zombies: Vec<i32>,
    /// Members whose re-verify failed (gone, or the pid was recycled to
    /// a process with a different `start_time`) or whose `kill(2)`
    /// errored — never signaled successfully, deliberately left alone.
    pub verify_failed: Vec<i32>,
}

/// #954 review r2 D1 — SIGKILL the members of process group `pgid`
/// INDIVIDUALLY, each under a verify-then-signal identity check, instead
/// of one group-wide `kill(-pgid, SIGKILL)`.
///
/// This is the only safe straggler cleanup once the group LEADER has been
/// observed dead (zombie or fully reaped) on a path that does not own the
/// leader's `Child` handle: an external parent can reap the zombie at any
/// moment, and once the last member is gone the kernel may recycle the
/// numeric pgid — so a group-wide signal races against recycling no
/// matter how recently the leader was observed. Per-pid
/// capture-then-re-verify closes that race with the SAME posture the
/// codebase already accepts everywhere for single-pid signals
/// ([`verify_owned_pid`] immediately before `kill`, e.g. the boot-reclaim
/// and reconciliation paths): a recycled pid has a strictly later
/// `start_time` than the scan-time stamp, so the re-verify rejects it and
/// no signal is sent.
pub fn sigkill_verified_group_members(pgid: i32) -> GroupSweepOutcome {
    sigkill_verified_members(&scan_process_group_members(pgid))
}

/// Kill phase of [`sigkill_verified_group_members`], split out so tests
/// can drive it with a fabricated member list (e.g. a deliberately
/// mismatched `start_time` to pin the reject-on-recycle guard).
pub fn sigkill_verified_members(members: &[GroupMember]) -> GroupSweepOutcome {
    let mut outcome = GroupSweepOutcome::default();
    let self_pid = std::process::id() as i32;
    for member in members {
        if member.pid <= 1 || member.pid == self_pid {
            // Defensive: never signal init or ourselves, whatever the
            // scan claimed.
            continue;
        }
        if member.is_zombie {
            outcome.skipped_zombies.push(member.pid);
            continue;
        }
        // Verify-then-signal: re-read `starttime` immediately before the
        // kill; a mismatch (or a vanished entry) means the scanned member
        // is gone and the pid may belong to an unrelated process.
        if read_proc_start_time(member.pid) != Some(member.start_time) {
            outcome.verify_failed.push(member.pid);
            continue;
        }
        // SAFETY: `kill(2)` on a positive, identity-re-verified pid; no
        // memory is shared and the call is async-signal-safe. The
        // verify→kill window is the same accepted ε as every
        // `verify_owned_pid`-then-`kill` site in this codebase.
        let rc = unsafe { libc::kill(member.pid, libc::SIGKILL) };
        if rc == 0 {
            outcome.killed.push(member.pid);
        } else {
            outcome.verify_failed.push(member.pid);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The comm-wrap split must survive `)` inside `comm`, and the field
    /// indices must line up with proc(5).
    #[test]
    fn parse_proc_stat_fields_handles_paren_comm() {
        // pid (comm) state ppid pgrp session tty tpgid flags minflt
        // cminflt majflt cmajflt utime stime cutime cstime priority nice
        // num_threads itrealvalue starttime …
        let stat =
            "1234 (weird) name)) Z 1 4321 4321 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 5555 0 0";
        let fields = parse_proc_stat_fields(stat).expect("parse");
        assert_eq!(
            fields,
            ProcStatFields {
                state: 'Z',
                pgrp: 4321,
                start_time: 5555,
            }
        );
        assert_eq!(parse_starttime_from_stat(stat), Some(5555));
        assert_eq!(parse_proc_stat_fields("garbage"), None);
    }

    /// #954 review r2 D1 — the reject-on-recycle guard: a member whose
    /// scan-time `start_time` no longer matches the live process must NOT
    /// be signaled. This is the deterministic, constructible analog of
    /// the recycled-pgid hazard (an actual kernel pid/pgid recycle cannot
    /// be forced in a test): a recycled pid presents exactly as a live
    /// process with a different `start_time`.
    #[cfg(target_os = "linux")]
    #[test]
    fn sweep_rejects_member_with_mismatched_start_time() {
        let mut child = std::process::Command::new("sleep")
            .arg("300")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = i32::try_from(child.id()).expect("pid fits i32");
        let live = read_proc_start_time(pid).expect("live start_time");
        let outcome = sigkill_verified_members(&[GroupMember {
            pid,
            start_time: live + 1,
            is_zombie: false,
        }]);
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        // Cleanup before asserting.
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(outcome.verify_failed, vec![pid]);
        assert!(outcome.killed.is_empty());
        assert!(
            alive,
            "a start_time-mismatched member must never be signaled"
        );
    }

    /// #954 review r2 D1 — end-to-end sweep on a real group whose leader
    /// is a zombie held unreaped by THIS test: the zombie leader is
    /// skipped (already dead, unsafe to touch) while the live
    /// TERM-ignoring member is identity-verified and SIGKILLed
    /// individually.
    #[cfg(target_os = "linux")]
    #[test]
    fn sweep_kills_live_member_and_skips_zombie_leader() {
        use std::os::unix::process::CommandExt;
        // The leader becomes its own process-group leader, spawns a
        // TERM-ignoring survivor inside the group, prints its pid, and
        // exits. We (the parent) do NOT wait() yet, so the leader stays a
        // zombie pinning nothing but its own stat entry.
        let mut leader = std::process::Command::new("sh")
            .arg("-c")
            // The survivor's stdout must NOT inherit the pipe, or the
            // parent's read_to_string below would block until the
            // survivor dies instead of until the leader exits.
            .arg(r#"( trap '' TERM; sleep 300 ) >/dev/null 2>&1 & echo $!"#)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn group leader");
        let leader_pid = i32::try_from(leader.id()).expect("pid fits i32");
        let mut out = String::new();
        use std::io::Read as _;
        leader
            .stdout
            .take()
            .expect("piped stdout")
            .read_to_string(&mut out)
            .expect("read survivor pid");
        let survivor_pid = out.trim().parse::<i32>().expect("survivor pid int");
        // Leader exits after echo; poll until its stat shows Z (we hold
        // the zombie — std reaps only on wait()).
        let mut leader_zombie = false;
        for _ in 0..100 {
            let stat = std::fs::read_to_string(format!("/proc/{leader_pid}/stat"))
                .expect("leader stat (zombie held by us)");
            if parse_proc_stat_fields(&stat).map(|f| f.state) == Some('Z') {
                leader_zombie = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(leader_zombie, "leader must become a held zombie");

        let outcome = sigkill_verified_group_members(leader_pid);

        // Survivor (and any inner sleep) must be gone/dying; the zombie
        // leader must be untouched and still present.
        let mut survivor_gone = false;
        for _ in 0..100 {
            // ESRCH, or a zombie awaiting init's reap, both count as dead.
            let alive = unsafe { libc::kill(survivor_pid, 0) } == 0
                && std::fs::read_to_string(format!("/proc/{survivor_pid}/stat"))
                    .ok()
                    .and_then(|stat| parse_proc_stat_fields(&stat))
                    .is_some_and(|f| f.state != 'Z');
            if !alive {
                survivor_gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let leader_still_zombie = std::fs::read_to_string(format!("/proc/{leader_pid}/stat"))
            .ok()
            .and_then(|stat| parse_proc_stat_fields(&stat))
            .map(|f| f.state)
            == Some('Z');
        // Cleanup: reap the held zombie; belt-kill any leftovers.
        let _ = leader.wait();
        unsafe {
            libc::kill(-leader_pid, libc::SIGKILL);
        }
        assert!(
            outcome.skipped_zombies.contains(&leader_pid),
            "zombie leader must be skipped, got {outcome:?}"
        );
        assert!(
            outcome.killed.contains(&survivor_pid),
            "live survivor must be individually killed, got {outcome:?}"
        );
        assert!(survivor_gone, "survivor must be dead after the sweep");
        assert!(
            leader_still_zombie,
            "the sweep must not have reaped or signaled the held zombie"
        );
    }
}

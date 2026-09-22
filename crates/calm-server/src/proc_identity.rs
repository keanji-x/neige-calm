//! Owned-process identity helpers shared by app-server supervision paths.

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
    /// [`read_proc_start_time`] returns and [`verify_owned_pid`] checks.
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

/// Read the kernel's per-boot UUID (`/proc/sys/kernel/random/boot_id`); every reboot rerolls it. `None` on non-Linux or a read failure, which callers treat as 'can't prove identity → skip the kill'.
#[cfg(target_os = "linux")]
pub fn read_boot_id() -> Option<String> {
    let raw = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Non-Linux stub for [`read_boot_id`]; off-Linux cross-boot reclaim is a no-op.
#[cfg(not(target_os = "linux"))]
pub fn read_boot_id() -> Option<String> {
    None
}

/// Verify that the live process at `pid` is the SAME process whose `(start_time, boot_id)` we captured at spawn: no reboot since, `/proc/<pid>/stat` exists, and its `starttime` matches.
/// Needed on top of the socket probe because of the TOCTOU between probe and signal: a recycled pid has a strictly later `start_time`, and a cross-reboot recycle has a different `boot_id`.
/// Returns `false` unconditionally on non-Linux.
pub fn verify_owned_pid(pid: i32, expected_start_time: u64, expected_boot_id: &str) -> bool {
    // Reboot check FIRST: cheapest, and the whole prior boot's pid namespace is dead regardless of stamp.
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

/// Send `signal` to the owned process **group** `pgid` (`kill(-pgid, signal)`). A non-positive `pgid` is refused; `ESRCH` is swallowed.
/// Returns `true` if the signal was delivered to at least one process.
pub fn signal_process_group(pgid: i32, signal: libc::c_int) -> bool {
    if pgid <= 1 {
        // kill(-1, …) would signal every process we can reach; kill(0, …) would hit our own group.
        tracing::warn!(
            pgid,
            "planner push: refusing to signal non-positive process group"
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
        tracing::debug!(pgid, signal, error = %err, "planner push: kill(-pgid) returned error (likely already gone)");
        false
    }
}

/// One member of process group `pgid` with the identity stamp captured AT scan time. No `boot_id`: scan and signal happen within one live process, so `(pid, start_time)` is a complete same-boot identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupMember {
    pub pid: i32,
    /// `starttime` captured at scan; re-verified before any signal.
    pub start_time: u64,
    /// State was `Z` at scan: already dead, only awaiting its parent's `wait()` — never signaled.
    pub is_zombie: bool,
}

/// Enumerate the CURRENT members of process group `pgid` by scanning `/proc/*/stat`. Snapshot semantics: a process that forks into the group after the scan is not seen, and any member may die before the caller acts, hence the per-entry `start_time` for a verify-then-signal re-check.
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

/// Non-Linux stub: no `/proc`, so nothing to signal (fail-closed).
#[cfg(not(target_os = "linux"))]
pub fn scan_process_group_members(_pgid: i32) -> Vec<GroupMember> {
    Vec::new()
}

/// Three-value classification of `/proc/<pid>/environ` against an exact `key=value` marker. The
/// environ is NUL-separated and readable by the same uid; a process started from an empty
/// environment that never `unset`s the marker keeps it, and every descendant inherits it. This is
/// what tells a recovered gate's own descendants (across a kernel restart) apart from an unrelated
/// process that recycled the numeric pgid once the wrapper's leader died — but "cannot read the
/// environ" must NOT be collapsed into "proven not ours": a live same-uid descendant can make its
/// own environ unreadable (`PR_SET_DUMPABLE=0` → EACCES), and dropping it from both the wait and
/// the kill would let it keep mutating the checkout while the gate reports clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerAuth {
    /// Environ readable and carrying the exact marker: a proven descendant. Killed by the sweep;
    /// blocks the wait.
    Present,
    /// Environ readable WITHOUT the marker (a recycled-pgid stranger), or the process is already
    /// gone (`ENOENT`): not ours and not live — never killed, never blocks the wait.
    Foreign,
    /// Environ unreadable while the process is live (`EACCES`, …): cleanup-uncertain, NOT
    /// proven-foreign. Never killed (identity unproven), but blocks the wait (fail closed →
    /// `gate-infra`) so a hidden-environ descendant cannot slip a clean after-sample past the gate.
    Unreadable,
}

#[cfg(target_os = "linux")]
pub fn proc_env_marker(pid: i32, key: &str, value: &str) -> MarkerAuth {
    match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(bytes) => {
            let needle = format!("{key}={value}");
            if bytes
                .split(|&b| b == 0)
                .any(|entry| entry == needle.as_bytes())
            {
                MarkerAuth::Present
            } else {
                MarkerAuth::Foreign
            }
        }
        // The process left between the scan and this read: not a live member to wait for and not a
        // live process to kill — fold into `Foreign`.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => MarkerAuth::Foreign,
        // Live but unreadable (EACCES from `PR_SET_DUMPABLE=0`, etc.): membership cannot be proven
        // OR disproven — cleanup-uncertain.
        Err(_) => MarkerAuth::Unreadable,
    }
}

/// Non-Linux stub for [`proc_env_marker`]: no `/proc`, so nothing is authenticated (`Foreign`).
#[cfg(not(target_os = "linux"))]
pub fn proc_env_marker(_pid: i32, _key: &str, _value: &str) -> MarkerAuth {
    MarkerAuth::Foreign
}

/// Does `/proc/<pid>/environ` carry the exact `key=value` entry (i.e. [`MarkerAuth::Present`])? This
/// is the KILL-path predicate: [`group_members_with_env_marker`] → [`sigkill_verified_members`]
/// signals ONLY proven members, never a `Foreign` (recycled pgid) or `Unreadable` (dumpable-off /
/// env-cleared) process. The wait path uses [`proc_env_marker`] directly so it can fail closed on
/// `Unreadable`. Returns `false` on non-Linux and for every non-`Present` classification.
pub fn proc_env_contains(pid: i32, key: &str, value: &str) -> bool {
    matches!(proc_env_marker(pid, key, value), MarkerAuth::Present)
}

/// The members of process group `pgid` whose `/proc/<pid>/environ` carries `key=value` — the gate's
/// own descendants, told apart from an unrelated process that recycled the numeric pgid after the
/// wrapper's leader died. Members without the marker (foreign, or environ unreadable) are dropped,
/// so a caller sweeping or waiting on this list never signals nor blocks on a foreign process.
pub fn group_members_with_env_marker(pgid: i32, key: &str, value: &str) -> Vec<GroupMember> {
    scan_process_group_members(pgid)
        .into_iter()
        .filter(|member| proc_env_contains(member.pid, key, value))
        .collect()
}

/// Result of a [`sigkill_verified_group_members`] sweep.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GroupSweepOutcome {
    /// Members re-verified immediately before an individually delivered SIGKILL that returned 0.
    pub killed: Vec<i32>,
    /// Zombies at scan: signaling them is useless and they cannot be told from an imminently-reaped (recyclable) pid, so the sweep never touches them.
    pub skipped_zombies: Vec<i32>,
    /// Members whose re-verify failed (gone, or pid recycled) or whose `kill(2)` errored; deliberately left alone.
    pub verify_failed: Vec<i32>,
}

/// SIGKILL the members of process group `pgid` INDIVIDUALLY, each under a verify-then-signal identity check, instead of one `kill(-pgid, SIGKILL)`.
/// Once the leader is dead and reapable the kernel may recycle the numeric pgid, so a group-wide signal races against recycling; a recycled pid has a strictly later `start_time`, so the per-pid re-verify rejects it.
pub fn sigkill_verified_group_members(pgid: i32) -> GroupSweepOutcome {
    sigkill_verified_members(&scan_process_group_members(pgid))
}

/// Kill phase, split out so tests can drive it with a fabricated member list.
pub fn sigkill_verified_members(members: &[GroupMember]) -> GroupSweepOutcome {
    let mut outcome = GroupSweepOutcome::default();
    let self_pid = std::process::id() as i32;
    for member in members {
        if member.pid <= 1 || member.pid == self_pid {
            // Never signal init or ourselves, whatever the scan claimed.
            continue;
        }
        if member.is_zombie {
            outcome.skipped_zombies.push(member.pid);
            continue;
        }
        // Verify-then-signal: a mismatch or a vanished entry means the pid may belong to an unrelated process.
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

    #[test]
    fn parse_proc_stat_fields_handles_paren_comm() {
        // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt utime stime cutime cstime priority nice num_threads itrealvalue starttime …
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

    /// A recycled pid presents exactly as a live process with a different `start_time`; an actual kernel pid recycle cannot be forced in a test.
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
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(outcome.verify_failed, vec![pid]);
        assert!(outcome.killed.is_empty());
        assert!(
            alive,
            "a start_time-mismatched member must never be signaled"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sweep_kills_live_member_and_skips_zombie_leader() {
        use std::os::unix::process::CommandExt;
        // The leader becomes its own group leader, spawns a TERM-ignoring survivor, prints its pid, and exits; we do NOT wait() yet so it stays a zombie.
        let mut leader = std::process::Command::new("sh")
            .arg("-c")
            // The survivor's stdout must NOT inherit the pipe, or `read_to_string` below blocks until the survivor dies.
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

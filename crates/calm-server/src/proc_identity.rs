//! Owned-process identity helpers shared by app-server supervision paths.

pub use plugin::proc_identity::{
    ProcStatFields, parse_proc_stat_fields, parse_starttime_from_stat, read_proc_start_time,
};

/// Did a `/proc/<pid>/…` read fail only because the process is gone? A process reaped after the
/// `/proc` listing fails the path lookup with `ENOENT`, but one reaped between the open and the
/// read of its file (or while its directory is held) fails with `ESRCH`. Either way it is not a
/// live process, so a scan skips it rather than failing.
pub(crate) fn proc_entry_vanished(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

/// Read Linux's per-boot UUID from `/proc/sys/kernel/random/boot_id`.
/// A read failure cannot prove identity, so callers skip the kill.
#[cfg(target_os = "linux")]
pub fn read_boot_id() -> Option<String> {
    let raw = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Read macOS's per-boot UUID from `kern.bootsessionuuid`.
/// Require a complete, nonempty NUL-terminated UUID; access denied means unknown.
#[cfg(target_os = "macos")]
pub fn read_boot_id() -> Option<String> {
    let mut len = 0;
    // SAFETY: a null output buffer queries the required length; newp=null and
    // newlen=0 make this read-only. The name is a static NUL-terminated string.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len <= 1 {
        return None;
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).ok()?;
    bytes.resize(len, 0u8);
    // SAFETY: bytes is writable for len bytes, and len points to the capacity
    // passed to the kernel. This call also supplies no new value to write.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != bytes.len() {
        return None;
    }
    let boot = std::ffi::CStr::from_bytes_with_nul(&bytes)
        .ok()?
        .to_str()
        .ok()?;
    uuid::Uuid::parse_str(boot).ok()?;
    Some(boot.to_owned())
}

/// Unsupported platforms cannot prove cross-boot process identity.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn read_boot_id() -> Option<String> {
    None
}

/// Verify the platform-native `(start_time, boot_id)` captured at spawn.
/// A different start identifier rejects a recycled pid; a different boot UUID
/// rejects cross-reboot reuse. Read failures and unsupported platforms fail closed.
/// The caller's verify-then-signal window still exists; a socket probe alone
/// cannot establish this identity either.
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

#[cfg(unix)]
pub use plugin::child_process::signal_process_group;

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
    /// Environ readable, NON-empty and WITHOUT the marker (a recycled-pgid stranger), or the process
    /// is already gone (`ENOENT`, or `ESRCH` once reaped): not ours and not live — never killed,
    /// never blocks the wait.
    Foreign,
    /// Environ unreadable while the process is live (`EACCES`, …), or read as zero bytes — what a
    /// live process shows inside execve before its new environ is set up (#1805): no environ image
    /// proves membership either way, so cleanup-uncertain, NOT proven-foreign. Never killed
    /// (identity unproven), but blocks the wait (fail closed → `gate-infra`) so a hidden-environ or
    /// mid-exec descendant cannot slip a clean after-sample past the gate. A recycled-pgid stranger
    /// whose environ is truly empty therefore also blocks the wait.
    Unreadable,
}

#[cfg(target_os = "linux")]
pub fn proc_env_marker(pid: i32, key: &str, value: &str) -> MarkerAuth {
    proc_env_marker_in(std::path::Path::new("/proc"), pid, key, value)
}

/// [`proc_env_marker`] against `proc_root` (`/proc` in production).
#[cfg(target_os = "linux")]
fn proc_env_marker_in(proc_root: &std::path::Path, pid: i32, key: &str, value: &str) -> MarkerAuth {
    match std::fs::read(proc_root.join(pid.to_string()).join("environ")) {
        // Zero bytes is indistinguishable from a task inside execve, which has no environ image yet
        // (#1805): membership cannot be proven OR disproven — cleanup-uncertain.
        Ok(bytes) if bytes.is_empty() => MarkerAuth::Unreadable,
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
        // The process left between the scan and this read (`ENOENT`, or `ESRCH` once it is reaped
        // mid-open): not a live member to wait for and not a live process to kill — `Foreign`.
        Err(error) if proc_entry_vanished(&error) => MarkerAuth::Foreign,
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn boot_id_is_stable_nonempty_uuid() {
        let boot = read_boot_id().expect("kernel boot id");
        uuid::Uuid::parse_str(&boot).expect("boot id is a UUID");
        assert_eq!(read_boot_id(), Some(boot));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn verify_owned_pid_rejects_wrong_start_time() {
        let pid = std::process::id() as i32;
        let start = read_proc_start_time(pid).expect("own start time");
        let boot = read_boot_id().expect("kernel boot id");
        assert!(verify_owned_pid(pid, start, &boot));
        assert!(!verify_owned_pid(pid, start ^ 1, &boot));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn verify_owned_pid_rejects_wrong_boot_id() {
        let pid = std::process::id() as i32;
        let start = read_proc_start_time(pid).expect("own start time");
        let boot = read_boot_id().expect("kernel boot id");
        assert!(verify_owned_pid(pid, start, &boot));
        assert!(!verify_owned_pid(pid, start, &format!("{boot}-stale")));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn verify_owned_pid_rejects_invalid_pids() {
        let boot = read_boot_id().expect("kernel boot id");
        for pid in [i32::MIN, -1, 0, i32::MAX] {
            assert!(!verify_owned_pid(pid, 1, &boot), "pid {pid}");
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn verify_owned_pid_accepts_live_child_and_rejects_reaped_child() {
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let status = self.0.wait().expect("reap identity test child");
                eprintln!("identity test child {} reaped: {status}", self.0.id());
            }
        }
        let mut child = ChildGuard(
            std::process::Command::new("sleep")
                .arg("60")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn identity test child"),
        );
        let pid = i32::try_from(child.0.id()).expect("pid fits i32");
        let start = read_proc_start_time(pid).expect("live child start time");
        let boot = read_boot_id().expect("kernel boot id");
        assert_ne!(start, 0);
        assert_eq!(read_proc_start_time(pid), Some(start));
        assert!(verify_owned_pid(pid, start, &boot));
        child.0.kill().expect("kill owned test child");
        child.0.wait().expect("reap owned test child");
        assert!(!verify_owned_pid(pid, start, &boot));
    }

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

    /// A process reaped between the group scan and the read of its environ fails that read with
    /// ESRCH (#1793): it is gone, so `Foreign` (never killed, never waited for), not `Unreadable`
    /// (which only a LIVE hidden-environ member may be). Pinned with a fake proc root whose entry
    /// resolves, through `/proc/self/fd`, to the held `/proc/<pid>` directory of a reaped child.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_reaped_process_environ_is_foreign_not_unreadable() {
        let reaped = crate::test_support::proc::ReapedProcDir::new();
        let pid = reaped.pid;
        let root = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(reaped.path(), root.path().join(pid.to_string()))
            .expect("symlink");

        let read = std::fs::read(root.path().join(format!("{pid}/environ")));
        assert_eq!(
            read.as_ref().map_err(std::io::Error::raw_os_error).err(),
            Some(Some(libc::ESRCH)),
            "the fixture must reproduce the reaped-mid-scan read: {read:?}"
        );
        assert_eq!(
            proc_env_marker_in(root.path(), pid, "NEIGE_GATE_OP", "w:reaped#g1"),
            MarkerAuth::Foreign
        );
    }

    /// An environ that reads zero bytes is what a live process shows while it is inside execve
    /// (#1805): no environ image proves membership either way, so it is `Unreadable`, not
    /// `Foreign`. An environ with entries but without the marker stays `Foreign`.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_empty_environ_is_unreadable_and_a_markerless_one_is_foreign() {
        let root = tempfile::tempdir().expect("tempdir");
        for (pid, environ) in [(4101, &b""[..]), (4102, &b"PATH=/bin\0HOME=/h\0"[..])] {
            std::fs::create_dir(root.path().join(pid.to_string())).expect("pid dir");
            std::fs::write(root.path().join(format!("{pid}/environ")), environ).expect("environ");
        }
        assert_eq!(
            proc_env_marker_in(root.path(), 4101, "NEIGE_GATE_OP", "w:empty#g1"),
            MarkerAuth::Unreadable
        );
        assert_eq!(
            proc_env_marker_in(root.path(), 4102, "NEIGE_GATE_OP", "w:empty#g1"),
            MarkerAuth::Foreign
        );
    }
}

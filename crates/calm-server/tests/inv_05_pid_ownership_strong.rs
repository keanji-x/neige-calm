//! # INV-5 — pid ownership must be stronger than `kill(pid, 0)` and stronger than a TOCTOU socket probe
//!
//! **Bug**: R3-B1 (from #318)
//! **Encoded contract**: before signaling a persisted pgid as "ours",
//! the kernel must hold evidence that pins the *identity* of the
//! process at that pgid (not just its liveness). The canonical Linux
//! mechanism is the `(pid, start_time)` pair: `start_time` (the
//! `starttime` field at offset 22 in `/proc/<pid>/stat`,
//! clock-ticks-since-boot at process creation) is captured at spawn,
//! persisted alongside the pgid, and verified against the live
//! `/proc` entry immediately before the kill. A mismatch (or `/proc`
//! entry gone) → skip the kill; the original process is dead and the
//! pgid has been recycled (or reclaimed by the kernel).
//!
//! Background: the create-wave / takeover path used to gate
//! `signal_process_group(pgid, …)` on
//! [`spec_appserver::socket_owned_by_appserver`] — a
//! `UnixStream::connect` to the per-card socket path. That's better
//! than a bare `kill(pid, 0)` liveness probe but TOCTOU-racy against
//! the subsequent ~400 ms SIGTERM → grace → SIGKILL sequence: the
//! original listener could exit, its pid be recycled, and our signal
//! land on an innocent process.
//!
//! `(pid, start_time)` closes the window: even if the pgid is
//! recycled between probe and kill, the recycled process has a
//! strictly LATER `starttime` (it was created AFTER our captured
//! stamp), so the verifier rejects.
//!
//! This test exercises the production helpers
//! [`spec_appserver::read_proc_start_time`] and
//! [`spec_appserver::verify_owned_pid`] introduced in this PR.

#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use calm_server::spec_appserver::{read_proc_start_time, verify_owned_pid};
use tokio::process::Command;

/// `verify_owned_pid` accepts a live child whose persisted stamp we
/// just captured. This is the steady-state "we did spawn this, it is
/// still our process" path the takeover hot loop relies on.
#[tokio::test]
async fn inv5_verify_owned_pid_accepts_live_child() {
    // A long-running child we can stamp + probe. `sleep` exists on
    // every unix CI image; the test doesn't wait on it.
    let mut child = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep child");
    let pid = i32::try_from(child.id().expect("sleep pid")).expect("pid fits i32");

    let stamp = read_proc_start_time(pid).expect(
        "/proc/<pid>/stat must be readable for a live child (this test requires \
         /proc; Linux-only)",
    );

    // Sanity: same call again returns the same stamp (idempotent / not
    // reading e.g. utime).
    let stamp_again = read_proc_start_time(pid).expect("second read");
    assert_eq!(
        stamp, stamp_again,
        "starttime must be invariant for a live pid"
    );

    // The identity check accepts (pid, stamp).
    assert!(
        verify_owned_pid(pid, stamp),
        "verify_owned_pid must accept a live child whose stamp we just captured \
         (pid={pid}, stamp={stamp})"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// `verify_owned_pid` rejects a stamp that doesn't match the live
/// `/proc/<pid>/stat`. This is the load-bearing case: a recycled pid
/// post-reboot has a strictly different stamp than the one we
/// captured before the reboot, so the verifier returns `false` and
/// the takeover skips the kill.
///
/// We don't need to actually reboot to exercise the rejection: any
/// stamp-with-pid mismatch reproduces the same code path. We
/// fabricate a "stale" stamp by adding 1 to the live stamp — that
/// value can NEVER match any real process's starttime for this pid
/// (starttime is invariant for a given pid; only a future,
/// post-recycle pid could have a higher stamp, and on this pid it
/// would have to coincidentally equal stamp+1).
#[tokio::test]
async fn inv5_verify_owned_pid_rejects_stale_stamp_for_live_pid() {
    let mut child = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep child");
    let pid = i32::try_from(child.id().expect("sleep pid")).expect("pid fits i32");

    let live = read_proc_start_time(pid).expect("live starttime readable");
    let stale = live.wrapping_add(1);

    assert!(
        !verify_owned_pid(pid, stale),
        "verify_owned_pid MUST reject a (pid, stamp) where stamp != live starttime \
         (post-reboot pid recycle: live={live}, stale={stale}). If this passes, the \
         identity check has degenerated to a liveness probe and the takeover would \
         SIGTERM/SIGKILL an unrelated process group."
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// `verify_owned_pid` rejects a pid that doesn't exist in `/proc`
/// (the process is gone, or never existed). `i32::MAX` is reserved
/// well above the kernel's pid ceiling (PID_MAX_LIMIT = 2^22 on
/// 64-bit Linux), so the `/proc/<i32::MAX>/stat` read is guaranteed
/// to ENOENT.
#[tokio::test]
async fn inv5_verify_owned_pid_rejects_nonexistent_pid() {
    let stamp = 12_345u64;
    assert!(
        !verify_owned_pid(i32::MAX, stamp),
        "verify_owned_pid MUST reject a pid whose /proc entry doesn't exist \
         (the process is dead and the pid has been reclaimed by the kernel \
         without recycling). If this passes, a dead-pid lookup is silently \
         treated as identity-confirmed and the takeover could SIGTERM/SIGKILL \
         an unrelated process that happens to be at the same pgid later."
    );

    // `read_proc_start_time` must mirror the rejection — None on
    // ENOENT, not a panic / a 0 stamp / a spurious read.
    assert_eq!(read_proc_start_time(i32::MAX), None);
}

/// `verify_owned_pid` rejects a stamp captured BEFORE a child died,
/// against a pid that has since been reused by a different process.
/// This is the exact race the persisted (pid, start_time) pair
/// defends against: between capture and verify, the kernel reaped the
/// original process and assigned its pid to a fresh user process.
///
/// We can't deterministically force pid recycling in a test (the
/// kernel's pid allocator is best-effort sequential), but we CAN
/// exercise the moral equivalent:
///
///   1. Spawn child A. Capture (pid_a, stamp_a).
///   2. Wait for A to exit. (`/proc/pid_a/stat` is now gone — same
///      as a recycled pid where the original is dead.)
///   3. `verify_owned_pid(pid_a, stamp_a)` must return `false`
///      because there is no live process at that pid anymore. If a
///      later spawn reused `pid_a`, its starttime would be strictly
///      later than `stamp_a` and the verifier would STILL reject —
///      either way the contract holds.
#[tokio::test]
async fn inv5_verify_owned_pid_rejects_after_child_exit() {
    let mut child = Command::new("true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn true");
    let pid = i32::try_from(child.id().expect("true pid")).expect("pid fits i32");

    // Stamp BEFORE the child exits (`true` returns immediately, but
    // /proc entries linger briefly until the parent reaps).
    let stamp = read_proc_start_time(pid).expect("/proc readable while child alive or zombie");

    // Reap the child. After `wait`, the kernel clears `/proc/<pid>`.
    let _ = child.wait().await;

    // Poll briefly: `/proc/<pid>` cleanup is synchronous on `wait`
    // but defensive — wait up to 2s.
    let mut gone = false;
    for _ in 0..40 {
        if read_proc_start_time(pid).is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        gone,
        "after wait(), /proc/{pid} must be gone within 2s (kernel reap is sync \
         with wait but we poll defensively)"
    );

    assert!(
        !verify_owned_pid(pid, stamp),
        "verify_owned_pid MUST reject a (pid, stamp) when /proc/<pid> is gone \
         (the original process is reaped). If this passes, the takeover would \
         signal a pgid the kernel has already reclaimed — and a future spawn \
         that lands on that pid would be killed instead. pid={pid}, stamp={stamp}."
    );
}

/// `read_proc_start_time` returns a sensible (nonzero, monotonic-ish)
/// stamp for a freshly-spawned child. Pinning this is a defence
/// against a future refactor that, e.g., parses the wrong field
/// (utime, stime) and silently produces near-zero counter values
/// indistinguishable across processes — which would defeat the
/// identity check entirely.
#[tokio::test]
async fn inv5_read_proc_start_time_returns_nonzero_field_22() {
    let mut child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid = i32::try_from(child.id().expect("sleep pid")).expect("pid fits i32");

    let stamp = read_proc_start_time(pid).expect("/proc readable");
    // `starttime` is jiffies-since-boot at process creation. Any
    // long-lived test host has uptime > a few seconds, so `starttime`
    // for a child spawned NOW is at minimum (uptime_jiffies - a
    // small slack). It's effectively never < 100 on a real Linux
    // box; the conservative assertion is "nonzero", which fails
    // immediately if the parser landed on a zero-valued field like
    // a fresh counter.
    assert!(
        stamp > 0,
        "starttime field 22 of /proc/<pid>/stat must be > 0 for a child \
         spawned post-boot; got {stamp} (likely the parser landed on the \
         wrong field — utime/stime/etc. — and the identity check is now a \
         zero-vs-zero comparison)"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

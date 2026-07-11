//! # INV-5 — pid ownership must be stronger than `kill(pid, 0)` and stronger than a TOCTOU socket probe
//!
//! **Bug**: R3-B1 (from #318)
//! **Encoded contract**: before signaling a persisted pgid as "ours",
//! the kernel must hold evidence that pins the *identity* of the
//! process at that pgid (not just its liveness). The canonical Linux
//! mechanism is the `(pid, start_time, boot_id)` triple:
//!
//!   * `start_time` — field 22 (1-indexed) of `/proc/<pid>/stat`,
//!     clock-ticks-since-boot at process creation. Invariant within a
//!     boot; defends against same-boot pid recycle (the recycled
//!     process has a strictly later stamp).
//!   * `boot_id` — `/proc/sys/kernel/random/boot_id`, a per-boot UUID.
//!     Distinguishes "same kernel, pid recycled" from "host rebooted —
//!     every prior pid is dead".
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
//! `(pid, start_time, boot_id)` closes the window:
//!
//!   * Mid-boot pid recycle: recycled process has a later `start_time`.
//!   * Cross-reboot pid recycle: `boot_id` differs.
//!   * Liveness failure: `/proc/<pid>` ENOENT → reject.
//!
//! This test exercises the production helpers
//! [`proc_identity::read_proc_start_time`],
//! [`proc_identity::read_boot_id`], and
//! [`proc_identity::verify_owned_pid`] introduced in this PR.

// `/proc` is Linux-only. The production helpers compile on every Unix
// (with a non-Linux stub that returns `None` / `false`), but a
// behavioral test that reads `/proc` directly to capture a real stamp
// can only run on Linux. macOS / BSD CI would otherwise fail because
// `read_proc_start_time` returns `None` and the `.expect(…)` in these
// tests would panic.
#![cfg(target_os = "linux")]

use std::process::Stdio;
use std::time::Duration;

use calm_server::proc_identity::{read_boot_id, read_proc_start_time, verify_owned_pid};
use tokio::process::Command;

/// `verify_owned_pid` accepts a live child whose persisted
/// `(start_time, boot_id)` we just captured. This is the steady-state
/// "we did spawn this, it is still our process" path the takeover hot
/// loop relies on.
#[tokio::test]
async fn inv5_verify_owned_pid_accepts_live_child() {
    // A long-running child we can stamp + probe. `sleep` exists on
    // every Linux CI image; the test doesn't wait on it.
    let mut child = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep child");
    let pid = i32::try_from(child.id().expect("sleep pid")).expect("pid fits i32");

    let stamp = read_proc_start_time(pid).expect("/proc/<pid>/stat readable for live child");
    let boot = read_boot_id().expect("/proc/sys/kernel/random/boot_id readable on Linux");

    // Sanity: repeated reads return the same values (idempotent / not
    // reading e.g. utime / not reading a generated value).
    let stamp_again = read_proc_start_time(pid).expect("second start_time read");
    assert_eq!(
        stamp, stamp_again,
        "starttime must be invariant for a live pid"
    );
    let boot_again = read_boot_id().expect("second boot_id read");
    assert_eq!(
        boot, boot_again,
        "boot_id must be invariant within a kernel boot"
    );

    // The identity check accepts (pid, stamp, boot).
    assert!(
        verify_owned_pid(pid, stamp, &boot),
        "verify_owned_pid must accept a live child whose stamp+boot we just \
         captured (pid={pid}, stamp={stamp}, boot={boot})"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// `verify_owned_pid` rejects a `start_time` stamp that doesn't match
/// the live `/proc/<pid>/stat` — even with a matching boot_id. This
/// is the same-boot pid-recycle defence: a recycled pid has a
/// strictly later stamp than the one we captured before it was
/// recycled.
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

    let live_stamp = read_proc_start_time(pid).expect("live starttime");
    let boot = read_boot_id().expect("live boot_id");
    let stale_stamp = live_stamp.wrapping_add(1);

    assert!(
        !verify_owned_pid(pid, stale_stamp, &boot),
        "verify_owned_pid MUST reject a (pid, stamp, boot) where stamp != live \
         starttime (same-boot pid recycle: live={live_stamp}, stale={stale_stamp}). \
         If this passes, the identity check has degenerated to a liveness probe \
         and the takeover would SIGTERM/SIGKILL an unrelated process group."
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// `verify_owned_pid` rejects a `boot_id` that doesn't match the live
/// kernel — even with a matching `start_time` and a live pid. This is
/// the **cross-reboot** defence: after a host reboot the prior boot's
/// entire pid namespace is dead, but `start_time` is also relative to
/// the new boot, so a coincidental stamp collision could in principle
/// fool a stamp-only check. The `boot_id` companion makes this
/// impossible.
#[tokio::test]
async fn inv5_verify_owned_pid_rejects_stale_boot_id() {
    let mut child = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep child");
    let pid = i32::try_from(child.id().expect("sleep pid")).expect("pid fits i32");

    let stamp = read_proc_start_time(pid).expect("live starttime");
    let live_boot = read_boot_id().expect("live boot_id");
    // Fabricate a "from a different boot" UUID. The kernel's boot_id is a
    // valid UUID; we just need a string that compares !=.
    let stale_boot = "00000000-0000-0000-0000-000000000000";
    assert_ne!(
        live_boot, stale_boot,
        "live boot_id must differ from the fabricated all-zeros UUID"
    );

    assert!(
        !verify_owned_pid(pid, stamp, stale_boot),
        "verify_owned_pid MUST reject a (pid, stamp, boot) where boot != live \
         kernel boot_id (host reboot: live_boot={live_boot}, stale_boot={stale_boot}). \
         If this passes, after a host reboot a recycled pid with a coincidentally-\
         matching start_time could be SIGTERM/SIGKILL'd. The boot_id companion \
         exists precisely to close this gap."
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// `verify_owned_pid` rejects a pid that doesn't exist in `/proc`
/// (the process is gone, or never existed). `i32::MAX` is reserved
/// well above the kernel's pid ceiling (PID_MAX_LIMIT = 2^22 on
/// 64-bit Linux), so `/proc/<i32::MAX>/stat` is guaranteed ENOENT.
#[tokio::test]
async fn inv5_verify_owned_pid_rejects_nonexistent_pid() {
    let stamp = 12_345u64;
    let boot = read_boot_id().expect("live boot_id");
    assert!(
        !verify_owned_pid(i32::MAX, stamp, &boot),
        "verify_owned_pid MUST reject a pid whose /proc entry doesn't exist \
         (the process is dead and the pid has been reclaimed by the kernel \
         without recycling). If this passes, a dead-pid lookup is silently \
         treated as identity-confirmed."
    );

    // `read_proc_start_time` must mirror the rejection — None on
    // ENOENT, not a panic / 0 / spurious read.
    assert_eq!(read_proc_start_time(i32::MAX), None);
}

/// `verify_owned_pid` rejects a stamp captured BEFORE a child died,
/// against a pid that may since be reused by a different process.
/// This is the exact race the persisted identity stamp defends
/// against. We can't deterministically force pid recycling, but we
/// CAN exercise the moral equivalent:
///
///   1. Spawn child A. Capture (pid_a, stamp_a, boot).
///   2. Wait for A to exit + be reaped.
///   3. `verify_owned_pid(pid_a, stamp_a, boot)` must return `false`
///      because there is no live process at that pid anymore.
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
    let boot = read_boot_id().expect("live boot_id");

    // Reap the child. After `wait`, the kernel clears `/proc/<pid>`.
    let _ = child.wait().await;

    // Poll briefly: `/proc/<pid>` cleanup is synchronous on `wait` but
    // we poll defensively up to 2s.
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
        !verify_owned_pid(pid, stamp, &boot),
        "verify_owned_pid MUST reject a (pid, stamp, boot) when /proc/<pid> is \
         gone (the original process is reaped). pid={pid}, stamp={stamp}."
    );
}

/// `read_proc_start_time` returns a sensible (nonzero) stamp for a
/// freshly-spawned child. Pinning this is a defence against a future
/// refactor that, e.g., parses the wrong field (utime, stime) and
/// silently produces near-zero counter values indistinguishable
/// across processes.
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

/// `read_boot_id` returns a well-formed UUID string. Pinning the shape
/// guards against a parse-the-wrong-file refactor.
#[test]
fn inv5_read_boot_id_returns_canonical_uuid() {
    let boot = read_boot_id().expect("/proc/sys/kernel/random/boot_id readable on Linux");
    // Canonical UUID is 36 chars: 8-4-4-4-12 hex with 4 dashes.
    assert_eq!(
        boot.len(),
        36,
        "boot_id must be a 36-char canonical UUID; got {boot:?}"
    );
    let dashes: Vec<usize> = boot
        .char_indices()
        .filter_map(|(i, c)| (c == '-').then_some(i))
        .collect();
    assert_eq!(
        dashes,
        vec![8, 13, 18, 23],
        "boot_id must have dashes at positions 8/13/18/23 (canonical UUID); got {boot:?}"
    );
}

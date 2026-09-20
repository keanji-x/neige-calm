//! Pid ownership must be pinned by the `(pid, start_time, boot_id)` triple, not by `kill(pid, 0)`
//! or a TOCTOU socket probe: `start_time` defends against same-boot pid recycle, `boot_id`
//! against cross-reboot recycle.

// `/proc` is Linux-only; the non-Linux production stubs return `None` / `false`.
#![cfg(target_os = "linux")]

use std::process::Stdio;
use std::time::Duration;

use calm_server::proc_identity::{read_boot_id, read_proc_start_time, verify_owned_pid};
use tokio::process::Command;

#[tokio::test]
async fn inv5_verify_owned_pid_accepts_live_child() {
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

    // Repeated reads return the same values (not reading e.g. utime).
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

    assert!(
        verify_owned_pid(pid, stamp, &boot),
        "verify_owned_pid must accept a live child whose stamp+boot we just \
         captured (pid={pid}, stamp={stamp}, boot={boot})"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Same-boot pid-recycle defence: a recycled pid has a strictly later stamp.
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

/// Cross-reboot defence: `start_time` is relative to the new boot, so a coincidental stamp
/// collision could fool a stamp-only check.
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
    // Any string that compares != to the kernel's boot_id.
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

/// `i32::MAX` is well above the kernel's pid ceiling (PID_MAX_LIMIT = 2^22), so `/proc/<i32::MAX>/stat` is ENOENT.
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

    // `read_proc_start_time` must mirror the rejection — None on ENOENT.
    assert_eq!(read_proc_start_time(i32::MAX), None);
}

/// Pid recycling cannot be forced deterministically; a stamp captured before the child died
/// must be rejected once there is no live process at that pid.
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

    // Stamp BEFORE the child exits (/proc entries linger until the parent reaps).
    let stamp = read_proc_start_time(pid).expect("/proc readable while child alive or zombie");
    let boot = read_boot_id().expect("live boot_id");

    let _ = child.wait().await;

    // `/proc/<pid>` cleanup is synchronous on `wait`, but poll defensively.
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

/// A nonzero stamp guards against parsing the wrong field (utime, stime).
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

/// Pinning the UUID shape guards against a parse-the-wrong-file refactor.
#[test]
fn inv5_read_boot_id_returns_canonical_uuid() {
    let boot = read_boot_id().expect("/proc/sys/kernel/random/boot_id readable on Linux");
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

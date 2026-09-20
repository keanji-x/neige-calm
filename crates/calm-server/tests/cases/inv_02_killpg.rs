//! Kill covers the entire process group, not just the leader: the `node` launcher forks the
//! native `codex app-server` as a grandchild in the same pgid, so a pid-only kill leaks it.

// `/proc/<pid>/stat` is Linux-only.
#![cfg(target_os = "linux")]

use std::process::Stdio;
use std::time::Duration;

use calm_server::proc_identity::signal_process_group;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Read `/proc/<pid>/stat` field 5 (pgrp); `None` once the process is reaped.
fn pgrp_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(2)?.parse().ok()
}

/// Spawn `sh -c "sleep 60 & echo $! ; wait"` as a process-group leader.
/// Returns `(child, leader_pid_aka_pgid, grandchild_pid)`.
async fn spawn_leader_with_grandchild() -> (tokio::process::Child, i32, i32) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 60 & echo $! ; wait")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn launcher");
    let pgid = i32::try_from(child.id().expect("launcher pid")).expect("pid fits i32");

    let mut out = child.stdout.take().expect("stdout piped");
    let mut buf = Vec::new();
    let grandchild_pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut b = [0u8; 64];
            let n = out.read(&mut b).await.expect("read launcher stdout");
            if n == 0 {
                panic!("launcher closed stdout before printing child pid");
            }
            buf.extend_from_slice(&b[..n]);
            if let Some(nl) = buf.iter().position(|&c| c == b'\n') {
                let line = String::from_utf8_lossy(&buf[..nl]);
                break line.trim().parse::<i32>().expect("grandchild pid int");
            }
        }
    })
    .await
    .expect("timed out reading grandchild pid");

    // Sanity: the grandchild shares the leader's pgid.
    assert_eq!(
        pgrp_of(grandchild_pid),
        Some(pgid),
        "grandchild must share the launcher's process group"
    );

    (child, pgid, grandchild_pid)
}

/// Wait up to `within` for `pid` to vanish from /proc.
async fn await_gone(pid: i32, within: Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if pgrp_of(pid).is_none() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pgrp_of(pid).is_none()
}

/// SIGKILL the whole group + waitpid the launcher, so nothing leaks even when an assertion fires.
async fn force_cleanup(mut child: tokio::process::Child, pgid: i32) {
    let _ = tokio::task::spawn_blocking(move || unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    })
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// A pid-only SIGTERM to the leader leaves the grandchild alive; the group form reaps both.
/// `#[ignore]`d because it passes on main; it is a regression guard for teardown refactors.
#[tokio::test]
#[ignore = "regression-guard: passes on main; un-ignore to verify after teardown refactors. \
            see issue #318 INV-2."]
async fn inv2_pid_only_kill_leaks_grandchild() {
    let (child, pgid, grandchild_pid) = spawn_leader_with_grandchild().await;

    // Bug shape: kill ONLY the leader pid (`pgid` is also the leader pid via `process_group(0)`).
    let rc = unsafe { libc::kill(pgid, libc::SIGTERM) };
    assert_eq!(rc, 0, "kill(leader_pid, SIGTERM) should deliver");

    // The leader dies; the grandchild is reparented to init and keeps running.
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        pgrp_of(grandchild_pid).is_some(),
        "INV-2 GUARD: pid-only SIGTERM to the leader (pid={pgid}) was supposed to \
         leak the `sleep` grandchild (pid={grandchild_pid}). If the grandchild is \
         gone, either /proc is racy or the launcher's exit reaped it via SIGHUP \
         propagation (unexpected without controlling-terminal semantics). Either \
         way, the encoding of 'pid-only kill leaks grandchild' is broken — fix \
         the test."
    );

    // The group kill MUST reap the grandchild.
    assert!(
        signal_process_group(pgid, libc::SIGKILL),
        "signal_process_group(pgid, SIGKILL) should deliver"
    );
    let gone = await_gone(grandchild_pid, Duration::from_millis(500)).await;
    force_cleanup(child, pgid).await;

    assert!(
        gone,
        "INV-2 violated: even the group SIGKILL failed to reap grandchild \
         pid={grandchild_pid} (group pgid={pgid}) within 500ms. \
         `signal_process_group` must use `kill(-pgid, …)` (the negative-pgid \
         form) so the kernel signals the whole group. A regression to \
         `kill(pid, …)` would manifest exactly this way."
    );
}

/// SIGKILL (not SIGTERM) so the test does not depend on the launcher cooperating: this isolates
/// group targeting from the orthogonal "what signal" question.
#[tokio::test]
async fn inv2_signal_process_group_targets_group_not_pid() {
    let (child, pgid, grandchild_pid) = spawn_leader_with_grandchild().await;

    let delivered = signal_process_group(pgid, libc::SIGKILL);
    assert!(
        delivered,
        "signal_process_group(pgid={pgid}, SIGKILL) reported failure on a known-live group"
    );

    // The GRANDCHILD must die; a pid-only rewrite would leave it lingering.
    let gone = await_gone(grandchild_pid, Duration::from_millis(500)).await;
    force_cleanup(child, pgid).await;

    assert!(
        gone,
        "INV-2 violated: signal_process_group(pgid={pgid}, SIGKILL) failed to reap \
         grandchild pid={grandchild_pid} within 500ms. The helper must target the \
         process GROUP (`kill(-pgid, …)`), not just the leader pid — a pid-only \
         kill leaves the native `codex app-server` grandchild alive holding the \
         listen socket. See proc_identity.rs::signal_process_group."
    );
}

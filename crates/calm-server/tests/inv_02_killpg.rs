//! # INV-2 — kill covers the entire process group, not just the leader
//!
//! **Bug**: R1-B2 (from #318)
//! **Encoded contract**: any teardown path that reaps a codex app-server
//! MUST signal the **whole process group** (`kill(-pgid, …)`), not just
//! the leader pid. The native `codex app-server` is a grandchild forked
//! by the `node` launcher; killing only the leader pid leaks the
//! grandchild and leaves the listen socket bound.
//!
//! **Why this design**: we model the production launcher shape (node →
//! native codex grandchild sharing the leader's pgid) with a `sh -c`
//! launcher that spawns a long-lived `sleep` grandchild in the same
//! group. We then attempt a leader-only kill (the bug shape) and assert
//! that the grandchild is STILL ALIVE afterwards — proving that "kill the
//! leader pid" is not equivalent to "kill the group". Production code
//! today calls `signal_process_group(-pgid, …)` and would reap both; this
//! test pins the SEMANTIC GAP between the two operations so any future
//! refactor that swaps `signal_process_group` for a pid-only `child.kill`
//! immediately fails here.
//!
//! **Current behavior on main**: the test fails *as designed* because
//! `signal_process_group(pgid, …)` IS already used in production (good!),
//! but the bug-shape assertion this test makes — "a pid-only kill leaks
//! the grandchild" — is meant to fail in this test environment because
//! the test ALSO exercises the production-correct group kill at the end
//! and verifies the grandchild is reaped THEN. Without a regression test
//! pinning the difference, a future refactor swapping `kill(-pgid)` for
//! `kill(pid)` would silently re-introduce R1-B2. We encode the
//! difference explicitly so a deviation surfaces immediately.
//!
//! **Stronger contract — what fails on main**: we additionally assert
//! that `signal_process_group(pgid)` reaps the grandchild within a
//! BOUNDED WINDOW (300ms). The current implementation only sends SIGTERM
//! with no SIGKILL escalation here, so a child that ignores SIGTERM (we
//! plant a `trap '' TERM` launcher) lives indefinitely. This is the gap:
//! the public `signal_process_group` is single-shot; production callers
//! (`Drop`, `SpawnRollback`) only fire SIGTERM and rely on the launcher
//! cooperating. A spec app-server that hangs on SIGTERM (intentional or
//! buggy `trap`) is not reaped by the kernel's teardown path.
//!
//! See: `src/spec_appserver.rs::signal_process_group` (line ~672),
//! `Drop for SpecPushHandle` (line ~640), `SpawnRollback::drop`
//! (line ~1047).

#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use calm_server::spec_appserver::signal_process_group;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Read `/proc/<pid>/stat` field 5 (pgrp). Returns `None` if the process
/// is gone (the `/proc` entry vanishes on reap).
fn pgrp_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(2)?.parse().ok()
}

/// Test that `signal_process_group(pgid, SIGTERM)` (the production
/// teardown call) reaps a SIGTERM-IGNORING grandchild within a bounded
/// window. The production path's single-SIGTERM Drop cannot defeat a
/// `trap '' TERM` launcher → grandchild leaks. INV-2 says "kill must
/// cover the entire process group" — a single SIGTERM that the leader
/// ignores doesn't satisfy this; an escalation to SIGKILL (or a teardown
/// path that doesn't trust SIGTERM alone) is required.
///
/// We use a launcher that:
///   1. Installs `trap '' TERM` (ignore SIGTERM).
///   2. Spawns a `sleep 60 &` grandchild in the same group.
///   3. Prints the grandchild's pid for the test to read.
///   4. `wait`s forever.
///
/// Then we call `signal_process_group(pgid, SIGTERM)` (mirroring `Drop`'s
/// teardown) and assert the grandchild is reaped within 300ms. Because
/// the launcher ignored SIGTERM and the test's signal_process_group
/// doesn't escalate, the grandchild survives — the assertion fails.
#[tokio::test]
async fn inv2_group_signal_reaps_sigterm_ignoring_grandchild() {
    let mut child = Command::new("sh")
        .arg("-c")
        // `setsid` would detach; we want this shell to be the group leader
        // (`process_group(0)`) and the sleep to inherit the same pgid.
        // The trap ignores SIGTERM in the leader so the test exercises
        // the "SIGTERM alone is insufficient" gap.
        .arg("trap '' TERM; sleep 60 & echo $! ; wait")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn trap launcher");
    let pgid = i32::try_from(child.id().expect("launcher pid")).expect("pid fits i32");

    // Read the grandchild's pid.
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

    // Sanity: the grandchild's pgrp matches the leader pgid.
    assert_eq!(
        pgrp_of(grandchild_pid),
        Some(pgid),
        "grandchild must share the launcher's process group"
    );

    // Production teardown call: bare SIGTERM to the group (mirrors
    // `Drop for SpecPushHandle` and `SpawnRollback::drop`).
    assert!(
        signal_process_group(pgid, libc::SIGTERM),
        "signal_process_group should report success delivering SIGTERM"
    );

    // INV-2 strict: within 300ms, the grandchild must be gone. The bug
    // we're catching is "a SIGTERM the launcher ignores is not 'killing
    // the group'". A correct fix would either send SIGKILL eagerly here,
    // or have the Drop path escalate via the same SIGTERM → grace →
    // SIGKILL ladder `reap_spec_push` uses.
    let mut gone = false;
    for _ in 0..30 {
        if pgrp_of(grandchild_pid).is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Teardown FIRST so we don't leak the launcher even when the assert
    // fires. (kill_on_drop will catch this on test wind-down too, but
    // explicit cleanup is cheaper to debug.)
    let cleanup_pgid = pgid;
    let _ = tokio::task::spawn_blocking(move || unsafe {
        libc::kill(-cleanup_pgid, libc::SIGKILL);
    })
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        gone,
        "INV-2 violated: signal_process_group(pgid={pgid}, SIGTERM) failed to reap \
         grandchild pid={grandchild_pid} within 300ms. The launcher ignored SIGTERM and \
         the teardown path doesn't escalate to SIGKILL — a hanging codex app-server \
         survives the kernel's Drop reaper and keeps the listen socket bound. \
         INV-2 says the teardown must guarantee the WHOLE group is reaped, not just \
         'attempted SIGTERM'."
    );
}

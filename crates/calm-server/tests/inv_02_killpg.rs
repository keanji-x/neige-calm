//! # INV-2 — kill covers the entire process group, not just the leader
//!
//! **Bug**: R1-B2 (from #318)
//! **Encoded contract**: every teardown path that reaps a codex
//! app-server MUST target the process group via `kill(-pgid, …)`, NEVER
//! the leader pid alone. The `node` launcher forks the native
//! `codex app-server` as a grandchild in the same pgid; a pid-only kill
//! reaps only the launcher and leaks the grandchild (which keeps the
//! listen socket bound and continues writing rollouts). The production
//! code today uses `spec_appserver::signal_process_group(pgid, …)`
//! everywhere (Drop, `reap_spec_push`, takeover pre-respawn cleanup),
//! which IS correct.
//!
//! ## Why this encoding (and how it differs from the v1 version)
//!
//! v1 of this test planted a `trap '' TERM` launcher and asserted the
//! grandchild died within 300ms after `signal_process_group(pgid,
//! SIGTERM)`. That's wrong: POSIX guarantees a SIGTERM-ignoring process
//! survives SIGTERM — the v1 test was encoding "SIGTERM defeats
//! SIG_IGN" (an OS-level impossibility), not the INV-2 contract.
//!
//! The actual bug from #315 R1-B2 was about **targeting**: a pid-only
//! `kill(pid)` reaches only the leader (the `node` launcher), and the
//! native `codex app-server` grandchild lives on. That fix has landed
//! (`signal_process_group` uses `kill(-pgid, …)` and is called from
//! every teardown site). So on origin/main, INV-2 is **enforced**.
//!
//! ## What this test does
//!
//! Two halves:
//!
//! 1. `inv2_pid_only_kill_leaks_grandchild` (failing → regression
//!    guard, marked `#[ignore]` because it does NOT fail on main):
//!    pin the SEMANTIC GAP between `kill(pid, SIG)` and
//!    `kill(-pgid, SIG)` by exercising both against a real launcher +
//!    grandchild. A pid-only SIGTERM leaves the grandchild alive; a
//!    group SIGTERM reaps both. A future refactor that swaps
//!    `signal_process_group(pgid, …)` for `child.kill()` or
//!    `libc::kill(pid, …)` would catch the leader but leak the
//!    grandchild — and this test would catch the regression.
//!
//! 2. `inv2_signal_process_group_targets_group_not_pid` (active, fails
//!    if a future refactor weakens `signal_process_group` to a pid-only
//!    call): we call the production helper against a launcher + sleep
//!    grandchild and assert BOTH die. If a refactor accidentally
//!    rewires it to `kill(pid, …)`, the grandchild stays alive and the
//!    assertion fires.
//!
//! Both encode the invariant ("group, not pid") behaviorally — neither
//! depends on an OS-level impossibility, and neither makes a
//! SIGTERM-ignoring child the load-bearing failure mode.
//!
//! ## On main (today)
//!
//! - Test 1 (`#[ignore]`'d regression guard): would PASS on main if
//!   un-ignored — `signal_process_group(pgid, …)` does reach the
//!   grandchild. We ignore it because the issue mandates "tests must
//!   FAIL on main" and this one doesn't — keeping it as `#[ignore]`
//!   honestly reflects that it's a regression guard, not a
//!   currently-failing invariant.
//! - Test 2 (active): also PASSES on main (group kill works). It would
//!   FAIL the day a refactor weakens the helper.
//!
//! ## Why no currently-failing INV-2 test
//!
//! After careful production-code reading we cannot construct a
//! scenario where, on main, a teardown of a healthy codex app-server
//! handle leaks any group member. The bug R1-B2 listed is fixed. The
//! residual gap a stricter reading might cite — "`Drop` only fires
//! SIGTERM, no SIGKILL escalation, so a non-cooperative launcher
//! survives Drop" — is by design (`reap_spec_push` is the load-bearing
//! teardown ladder; `Drop` is the synchronous best-effort safety net),
//! and the issue forbids reshaping production. We document the gap and
//! ship the two regression-guard tests above instead of inventing a
//! failing-on-main test that doesn't reflect an actual invariant.
//!
//! See: `src/spec_appserver.rs::signal_process_group` (line ~672), the
//! callers (`Drop for SpecPushHandle` line ~640, `SpawnRollback::drop`
//! line ~1057, `terminal_sweeper::reap_spec_push` line ~330,
//! `lib::try_takeover_one_wave` line ~321).

#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use calm_server::spec_appserver::signal_process_group;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Read `/proc/<pid>/stat` field 5 (pgrp). Returns `None` if the
/// process is gone (the `/proc` entry vanishes on reap).
fn pgrp_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(2)?.parse().ok()
}

/// Spawn `sh -c "sleep 60 & echo $! ; wait"` as a process-group
/// leader. Returns `(child, leader_pid_aka_pgid, grandchild_pid)`. The
/// child is `kill_on_drop(true)` for test-teardown safety.
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

/// Wait up to `Duration` for `pid` to be reaped from /proc. Returns
/// `true` if gone within the window.
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

/// Force-cleanup helper: SIGKILL the whole group + waitpid the
/// launcher. Called from every test arm to avoid leaks even when an
/// assertion fires.
async fn force_cleanup(mut child: tokio::process::Child, pgid: i32) {
    let _ = tokio::task::spawn_blocking(move || unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    })
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// INV-2 (regression guard, `#[ignore]`'d): demonstrate that the
/// SEMANTIC DIFFERENCE between "pid-only kill" and "group kill" is
/// load-bearing — a pid-only SIGTERM to the leader leaves the
/// grandchild alive, while `signal_process_group(pgid, SIGTERM)` reaps
/// both.
///
/// Marked `#[ignore]` because on origin/main this test PASSES: the
/// production helper uses the group form. Issue #318 mandates "tests
/// must fail on main"; this one doesn't, and we'd rather mark it
/// honest than fake a failure. Un-ignore to use as a regression check
/// after any refactor that touches teardown.
#[tokio::test]
#[ignore = "regression-guard: passes on main; un-ignore to verify after teardown refactors. \
            see issue #318 INV-2."]
async fn inv2_pid_only_kill_leaks_grandchild() {
    let (child, pgid, grandchild_pid) = spawn_leader_with_grandchild().await;

    // Bug shape: kill ONLY the leader pid, NOT the group. (`pgid` is
    // also the leader pid because the launcher was spawned as a group
    // leader via `process_group(0)`.)
    let rc = unsafe { libc::kill(pgid, libc::SIGTERM) };
    assert_eq!(rc, 0, "kill(leader_pid, SIGTERM) should deliver");

    // The leader dies (it's a plain sh, not trap-ignoring). The
    // grandchild (`sleep 60`) inherits PPID=1 (init / systemd) and
    // keeps running. Give the kernel ~150ms to reparent + reap the
    // launcher.
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

    // Now do the group kill — this MUST reap the grandchild. If a
    // future refactor weakens `signal_process_group` to a pid-only
    // call, this assertion fires.
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

/// INV-2 strict: `signal_process_group(pgid, SIGKILL)` MUST reap every
/// process in the group, not just the leader. This is the *current*
/// behavior on main; the test exists to fail the day a refactor
/// weakens the helper to a pid-only kill.
///
/// We use SIGKILL (not SIGTERM) so the test does NOT depend on the
/// launcher cooperating — SIGKILL cannot be caught or ignored. This
/// isolates the INV-2 invariant (group targeting) from the orthogonal
/// "what signal" question.
///
/// Note: on origin/main this test PASSES — the production helper
/// `signal_process_group` already does `kill(-pgid, …)`. We ship it as
/// active (no `#[ignore]`) because:
///   (a) it's a guard against the `R1-B2`-shape regression returning;
///   (b) the issue allows tests that pass IF they encode an invariant
///       and would fail a known-bad implementation; this test would
///       fail any pid-only weakening of the helper, which is the bug.
#[tokio::test]
async fn inv2_signal_process_group_targets_group_not_pid() {
    let (child, pgid, grandchild_pid) = spawn_leader_with_grandchild().await;

    // Production call shape — exactly what Drop / reap_spec_push /
    // try_takeover_one_wave invoke.
    let delivered = signal_process_group(pgid, libc::SIGKILL);
    assert!(
        delivered,
        "signal_process_group(pgid={pgid}, SIGKILL) reported failure on a known-live group"
    );

    // INV-2 strict: the GRANDCHILD must die. If signal_process_group
    // were rewritten as `kill(pid, SIGKILL)` (the bug shape), only the
    // launcher dies and the grandchild lingers (reparented to init,
    // continues `sleep 60`).
    let gone = await_gone(grandchild_pid, Duration::from_millis(500)).await;
    force_cleanup(child, pgid).await;

    assert!(
        gone,
        "INV-2 violated: signal_process_group(pgid={pgid}, SIGKILL) failed to reap \
         grandchild pid={grandchild_pid} within 500ms. The helper must target the \
         process GROUP (`kill(-pgid, …)`), not just the leader pid — a pid-only \
         kill leaves the native `codex app-server` grandchild alive holding the \
         listen socket. See spec_appserver.rs::signal_process_group."
    );
}

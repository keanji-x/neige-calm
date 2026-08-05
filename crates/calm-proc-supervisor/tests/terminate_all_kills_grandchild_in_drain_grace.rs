//! #993 semantics lock for the *hard* case, guarding the #1013 fix.
//!
//! `terminate_all_kills_grandchild.rs` locks the easy half: the leader is
//! still alive, so any liveness predicate you can think of says "signal it".
//! This file locks the half that actually discriminates between the right
//! guard and the tempting wrong one.
//!
//! The three cases the #1013 guard distinguishes are:
//!
//! ```text
//! 1. !reaped                  -> leader still a group member  -> signal
//! 2. reaped && exit.is_none() -> inside the drain grace; a grandchild holds
//!                                the slave, group non-empty    -> signal
//! 3. exit.is_some()           -> drained, group empty, pid recyclable
//!                                                              -> refuse
//! ```
//!
//! Case 1 is the other test. Case 3 is `signal_rejects_reaped_pty.rs`. **This
//! test is case 2** — and it is the only one that goes red if the guard is
//! narrowed to `pty_running()` (which is `!reaped && exit.is_none()`, i.e.
//! false here) or to `process_group_leader().is_some()`. Either narrowing
//! reintroduces the #993 grandchild leak; this test is the detector.
//!
//! Determinism: case 2 is a window, and in production that window is
//! `PTY_DRAIN_GRACE` = 50ms — far too tight to *race* into from a test. So we
//! do not race: `start_with_drain_grace` widens the window to 5s, and we then
//! *establish* the state (leader reaped, grandchild alive, no sticky exit yet)
//! by assertion before acting. Nothing here sleeps as a synchronisation
//! primitive; every wait polls a real condition with a deadline.
//!
//! This test is green both before and after the #1013 fix — that is the point.
//! If it goes red, the fix changed #993 semantics.
//!
//! Two properties the fixture must have, and both are asserted rather than
//! assumed, because getting either wrong makes this test vacuous:
//!
//! * **Same process group.** Non-interactive `sh` has job control off, so the
//!   backgrounded child stays in the leader's process group. That shared pgid
//!   is the whole point — a grandchild in its own group would not be reached
//!   by `kill(-pgid)` no matter how the guard is written. Checked against
//!   `/proc/<pid>/stat`.
//! * **Survives the leader.** When the controlling process (the session leader
//!   `sh`) terminates, POSIX has the kernel send SIGHUP to the terminal's
//!   foreground process group. A plain `sleep 300 &` therefore dies *by
//!   itself* the moment the leader exits — which made an earlier draft of this
//!   test pass even with the guard narrowed to `pty_running()`, i.e. pass
//!   while the regression it exists to catch was present. The fixture ignores
//!   SIGHUP (`trap "" HUP` before `exec`) so the only thing that can kill the
//!   grandchild is the SIGTERM under test, and the test asserts it is still
//!   alive after the leader is reaped.
//!
//!   Installing that trap is itself a race: if the leader exits before the
//!   freshly forked subshell has run `trap`, the HUP lands and the grandchild
//!   dies anyway. `pty_exit_ordering.rs` closes it with `sleep 0.2`; here the
//!   leader instead *waits for a readiness file* the subshell touches after
//!   installing the trap — a condition, not a duration.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

mod proc_probe;
use proc_probe::{alive, await_death, await_reaped, process_group_of};

/// The fixture, as a `sh` script.
///
/// ```text
/// ( trap '' HUP        # survive the controlling-process SIGHUP
///   : > ready          # ... and only then announce readiness
///   exec sleep 300     # so $! is the sleep's own pid; SIG_IGN survives exec
/// ) &
/// echo GC=$!
/// while [ ! -e ready ]; do sleep 0.01; done   # poll the condition
/// exit 0               # leader exits, still inside the drain window
/// ```
fn grandchild_script(ready: &Path) -> String {
    let ready = ready.display();
    format!(
        "(trap '' HUP; : > '{ready}'; exec sleep 300) & echo GC=$!; \
         while [ ! -e '{ready}' ]; do sleep 0.01; done; exit 0"
    )
}

/// The drain window we operate inside. Two orders of magnitude above the
/// scheduling work between "leader reaped" and our assertions, so being inside
/// it is a fact, not a coin flip.
const WIDE_DRAIN_GRACE: Duration = Duration::from_secs(5);

#[tokio::test]
async fn terminate_all_kills_grandchild_inside_drain_grace() {
    let supervisor = InProcessProcSupervisor::start_with_drain_grace(WIDE_DRAIN_GRACE)
        .await
        .expect("start supervisor");
    let proc_id = "pty-grandchild-drain";

    // The leader exits immediately; the grandchild inherits the slave fd and
    // outlives it, so the master never EOFs and the waiter sits in the drain
    // window for the whole (widened) grace. That is exactly the shape the
    // `PTY_DRAIN_GRACE` doc comment describes as the only case where the
    // window is non-trivial.
    let scratch = tempfile::tempdir().expect("tempdir");
    let leader = ensure_pty(
        supervisor.sock(),
        proc_id,
        "/bin/sh",
        &["-c", &grandchild_script(&scratch.path().join("ready"))],
    )
    .await;
    // The leader may well have echoed `GC=` before we manage to attach, in
    // which case the line arrives in the attach *replay* rather than in a later
    // `Output` frame. Seed the parser with the replay so which of the two it
    // is cannot decide whether this test passes.
    let (mut attach, replay) = attach(supervisor.sock(), proc_id).await;
    let grandchild = read_grandchild_pid(&mut attach, replay).await;
    // Never leak a `sleep 300`, on any exit path including a panicking
    // assertion below.
    let _reaper = KillOnDrop(grandchild);

    assert_ne!(
        grandchild, leader,
        "fixture must produce a distinct grandchild, not the leader itself"
    );
    // The whole mechanism under test is "signal the *group*". If the
    // grandchild were not in the leader's group, `kill(-leader)` could never
    // reach it and this test would be measuring nothing.
    assert_eq!(
        process_group_of(grandchild),
        Some(leader),
        "grandchild {grandchild} must share the leader's process group ({leader})"
    );

    // --- Establish case 2, do not assume it. -----------------------------
    //
    // Deliberately `await_reaped`, not `await_death`: until `child.wait()`
    // returns, the leader is either running or a zombie, and `kill(pid, 0)`
    // succeeds in both states. Only once the waiter reaps it is the pid
    // released and `kill` gives ESRCH — and it is the *reap* that opens the
    // drain window this test stands inside. A `/proc`-state probe would
    // report the zombie leader as dead and let us proceed too early.
    assert!(
        await_reaped(leader, Duration::from_secs(5)),
        "leader {leader} was never reaped; the test never reached the drain \
         window it exists to cover"
    );
    assert!(
        alive(grandchild),
        "grandchild {grandchild} must be alive — otherwise this test would pass \
         without terminate_all doing anything"
    );
    // Still inside the window: the waiter is blocked in `wait_for_drain`, so
    // no sticky exit has been recorded. This is what separates case 2 from
    // case 3; if it ever flips, the test is no longer testing case 2 and we
    // want a loud failure rather than a quietly-degraded assertion.
    let stats = supervisor
        .registry()
        .debug_entry_stats(proc_id)
        .expect("entry still registered inside the drain grace");
    assert!(
        !stats.exited,
        "expected to be INSIDE the drain grace (no sticky exit yet) — with a {}s \
         grace this should not be reachable; the test would otherwise be \
         exercising case 3, which is a different contract",
        WIDE_DRAIN_GRACE.as_secs()
    );

    // --- Act -------------------------------------------------------------
    //
    // The same call `main.rs` makes on shutdown, invoked directly so the
    // assertion below observes it rather than `Drop` ordering.
    supervisor.registry().terminate_all_process_groups_sync();

    assert!(
        await_death(grandchild, Duration::from_secs(5)),
        "grandchild {grandchild} survived terminate_all_process_groups while the \
         entry was inside the drain grace — the #1013 liveness guard was \
         narrowed past case 2 and the #993 grandchild leak is back"
    );
    assert!(
        !alive(grandchild),
        "grandchild {grandchild} must be gone at the end of the test"
    );
}

/// Belt-and-braces cleanup: a `sleep 300` must never outlive this test, not
/// even when an assertion above panics before the terminate_all call.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0 as libc::pid_t, libc::SIGKILL);
        }
    }
}

async fn read_grandchild_pid(stream: &mut UnixStream, replay: Vec<u8>) -> u32 {
    let mut buf = String::from_utf8_lossy(&replay).into_owned();
    if let Some(pid) = parse_grandchild_pid(&buf) {
        return pid;
    }
    loop {
        match timeout_read(stream).await {
            ControlReply::Output { bytes, .. } => {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                if let Some(pid) = parse_grandchild_pid(&buf) {
                    return pid;
                }
            }
            other => panic!("unexpected frame while waiting for grandchild pid: {other:?}"),
        }
    }
}

async fn ensure_pty(sock: &Path, proc_id: &str, program: &str, args: &[&str]) -> u32 {
    let mut stream = UnixStream::connect(sock).await.expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: program.into(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            envs: Vec::new(),
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 1024 * 1024,
        }),
    )
    .await
    .expect("write ensure");
    let pid = match read_frame(&mut stream).await.expect("read spawned") {
        ControlReply::Spawned { pid } => pid,
        other => panic!("unexpected first reply: {other:?}"),
    };
    match read_frame(&mut stream).await.expect("read ready") {
        ControlReply::Ready => {}
        other => panic!("unexpected second reply: {other:?}"),
    }
    pid
}

/// Extracts the pid from a complete `GC=<digits><terminator>` line. Returns
/// `None` until the terminator has been seen, so a chunk boundary in the middle
/// of the number cannot truncate the pid we parse.
fn parse_grandchild_pid(buf: &str) -> Option<u32> {
    let rest = buf.split("GC=").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() == rest.len() {
        return None;
    }
    Some(digits.parse().expect("grandchild pid"))
}

async fn attach(sock: &Path, proc_id: &str) -> (UnixStream, Vec<u8>) {
    let mut stream = UnixStream::connect(sock).await.expect("connect attach");
    write_frame(
        &mut stream,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: Some(0),
            reader_id: "test".into(),
        }),
    )
    .await
    .expect("write attach");
    let replay = match read_frame(&mut stream).await.expect("read attach ok") {
        ControlReply::AttachOk(attached) => attached.replay,
        other => panic!("unexpected attach reply: {other:?}"),
    };
    (stream, replay)
}

async fn timeout_read(stream: &mut UnixStream) -> ControlReply {
    tokio::time::timeout(Duration::from_secs(5), read_frame(stream))
        .await
        .expect("timed out reading reply")
        .expect("read reply")
}

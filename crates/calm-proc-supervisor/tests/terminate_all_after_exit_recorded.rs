//! #1013 T5b — the lock on PR-B's one behaviour change: deleting the
//! `exit.is_none()` filter from `terminate_all_process_groups_sync`.
//!
//! # Why this file has to exist
//!
//! Until PR-B, shutdown skipped every entry whose sticky exit had already been
//! recorded. That was not paranoia: in that state the leader had been reaped,
//! so its pgid was back in the kernel's allocator and signalling it was exactly
//! the #1013 defect. The cost was a #993-shaped leak — a grandchild that
//! outlives a *recorded* exit never gets the shutdown SIGTERM.
//!
//! The pin removes the reason for the filter: the leader is a zombie this
//! process owns for the entry's whole registry lifetime, so its pgid cannot
//! have been recycled. Deleting the filter is therefore safe *only with* the
//! pin, which is why it could not ship in PR-A.
//!
//! And a claimed improvement that no test would notice being reverted is not
//! locked at all. `terminate_all_kills_grandchild_in_drain_grace.rs` (T5)
//! stands *inside* the drain grace, where `exit.is_none()` is still true, so it
//! is green under both the old and the new predicate. **Reverting the deletion
//! with the whole rest of the plan green is exactly what this file prevents.**
//!
//! # The state is established, not raced
//!
//! Every step below is a polled condition on durable state, not a sleep:
//!
//! * The leader exits immediately, but a grandchild holds the slave, so the
//!   master never EOFs. The waiter's drain wait therefore **times out** — the
//!   default 50ms grace is left alone precisely so that it does; that timeout
//!   is a certainty here, not a race, because the reader can never see `Ok(0)`.
//! * The waiter then unconditionally seals and stamps the sticky exit, so
//!   `exit_recorded` becomes true and stays true. We poll for it.
//! * `removable`'s third condition is `eof_reached`, which the same grandchild
//!   keeps false forever, so the entry cannot be swept out from under us. A
//!   long reclaim grace is used as well, belt and braces. We assert it rather
//!   than assume it.
//!
//! If the establishment steps ever become unreliable, fix the polling. Do not
//! "solve" it by putting the filter back.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::net::UnixStream;

mod proc_probe;
use proc_probe::{alive, await_death, process_group_of};

/// Long enough that the entry is nowhere near the sweeper while we work, short
/// enough that a wedged run still ends. The real guard against a sweep is
/// `eof_reached` staying false, which is asserted.
const LONG_RECLAIM_GRACE: Duration = Duration::from_secs(60);

/// Same shape as T5's fixture: a backgrounded subshell that ignores SIGHUP (the
/// kernel HUPs the foreground group when the session leader exits, and without
/// the trap the grandchild would die on its own and the test would pass while
/// measuring nothing), announces readiness through a file, and then holds the
/// slave open forever.
///
/// The pid is passed through a **file**, not through the pty. T5 parses a
/// `GC=` line out of the attach stream because it has a 5s drain grace to do it
/// in; here the grace is the 50ms default, so `Exited` can land before the
/// parse finishes and there would be nothing to do but fail. A file is a
/// durable state to poll for.
fn grandchild_script(ready: &Path, pid_file: &Path) -> String {
    let ready = ready.display();
    let pid_file = pid_file.display();
    format!(
        "(trap '' HUP; : > '{ready}'; exec sleep 300) & printf '%s' \"$!\" > '{pid_file}'; \
         while [ ! -e '{ready}' ]; do sleep 0.01; done; exit 0"
    )
}

#[tokio::test]
async fn terminate_all_kills_grandchild_after_the_exit_is_recorded() {
    // Note what is *not* widened: the drain grace stays at its 50ms default, so
    // the waiter times out and records the sticky exit. That is the whole
    // difference from T5.
    let supervisor = InProcessProcSupervisor::start_with_grace(LONG_RECLAIM_GRACE)
        .await
        .expect("start supervisor");
    let proc_id = "pty-grandchild-after-exit";

    let scratch = tempfile::tempdir().expect("tempdir");
    let pid_file = scratch.path().join("grandchild.pid");
    let leader = ensure_pty(
        supervisor.sock(),
        proc_id,
        &[
            "-c",
            &grandchild_script(&scratch.path().join("ready"), &pid_file),
        ],
    )
    .await;
    let grandchild = await_pid_file(&pid_file, Duration::from_secs(10)).await;
    let _reaper = KillOnDrop(grandchild);

    assert_ne!(
        grandchild, leader,
        "fixture must produce a distinct grandchild, not the leader itself"
    );
    // Degeneracy self-check, same as T5's: the mechanism under test is "signal
    // the *group*", so a grandchild in a different group would make this test
    // measure nothing.
    assert_eq!(
        process_group_of(grandchild),
        Some(leader),
        "grandchild {grandchild} must share the leader's process group ({leader})"
    );

    // --- Establish "the exit has been recorded". --------------------------
    assert!(
        poll_until(Duration::from_secs(10), || supervisor
            .registry()
            .debug_entry_stats(proc_id)
            .is_some_and(|stats| stats.exit_recorded))
        .await,
        "the sticky exit was never recorded; this test never reached the state \
         it exists to cover (the newly-covered one)"
    );
    // ...and that the entry is still registered. Guaranteed by `removable`'s
    // `eof_reached` condition, but asserted rather than assumed — if it ever
    // stops holding, everything below is vacuous.
    assert!(
        supervisor.registry().debug_entry_stats(proc_id).is_some(),
        "the entry must still be registered: a grandchild holds the slave, so \
         the master never EOFs and `removable` can never be satisfied"
    );
    assert!(
        alive(grandchild),
        "grandchild {grandchild} must still be alive — otherwise this test would \
         pass without terminate_all doing anything"
    );

    // --- Act --------------------------------------------------------------
    supervisor.registry().terminate_all_process_groups_sync();

    assert!(
        await_death(grandchild, Duration::from_secs(5)),
        "grandchild {grandchild} survived terminate_all_process_groups_sync after \
         the exit was recorded — the `exit.is_none()` filter is back in \
         `terminate_all_process_groups_sync`, and with it the #993 grandchild \
         leak in the state the #1013 pin made safe"
    );
}

async fn await_pid_file(path: &Path, budget: Duration) -> u32 {
    let deadline = Instant::now() + budget;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the fixture never wrote its grandchild pid to {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// **`async` and `tokio::time::sleep` on purpose**: `#[tokio::test]` runs on a
/// current-thread runtime, so a blocking poll here would also block the
/// supervisor's serve loop and the condition could never become true.
async fn poll_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

/// A `sleep 300` must never outlive this test, not even when an assertion
/// panics before the terminate_all call.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0 as libc::pid_t, libc::SIGKILL);
        }
    }
}

async fn ensure_pty(sock: &Path, proc_id: &str, args: &[&str]) -> u32 {
    let mut stream = UnixStream::connect(sock).await.expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: "/bin/sh".into(),
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

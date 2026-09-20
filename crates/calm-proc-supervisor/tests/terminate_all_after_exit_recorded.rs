//! Shutdown group-SIGTERMs an entry whose sticky exit is already recorded: under the pin its pgid is still ours, and a grandchild outliving a recorded exit must not leak.
//! The state is established by polling durable conditions, not by sleeping: the grandchild holds the slave so the drain wait times out and `eof_reached` stays false.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::net::UnixStream;

mod proc_probe;
use proc_probe::{alive, await_death, process_group_of};

/// Long enough that the entry is nowhere near the sweeper, short enough that a wedged run still ends; the real guard is `eof_reached` staying false.
const LONG_RECLAIM_GRACE: Duration = Duration::from_secs(60);

/// A backgrounded subshell that ignores SIGHUP (the kernel HUPs the foreground group when the session leader exits), announces readiness through a file, then holds the slave open forever.
/// The pid goes through a file, not the pty: with the 50ms drain grace `Exited` can land before a stream parse finishes.
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
    // The drain grace stays at its 50ms default so the waiter times out and records the sticky exit.
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
    // Degeneracy self-check: the mechanism under test is "signal the group", so a grandchild in a different group would measure nothing.
    assert_eq!(
        process_group_of(grandchild),
        Some(leader),
        "grandchild {grandchild} must share the leader's process group ({leader})"
    );

    // Establish "the exit has been recorded".
    assert!(
        poll_until(Duration::from_secs(10), || supervisor
            .registry()
            .debug_entry_stats(proc_id)
            .is_some_and(|stats| stats.exit_recorded))
        .await,
        "the sticky exit was never recorded; this test never reached the state \
         it exists to cover (the newly-covered one)"
    );
    // ...and that the entry is still registered — asserted rather than assumed, or everything below is vacuous.
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

/// `async` and `tokio::time::sleep` on purpose: on the current-thread runtime a blocking poll would also block the supervisor's serve loop.
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

/// A `sleep 300` must never outlive this test, not even when an assertion panics first.
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

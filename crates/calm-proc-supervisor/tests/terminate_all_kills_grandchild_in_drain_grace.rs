//! Shutdown still group-SIGTERMs an entry inside the drain grace (exit observed, no sticky exit yet, grandchild holding the slave); narrowing the guard to `pty_running()` reintroduces the grandchild leak.
//! The window is widened to 5s and the state is established by polling, not raced. The fixture ignores SIGHUP (the session leader's exit HUPs the foreground group) and stays in the leader's process group.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

mod proc_probe;
use proc_probe::{alive, await_death, process_group_of};

/// The fixture: a subshell ignores HUP, announces readiness, then `exec sleep 300`; the leader waits for the readiness file before exiting.
fn grandchild_script(ready: &Path) -> String {
    let ready = ready.display();
    format!(
        "(trap '' HUP; : > '{ready}'; exec sleep 300) & echo GC=$!; \
         while [ ! -e '{ready}' ]; do sleep 0.01; done; exit 0"
    )
}

/// Two orders of magnitude above the scheduling work between the exit and our assertions, so being inside the window is a fact, not a coin flip.
const WIDE_DRAIN_GRACE: Duration = Duration::from_secs(5);

#[tokio::test]
async fn terminate_all_kills_grandchild_inside_drain_grace() {
    let supervisor = InProcessProcSupervisor::start_with_drain_grace(WIDE_DRAIN_GRACE)
        .await
        .expect("start supervisor");
    let proc_id = "pty-grandchild-drain";

    // The leader exits immediately; the grandchild inherits the slave fd, so the master never EOFs and the waiter sits in the drain window for the whole grace.
    let scratch = tempfile::tempdir().expect("tempdir");
    let leader = ensure_pty(
        supervisor.sock(),
        proc_id,
        "/bin/sh",
        &["-c", &grandchild_script(&scratch.path().join("ready"))],
    )
    .await;
    // `GC=` may already be in the attach replay rather than a later `Output` frame; seed the parser with the replay.
    let (mut attach, replay) = attach(supervisor.sock(), proc_id).await;
    let grandchild = read_grandchild_pid(&mut attach, replay).await;
    // Never leak a `sleep 300`, on any exit path.
    let _reaper = KillOnDrop(grandchild);

    assert_ne!(
        grandchild, leader,
        "fixture must produce a distinct grandchild, not the leader itself"
    );
    // The mechanism under test is "signal the group"; a grandchild outside the leader's group would measure nothing.
    assert_eq!(
        process_group_of(grandchild),
        Some(leader),
        "grandchild {grandchild} must share the leader's process group ({leader})"
    );

    // Establish case 2, do not assume it: poll the entry's own `exit_observed` bit. A pid probe cannot work under the pin (`kill(zombie, 0) == 0` forever), and a `/proc` state probe would report the zombie as dead too early.
    assert!(
        await_exit_observed(supervisor.registry(), proc_id, Duration::from_secs(5)).await,
        "the waiter never observed leader {leader}'s exit; the test never reached \
         the drain window it exists to cover"
    );
    assert!(
        alive(grandchild),
        "grandchild {grandchild} must be alive — otherwise this test would pass \
         without terminate_all doing anything"
    );
    // Still inside the window: no sticky exit yet. This is what separates case 2 from case 3.
    let stats = supervisor
        .registry()
        .debug_entry_stats(proc_id)
        .expect("entry still registered inside the drain grace");
    assert!(
        !stats.exit_recorded,
        "expected to be INSIDE the drain grace (no sticky exit yet) — with a {}s \
         grace this should not be reachable; the test would otherwise be \
         exercising case 3, which is a different contract",
        WIDE_DRAIN_GRACE.as_secs()
    );

    // The same call `main.rs` makes on shutdown, invoked directly so the assertion observes it rather than `Drop` ordering.
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

/// Polls until the waiter has observed the leader's exit.
async fn await_exit_observed(
    registry: &calm_proc_supervisor::ProcRegistry,
    proc_id: &str,
    budget: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if registry
            .debug_entry_stats(proc_id)
            .is_some_and(|stats| stats.exit_observed)
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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

/// Returns `None` until the terminator has been seen, so a chunk boundary mid-number cannot truncate the pid.
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

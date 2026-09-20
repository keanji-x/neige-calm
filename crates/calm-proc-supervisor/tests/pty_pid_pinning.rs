//! The pty leader's pid is pinned (retained as a zombie) for the entry's whole registry lifetime, and released exactly when the entry is.
//! The pinned/released cases must exist as a pair; every counter read is the registry-scoped `debug_pin_count()`, because the cases run as threads of one process.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProcSignal, SignalRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::net::UnixStream;

/// Anti-hang bound for a single frame read. Nothing asserts promptness against
/// it; it only stops a broken build from hanging a CI runner.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

/// The exit is established, not raced: `Signal(Kill)` then poll until `Exited { signalled: true }`; the main assertions describe that one instant.
#[tokio::test]
async fn pty_pid_stays_pinned_while_the_entry_is_registered() {
    // Default 60s reclaim grace on purpose: the assertions must not be racing a sweep.
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-pin-held";

    // `exec` so the pid we are handed is the `sleep`'s own, not an intermediate shell.
    let leader = ensure_pty(supervisor.sock(), proc_id, &["-c", "exec sleep 600"]).await;

    // Degeneracy self-check: the child really started, so "state == Z" below cannot pass for the wrong reason.
    let state = await_proc_state_in(leader, &["S", "R"], Duration::from_secs(5)).await;
    assert!(
        state.is_some(),
        "leader {leader} never reached a running state; /proc state was {:?}",
        proc_state(leader)
    );

    // The counter counts handles, not observed exits: asserted while the leader is still running, the only place the two definitions disagree.
    assert_eq!(
        supervisor.registry().debug_pin_count(),
        1,
        "the registry must already own one un-reaped leader handle while the \
         leader is still running — a live leader occupies an RLIMIT_NPROC slot \
         exactly like a zombie one, so counting from the exit measures the \
         wrong quantity"
    );

    let mut attach = attach(supervisor.sock(), proc_id).await;
    signal(supervisor.sock(), proc_id, ProcSignal::Kill).await;
    await_exited(&mut attach, true).await;

    // Main assertions: one instant, three views of it.
    assert_eq!(
        proc_state(leader).as_deref(),
        Some("Z"),
        "leader {leader} must still be present as a zombie: the waiter observes \
         the exit with waitid(.., WNOWAIT) and must not reap. If /proc/{leader} \
         is gone, the pid has been returned to the allocator while the entry is \
         still registered — that is #1013"
    );
    assert_eq!(
        unsafe { libc::kill(-(leader as libc::pid_t), 0) },
        0,
        "the leader's process group must still be addressable ({leader})"
    );
    assert!(
        supervisor.registry().debug_entry_stats(proc_id).is_some(),
        "the entry must still be registered — the pin's whole claim is that it \
         outlives every signal path that can still reach this entry"
    );
}

/// The pin is a lifetime, not a leak.
#[tokio::test]
async fn the_pinned_pid_is_released_when_the_entry_is_removed() {
    let supervisor = InProcessProcSupervisor::start_with_grace(Duration::from_millis(50))
        .await
        .expect("start supervisor");
    let proc_id = "pty-pin-released";
    let leader = ensure_pty(supervisor.sock(), proc_id, &["-c", "exit 3"]).await;

    let mut attach = attach(supervisor.sock(), proc_id).await;
    await_exited(&mut attach, false).await;
    drop(attach);

    // Registry release first — necessary, but on its own it proves nothing about the reap.
    assert!(
        poll_until(Duration::from_secs(20), || supervisor
            .registry()
            .debug_entry_stats(proc_id)
            .is_none())
        .await,
        "entry was never swept out of the registry"
    );

    // Poll for the reap rather than asserting immediately: when the registry lets go, the reader thread and the sweeper's `doomed` vec still hold `Arc`s.
    // The conjunction is deliberate: `/proc/<pid>` alone is ambiguous because a recycled pid makes the directory reappear.
    assert!(
        poll_until(Duration::from_secs(5), || {
            supervisor.registry().debug_pin_count() == 0
                && proc_state(leader).as_deref() != Some("Z")
        })
        .await,
        "the leader zombie was never reaped after the entry was removed: \
         pin_count = {}, /proc/{leader} state = {:?}. Drop for ProcEntry is the \
         only reap in the crate; if it does not run, every entry leaks a zombie \
         and an RLIMIT_NPROC slot for the supervisor's lifetime",
        supervisor.registry().debug_pin_count(),
        proc_state(leader)
    );
}

/// `try_spawn_pty` inserts by `proc_id` and overwrites; the displaced entry keeps its own `Arc`s (a reader blocked in `read()` because a grandchild holds the slave), so its leader stays pinned.
/// A counter derived from the map cannot see it. This locks the counter's definition, not a fix for the orphaned entry's leak.
#[tokio::test]
async fn orphaned_entries_are_visible_to_the_pin_counter() {
    let supervisor = InProcessProcSupervisor::start_with_grace(Duration::from_millis(50))
        .await
        .expect("start supervisor");
    let proc_id = "pty-pin-orphan";
    let scratch = tempfile::tempdir().expect("tempdir");
    let pid_file = scratch.path().join("grandchild.pid");

    // First entry: its grandchild keeps the slave open, so the reader thread never returns and never releases its `Arc`.
    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", &grandchild_script(&pid_file)],
    )
    .await;
    let grandchild = await_pid_file(&pid_file, Duration::from_secs(10)).await;
    let _reaper = KillOnDrop(grandchild);
    assert!(
        proc_state(grandchild).is_some_and(|s| s != "Z"),
        "the grandchild must be alive and holding the slave, otherwise the first \
         entry's reader would reach EOF and the entry would not be orphaned"
    );
    assert!(
        poll_until(Duration::from_secs(10), || supervisor
            .registry()
            .debug_entry_stats(proc_id)
            .is_some_and(|s| s.exit_observed))
        .await,
        "the first leader never exited"
    );

    // Second entry, same proc_id: displaces the first in the map.
    ensure_pty(supervisor.sock(), proc_id, &["-c", "exec sleep 600"]).await;

    assert_eq!(
        supervisor.registry().debug_entry_count(),
        1,
        "the respawn must have displaced the first entry, leaving one in the map"
    );
    assert_eq!(
        supervisor.registry().debug_pin_count(),
        2,
        "both entries still own an un-reaped leader handle — the displaced one \
         is invisible to the map but not to the counter"
    );
}

/// A backgrounded subshell that reopens the pty slave (`exec < /dev/tty`) and blocks on it forever.
/// `trap '' HUP` is required: the session leader's exit SIGHUPs the foreground group, and without it the grandchild dies with its parent.
fn grandchild_script(pid_file: &Path) -> String {
    format!(
        "(trap '' HUP; exec < /dev/tty; while read _; do :; done) & \
         printf '%s' \"$!\" > {}; sleep 0.3; exit 0",
        pid_file.display(),
    )
}

struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0 as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// `/proc/<pid>/stat` field 3 (`state`). `comm` may contain spaces and
/// parentheses, so the only safe split point is the **last** `')'`.
fn proc_state(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().next().map(str::to_owned)
}

async fn await_proc_state_in(pid: u32, want: &[&str], budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(state) = proc_state(pid)
            && want.contains(&state.as_str())
        {
            return Some(state);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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

/// `async` and `tokio::time::sleep`, not `std::thread::sleep`: on the current-thread runtime a blocking poll also blocks the supervisor's serve loop and sweeper.
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

async fn attach(sock: &Path, proc_id: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect attach");
    write_frame(
        &mut stream,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: None,
            reader_id: "pin-test".into(),
        }),
    )
    .await
    .expect("write attach");
    match read_frame(&mut stream).await.expect("read attach ok") {
        ControlReply::AttachOk(_) => {}
        other => panic!("unexpected attach reply: {other:?}"),
    }
    stream
}

async fn signal(sock: &Path, proc_id: &str, sig: ProcSignal) {
    let mut stream = UnixStream::connect(sock).await.expect("connect signal");
    write_frame(
        &mut stream,
        &ControlMsg::Signal(SignalRequest {
            proc_id: proc_id.into(),
            sig,
        }),
    )
    .await
    .expect("write signal");
    match timeout_read(&mut stream).await {
        ControlReply::SignalOk => {}
        other => panic!("expected SignalOk, got {other:?}"),
    }
}

async fn await_exited(stream: &mut UnixStream, want_signalled: bool) {
    loop {
        match timeout_read(stream).await {
            ControlReply::Exited { signalled, .. } => {
                assert_eq!(signalled, want_signalled, "unexpected exit shape");
                return;
            }
            ControlReply::Output { .. } => {}
            other => panic!("unexpected attach frame before exit: {other:?}"),
        }
    }
}

async fn timeout_read(stream: &mut UnixStream) -> ControlReply {
    tokio::time::timeout(LIVENESS_BUDGET, read_frame(stream))
        .await
        .expect("timed out reading reply")
        .expect("read reply")
}

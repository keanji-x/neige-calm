//! #1013 T1/T2/T10 — the pty leader's pid is pinned for the entry's whole
//! registry lifetime, and released exactly when the entry is.
//!
//! The defect: `spawn_pty_waiter` used to `child.wait()`, which reaps. The
//! instant it returned, the leader's pid — and the pgid numerically equal to it
//! — went back into the kernel's allocator, while the entry stayed in the
//! registry for at least `PTY_RECLAIM_GRACE` (60s), or *unboundedly* when a
//! grandchild holds the slave. Every signal path that resolves to
//! `entry.pid` was aiming at a number that could already belong to someone
//! else. Retaining the zombie is the only mechanism that keeps a pid number
//! out of the allocator — no pidfd, tty reference or other handle does it.
//!
//! **T1 and T2 must exist as a pair.** T1 alone would pass an implementation
//! that pins forever and leaks; T2 alone would pass one that never pins at all.
//!
//! **What this pair does *not* prove**, stated plainly so nobody reads more
//! into it: it says nothing about Pipe entries (their child belongs to tokio
//! and is not pinned — see `pipe_procs_are_not_signalable.rs`), nothing about
//! the `tcgetpgrp` foreground-group target (a different process group's number,
//! which the leader's zombie cannot pin — #1013's remaining face), nothing
//! about a wildcard `wait` stealing the zombie (that is the
//! `no_wildcard_wait_in_the_supervisor_host` lint), nothing about `SIGCHLD`
//! being made auto-reaping after spawn (`pin_lost_on_autoreap.rs`), and nothing
//! about a waiter that panics (the `WaiterCompletion` guard plus typed
//! ownership). All five stay green here.
//!
//! Every counter read here is `debug_pin_count()`, which is **registry-scoped**
//! on purpose. `cargo test` runs the cases in this file as threads of one
//! process; a process-global counter would make T1's `== 1`, T2's `-> 0` and
//! T10's `== 2` redden each other deterministically. If someone moves the
//! counter to a `static`, these three go red together — that coupling is
//! intentional.

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

/// T1 — while the entry is registered, the leader's pid is still ours.
///
/// The exit is *established*, not raced: we send `Signal(Kill)` and poll the
/// attach stream until `Exited { signalled: true }` arrives. Only then do the
/// three main assertions run, and all three describe the same instant.
#[tokio::test]
async fn pty_pid_stays_pinned_while_the_entry_is_registered() {
    // Default 60s reclaim grace on purpose: the assertions must not be racing a
    // sweep, and 60s against a handful of polls is three orders of magnitude of
    // headroom.
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-pin-held";

    // `exec` so the pid we are handed is the `sleep`'s own — no intermediate
    // shell that could exit on its own and make the test vacuous.
    let leader = ensure_pty(supervisor.sock(), proc_id, &["-c", "exec sleep 600"]).await;

    // Degeneracy self-check: the child really started. Without it, a spawn
    // failure would leave a pid that was never alive and the "state == Z"
    // assertion below could pass for the wrong reason.
    let state = await_proc_state_in(leader, &["S", "R"], Duration::from_secs(5)).await;
    assert!(
        state.is_some(),
        "leader {leader} never reached a running state; /proc state was {:?}",
        proc_state(leader)
    );

    // --- The M3 lock: the counter counts *handles*, not *observed exits*. ---
    //
    // Asserted here, while the leader is still running, precisely because that
    // is the only place where the two definitions disagree. Under the rejected
    // "increment when the waiter observes an exit" definition this is 0.
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

    // --- Main assertions: one instant, three views of it. -----------------
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

/// T2 — and it is released when the entry goes away. The pin is a lifetime,
/// not a leak.
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

    // Registry release first — necessary, but on its own it proves nothing
    // about the reap.
    assert!(
        poll_until(Duration::from_secs(20), || supervisor
            .registry()
            .debug_entry_stats(proc_id)
            .is_none())
        .await,
        "entry was never swept out of the registry"
    );

    // Then poll for the reap itself, rather than asserting it immediately.
    // `debug_entry_stats() == None` only means the registry let go; at that
    // instant the reader thread and the sweeper's `doomed` vec still hold their
    // own `Arc`s, so `Drop for ProcEntry` has not run yet. An immediate
    // assertion here would make the test that locks the invariant into a flake
    // — and a flake gets its budget widened, not its bug fixed.
    //
    // The conjunction is deliberate. `pin_count` is our own definite state;
    // `/proc/<pid>` alone is ambiguous because a recycled pid makes the
    // directory reappear. Neither is sufficient alone.
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

/// T10 — the counter sees entries that have left the registry.
///
/// `try_spawn_pty` inserts by `proc_id` and **overwrites**. The displaced entry
/// leaves the map but keeps its own `Arc`s (here: a reader thread blocked
/// forever in `read()`, because a grandchild still holds the slave), so it is
/// never dropped and its leader stays pinned. A counter defined as "entries in
/// the map with such-and-such bits set" cannot see it, and rebuilding
/// `term:<id>` N times would leak N permanent zombies with the threshold WARN
/// never firing.
///
/// One honest limit: this locks the *counter's definition*, not a fix — the
/// orphaned entry's own leak is #996's business.
///
/// It *does* lock M3's `fetch_add` placement, contrary to what this comment
/// claimed before: the second entry below is a still-running `exec sleep 600`,
/// so moving the `fetch_add` from handle-install to exit-observed turns the
/// main assertion red (`left: 1, right: 2`), measured. T1's live-leader
/// assertion is the other lock on that placement.
#[tokio::test]
async fn orphaned_entries_are_visible_to_the_pin_counter() {
    let supervisor = InProcessProcSupervisor::start_with_grace(Duration::from_millis(50))
        .await
        .expect("start supervisor");
    let proc_id = "pty-pin-orphan";
    let scratch = tempfile::tempdir().expect("tempdir");
    let pid_file = scratch.path().join("grandchild.pid");

    // First entry: its grandchild keeps the slave open, so the reader thread
    // never sees EOF, never returns, and never releases its `Arc`.
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

/// The fixture from `pty_entry_reclaim.rs`: a backgrounded subshell that
/// reopens the pty slave (`exec < /dev/tty`, since a non-interactive shell
/// redirects a background job's stdin to /dev/null) and blocks on it forever.
/// `trap '' HUP` is required — the pty child is the session leader, so the
/// kernel SIGHUPs the foreground group when it exits, and without the trap the
/// grandchild would die with its parent and the master would EOF immediately.
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

/// Polls a condition with a deadline. **`async` and `tokio::time::sleep`, not
/// `std::thread::sleep`**: `#[tokio::test]` gives a current-thread runtime, so
/// blocking this thread also blocks the supervisor's serve loop — including its
/// periodic sweeper. A blocking poll for "the entry was swept" can therefore
/// never succeed; it deadlocks and then fails on its own deadline.
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

//! Repro for #993: the pty reader thread and the waitpid thread are not
//! synchronised, so `child.wait()` can win the race against draining the pty
//! master. Two observable invariants break:
//!
//! A. `Exited` must be the last frame on an attach stream — every byte the
//!    process wrote must reach an online attacher *before* `Exited`.
//! B. `exit.cursor` must equal the process' real final byte count.
//!
//! The A/B tests make the race deterministic by having the child burst a large
//! blob and exit immediately afterwards, with the whole test (and every thread
//! it spawns) pinned to a single CPU — see `pin_to_single_cpu`.
//!
//! Two more cover the degraded path, where a surviving grandchild holds the pty
//! slave open so the master never EOFs and the drain gate has to time out:
//!
//! C. the ring seal still makes `Exited` final
//!    (`exited_is_final_when_a_grandchild_holds_the_pty_open`);
//! D. the seal stops *publishing* but not *reading*, so the grandchild is not
//!    wedged on a full tty queue
//!    (`the_reader_keeps_draining_the_master_after_the_seal`).
//!
//! Neither degraded-path test asserts on elapsed time: both prove they are on
//! the degraded path by checking that the slave holder is still alive/making
//! progress, which is causal rather than schedule-dependent.
//!
//! Linux-only: `pin_to_single_cpu` needs `sched_setaffinity`/`cpu_set_t`, which
//! `libc` only exposes on Linux, and the degraded-path test relies on Linux pty
//! semantics. The gate is at crate root deliberately: on a non-Linux target the
//! whole test binary compiles away to zero tests rather than reporting a stub
//! test as green. Mirrors the `#[cfg(target_os = "linux")]` /
//! `#[cfg(all(unix, not(target_os = "linux")))]` split in `src/lib.rs`.
#![cfg(target_os = "linux")]

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, WriteStdinRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

/// Liveness upper bound for "read the next frame / wait for the expected
/// state". Anti-hang guard only — no case here claims the supervisor reacts
/// within this budget, so a slow-but-correct run must still pass. Costs
/// nothing on the happy path (each wait returns as soon as its frame lands).
/// The 1-2s budgets this replaces are the same shape that flaked on CI's
/// 2-core runner under `retries = 0`. 120s is the `slow-timeout` of nextest
/// `profile.ci`; the local `profile.default` warns at 60s. Both are warn-only,
/// so neither kills the test — past this point nextest's slow-test report is the
/// signal, not a hand-picked deadline.
/// To assert promptness, measure elapsed and assert on it instead.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

/// Budget for "the supervisor closes the attach stream **right after** it sent
/// `Exited`".
///
/// Deliberately *not* `LIVENESS_BUDGET`: promptness is the contract here, not
/// an anti-hang guard. Closing the stream is the last statement of the same
/// arm that publishes `Exited`, so a correct supervisor closes within
/// microseconds and this budget is three orders of magnitude of headroom.
///
/// Do not widen it back to `LIVENESS_BUDGET`. Mutation-verified by making the
/// attach handler `continue` instead of returning after it writes `Exited`:
/// at 2s the failure lands in 2.27s, at 120s the same regression takes the
/// full 120s to report. (Both budgets do catch that mutation today — the
/// attach handler parks on `rx.recv()` and the registry keeps a broadcast
/// sender alive, so the connection does *not* drop by itself when the last
/// slave holder dies. That is an implementation detail of the current
/// supervisor, not a property this test may lean on: the moment an idle attach
/// stream gains any independent way to end — an idle reaper, entry eviction,
/// a client-side timeout — a budget longer than the fixture's lifetime silently
/// degrades `after.is_err()` into "we waited for the process to finish", which
/// a supervisor that never closes the stream also satisfies. In
/// `exited_is_final_when_a_grandchild_holds_the_pty_open` the slave holder
/// lives only ~5s while `Exited` lands ~0.25s in.)
///
/// If a fixture here ever needs to outlive this budget, keep the budget short
/// and lengthen the fixture — never the other way round.
const EXITED_STREAM_CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// Bytes of filler the child bursts out right before exiting.
const BURST: usize = 200_000;
const SENTINEL: &str = "END-OF-STREAM-993";
const REPLAY_BYTES: usize = 8 * 1024 * 1024;

/// `read x` parks the child until we write stdin, so an attacher is guaranteed
/// to be online before the burst starts.
fn burst_script() -> String {
    format!("read x; printf '%0{BURST}d' 0; printf '\\n{SENTINEL}\\n'; exit 0")
}

/// Confine this thread — and therefore every thread/process it later spawns,
/// including the supervisor's pty reader thread, its waitpid thread and the
/// pty child — to a single CPU.
///
/// The reader and the waiter are woken microseconds apart, so on an idle
/// many-core box the reader always wins and the (real) race stays invisible;
/// under CPU contention — exactly what CI runners have — the waiter wins and
/// stamps a stale cursor. Pinning turns that contention into a property of the
/// test instead of a property of the machine. It changes scheduling only: a
/// supervisor that drains the pty before publishing the exit passes with or
/// without the pin.
fn pin_to_single_cpu() {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(
            libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set),
            0,
            "sched_getaffinity failed"
        );
        let cpu = (0..libc::CPU_SETSIZE as usize)
            .find(|cpu| libc::CPU_ISSET(*cpu, &set))
            .expect("no CPU in the affinity mask");
        let mut one: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut one);
        libc::CPU_SET(cpu, &mut one);
        assert_eq!(
            libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &one),
            0,
            "sched_setaffinity failed"
        );
    }
}

/// Invariant A: no output may be lost behind `Exited` on a live attach stream.
#[tokio::test]
async fn exited_is_the_last_frame_for_a_live_attacher() {
    pin_to_single_cpu();
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-exit-order-live";
    ensure_pty(supervisor.sock(), proc_id, &burst_script()).await;

    let mut attach = attach(supervisor.sock(), proc_id).await;
    let mut seen = match timeout_read(&mut attach).await {
        ControlReply::AttachOk(attached) => attached.replay,
        other => panic!("unexpected attach reply: {other:?}"),
    };

    write_stdin(supervisor.sock(), proc_id, b"go\n").await;

    let exit_cursor = loop {
        match timeout_read(&mut attach).await {
            ControlReply::Output { bytes, .. } => seen.extend(bytes),
            ControlReply::Exited { cursor, status, .. } => {
                assert_eq!(status, Some(0), "child should exit cleanly");
                break cursor;
            }
            other => panic!("unexpected attach frame: {other:?}"),
        }
    };

    assert!(
        contains(&seen, SENTINEL.as_bytes()),
        "issue #993: the live attacher received Exited (cursor {exit_cursor}) before the \
         process' trailing output; it saw only {} bytes and the final sentinel {:?} never \
         arrived (expected at least {} bytes)",
        seen.len(),
        SENTINEL,
        BURST,
    );

    // `Exited` is terminal by construction (the supervisor closes the stream),
    // so nothing may follow it.
    let after: Result<ControlReply, _> =
        tokio::time::timeout(EXITED_STREAM_CLOSE_BUDGET, read_frame(&mut attach))
            .await
            .expect("timed out waiting for stream close after Exited");
    assert!(
        after.is_err(),
        "expected the attach stream to end after Exited, got {after:?}",
    );
}

/// Invariant B: `exit.cursor` must be the process' real final byte count.
#[tokio::test]
async fn exit_cursor_equals_final_byte_count() {
    pin_to_single_cpu();
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-exit-order-cursor";
    ensure_pty(supervisor.sock(), proc_id, &burst_script()).await;

    write_stdin(supervisor.sock(), proc_id, b"go\n").await;

    // Let the child exit and the reader thread drain the master to EOF, so the
    // ring provably holds every byte the process ever wrote.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // A fresh attacher replays the whole ring and then gets the sticky exit.
    let mut attach = attach(supervisor.sock(), proc_id).await;
    let replay = match timeout_read(&mut attach).await {
        ControlReply::AttachOk(attached) => attached.replay,
        other => panic!("unexpected attach reply: {other:?}"),
    };
    let mut total = replay.len() as u64;
    let exit_cursor = loop {
        match timeout_read(&mut attach).await {
            ControlReply::Output { bytes, .. } => total += bytes.len() as u64,
            ControlReply::Exited { cursor, .. } => break cursor,
            other => panic!("unexpected attach frame: {other:?}"),
        }
    };

    assert!(
        total >= BURST as u64,
        "sanity: expected the ring to hold the whole burst, got {total} bytes",
    );
    assert_eq!(
        exit_cursor,
        total,
        "issue #993: exit.cursor under-reports the process' final byte position \
         (exit.cursor = {exit_cursor}, real final byte count = {total}, short by {})",
        total.saturating_sub(exit_cursor),
    );
}

/// Printed by the parent right before it exits; the grandchild never prints it.
const PARENT_SENTINEL: &str = "PARENT-DONE-993";

/// The parent backgrounds a subshell that inherits the pty slave fds and keeps
/// writing for ~5s, then the parent exits. The pty master therefore never
/// EOFs, so the waiter's drain gate must time out — this is the degraded path.
///
/// `trap '' HUP` is load-bearing: the pty child is the session leader, so the
/// kernel SIGHUPs the whole foreground process group when it exits. Without the
/// trap the grandchild dies with its parent, the master EOFs and the test
/// silently degenerates into another happy-path case. The parent's `sleep 0.2`
/// closes the fork/trap race — without it the parent can exit (and the HUP can
/// land) before the freshly forked subshell has installed the trap.
///
/// The parent writes the subshell's pid to `pid_file` (`$!`, so it is the real
/// pid regardless of how the shell forks) *before* it exits. That file is what
/// the test's degradation self-check reads — see `process_is_alive`.
fn grandchild_script(pid_file: &Path) -> String {
    format!(
        "read x; (trap '' HUP; i=0; while [ $i -lt 100 ]; do printf 'X'; sleep 0.05; \
         i=$((i+1)); done) & printf '%s' \"$!\" > {}; sleep 0.2; \
         printf '\\n{PARENT_SENTINEL}\\n'; exit 0",
        pid_file.display(),
    )
}

/// True when `pid` names a process that still exists and has not become a
/// zombie. Zombies are excluded deliberately: a reaped-but-unwaited process has
/// already closed every fd it held, so it proves nothing about the pty slave.
fn process_is_alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (comm) STATE ...`, and `comm` may itself contain spaces and
    // parentheses — split at the LAST ')' so the state field is unambiguous.
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    matches!(rest.split_whitespace().next(), Some(state) if state != "Z" && state != "X")
}

/// Reads the pid the parent stashed in `pid_file`.
fn read_pid_file(pid_file: &Path) -> i32 {
    let raw = std::fs::read_to_string(pid_file)
        .unwrap_or_else(|e| panic!("read {}: {e}", pid_file.display()));
    raw.trim()
        .parse()
        .unwrap_or_else(|e| panic!("parse pid {raw:?}: {e}"))
}

/// Invariant C (#993 R2, degraded path): when a surviving grandchild holds the
/// pty slave open the master never reaches EOF and the drain gate times out.
/// The waiter must then still make `Exited` final — it seals the byte ring in
/// the same critical section it publishes `Exited` from, so the grandchild's
/// later writes can be neither appended nor broadcast. It must also not wait
/// forever.
///
/// This is the path a plain "sample `cursor_tail`, then broadcast" waiter gets
/// wrong: the reader is still live, so it keeps pushing `Output` past the exit
/// cursor.
#[tokio::test]
async fn exited_is_final_when_a_grandchild_holds_the_pty_open() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("grandchild.pid");
    let proc_id = "pty-exit-order-grandchild";
    ensure_pty(supervisor.sock(), proc_id, &grandchild_script(&pid_file)).await;

    let mut attach = attach(supervisor.sock(), proc_id).await;
    let mut seen = match timeout_read(&mut attach).await {
        ControlReply::AttachOk(attached) => attached.replay,
        other => panic!("unexpected attach reply: {other:?}"),
    };

    write_stdin(supervisor.sock(), proc_id, b"go\n").await;

    // Liveness: the master never EOFs, so `Exited` can only arrive because the
    // grace expires and the waiter publishes anyway. `timeout_read`'s
    // `LIVENESS_BUDGET` is the whole "does not wedge forever" assertion — deliberately
    // the only one (#993 R3-C). A tighter elapsed-time bound derived from when
    // this *task* happened to observe the parent's sentinel is not safe in
    // either direction: a CI pause between reading the sentinel and reading
    // `Exited` inflates the measured gap without the supervisor doing anything
    // wrong, which is a false failure in a PR whose whole point is killing
    // flake. The degradation self-check below is causal and covers what the
    // bound was reaching for.
    let exit_cursor = loop {
        match timeout_read(&mut attach).await {
            ControlReply::Output { bytes, .. } => {
                seen.extend(bytes);
            }
            ControlReply::Exited { cursor, status, .. } => {
                assert_eq!(status, Some(0), "parent should exit cleanly");
                break cursor;
            }
            other => panic!("unexpected attach frame: {other:?}"),
        }
    };
    // Degradation self-check: this test is only meaningful if the drain gate
    // actually timed out, i.e. the master never EOFed. Proving that with a
    // *duration* would be a race — `sentinel_at` is when the TEST TASK was
    // scheduled to read the sentinel frame (parent write → reader thread →
    // append+broadcast → attach handler task → socket → this task: 3-4 wakeups),
    // while the reap path needs one, so on a contended CI runner the measured
    // gap can legitimately fall below the 50ms grace even on the degraded path.
    //
    // Instead we check the *cause* directly and monotonically: the grandchild
    // is the only holder of the pty slave fd, so if it is still alive here —
    // strictly after `Exited` was published, which is strictly after the gate
    // resolved — then it was alive when the gate ran, and the master provably
    // could not have EOFed. No timing assumption at all. (It sleeps ~5s total,
    // and `Exited` lands ~0.25s in.)
    //
    // The parent's trailing sentinel must also have arrived before `Exited` —
    // that is invariant A on the degraded path, and unlike a duration it is
    // schedule-independent.
    let grandchild_pid = read_pid_file(&pid_file);
    assert!(
        process_is_alive(grandchild_pid),
        "this test must exercise the DEGRADED path, but the grandchild (pid \
         {grandchild_pid}) was already gone when Exited arrived — it did not keep the \
         pty slave fd open, so the master EOFed and the drain gate never timed out",
    );

    assert!(
        contains(&seen, PARENT_SENTINEL.as_bytes()),
        "the parent's trailing output must arrive before Exited",
    );

    assert_eq!(
        seen.len() as u64,
        exit_cursor,
        "exit.cursor must account for exactly the bytes delivered before Exited",
    );

    // `Exited` is terminal: the supervisor closes the stream after it.
    let after: Result<ControlReply, _> =
        tokio::time::timeout(EXITED_STREAM_CLOSE_BUDGET, read_frame(&mut attach))
            .await
            .expect("timed out waiting for stream close after Exited");
    assert!(
        after.is_err(),
        "expected the attach stream to end after Exited, got {after:?}",
    );

    // The seal is the real assertion: the grandchild is still writing 'X' every
    // 50ms, but the ring must be frozen at the exit cursor forever. Without the
    // seal the reader keeps appending and this tail keeps growing.
    let tail_at_exit = attach_tail(supervisor.sock(), proc_id).await;
    assert_eq!(
        tail_at_exit, exit_cursor,
        "ring tail right after Exited must equal exit.cursor",
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    let tail_later = attach_tail(supervisor.sock(), proc_id).await;
    assert_eq!(
        tail_later,
        exit_cursor,
        "issue #993 R2: the ring grew by {} bytes after Exited — the grandchild's \
         output is still being appended/broadcast, so Exited was not the last frame",
        tail_later.saturating_sub(exit_cursor),
    );
}

/// Bytes the grandchild bursts *after* the ring is sealed. Must comfortably
/// exceed the kernel's pty buffer (~64 KiB) so an unread master would block the
/// writer instead of merely buffering it.
const POST_SEAL_BURST: usize = 300_000;

/// The kernel's tty queue is ~64 KiB, so anything at or above this really did
/// have to be *drained* by the reader rather than merely buffered.
const KERNEL_TTY_QUEUE: usize = 64 * 1024;

/// Same shape as `grandchild_script`, but the grandchild stays quiet until well
/// past the seal and then bursts `POST_SEAL_BURST` bytes into the slave before
/// touching `done_file`.
///
/// The marker is the *actual* byte count it managed to push (`${#blob}`), not a
/// fixed `done` string (#993 R3-E): a shell whose `printf '%0Nd'` truncates or
/// fails would still have created a constant marker and turned this test green
/// without ever filling the kernel queue. The count is built first and written
/// after the burst, so it can only appear once the tty accepted every byte.
fn post_seal_writer_script(done_file: &Path) -> String {
    format!(
        "read x; (trap '' HUP; sleep 0.6; blob=$(printf '%0{POST_SEAL_BURST}d' 0); \
         printf '%s' \"$blob\"; printf '%s' ${{#blob}} > {}) & sleep 0.2; \
         printf '\\n{PARENT_SENTINEL}\\n'; exit 0",
        done_file.display(),
    )
}

/// Reads the byte count the post-seal grandchild published, or `None` while the
/// file is still absent/partial.
fn read_written_bytes(done_file: &Path) -> Option<usize> {
    let raw = std::fs::read_to_string(done_file).ok()?;
    raw.trim().parse().ok()
}

/// Invariant D (#993 R3): sealing the ring must stop *publishing*, not
/// *reading*.
///
/// The entry owns the pty master for the process' whole lifetime and no
/// production code path sends `ControlMsg::Cleanup`, so if the reader thread
/// stopped at the seal the master would stay open and unread forever. A
/// surviving grandchild that still holds the slave would then fill the kernel's
/// ~64 KiB tty queue and block inside `write()` for good — a hang that did not
/// exist before the seal was introduced.
///
/// The grandchild here bursts 300 KiB well after the seal and only then writes
/// its done-marker — which carries the *number of bytes it actually pushed*, so
/// a truncated burst fails the test instead of silently passing it. The marker
/// appearing proves the master is still being drained; the frozen ring tail
/// proves those bytes were discarded rather than published, i.e. that the burst
/// really landed on the post-seal path.
#[tokio::test]
async fn the_reader_keeps_draining_the_master_after_the_seal() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let temp = tempfile::tempdir().expect("tempdir");
    let done_file = temp.path().join("grandchild.done");
    let proc_id = "pty-exit-order-post-seal-drain";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        &post_seal_writer_script(&done_file),
    )
    .await;

    let mut attach = attach(supervisor.sock(), proc_id).await;
    match timeout_read(&mut attach).await {
        ControlReply::AttachOk(_) => {}
        other => panic!("unexpected attach reply: {other:?}"),
    }
    write_stdin(supervisor.sock(), proc_id, b"go\n").await;

    let exit_cursor = loop {
        match timeout_read(&mut attach).await {
            ControlReply::Output { .. } => {}
            ControlReply::Exited { cursor, status, .. } => {
                assert_eq!(status, Some(0), "parent should exit cleanly");
                break cursor;
            }
            other => panic!("unexpected attach frame: {other:?}"),
        }
    };

    // The grandchild has not started its burst yet (it sleeps 0.6s, the seal
    // lands ~0.25s in), so this is the sealed tail.
    assert_eq!(
        attach_tail(supervisor.sock(), proc_id).await,
        exit_cursor,
        "ring tail right after Exited must equal exit.cursor",
    );

    // The marker is written only after all `POST_SEAL_BURST` bytes have been
    // accepted by the tty. With a reader that stops at the seal, the grandchild
    // wedges in `write()` after ~64 KiB and this never appears.
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    let written = loop {
        if let Some(written) = read_written_bytes(&done_file) {
            break written;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "issue #993 R3: the grandchild never finished writing {POST_SEAL_BURST} bytes \
             after the exit seal — the supervisor stopped reading the pty master, so the \
             surviving grandchild is blocked in write() on a full tty queue",
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    // ...and it really was a burst: a marker written after a truncated or
    // failed `printf` would prove nothing, because anything below the kernel's
    // tty queue fits in the buffer whether or not the reader kept draining.
    assert!(
        written >= KERNEL_TTY_QUEUE,
        "the post-seal burst must exceed the kernel tty queue ({KERNEL_TTY_QUEUE} bytes) \
         for this test to prove anything about draining, but the grandchild only wrote \
         {written} bytes (expected {POST_SEAL_BURST})",
    );

    // ...and the drained bytes were discarded, not published: had they been
    // appended, the tail would have grown by ~300 KiB. This is also the proof
    // that the burst really landed after the seal.
    assert_eq!(
        attach_tail(supervisor.sock(), proc_id).await,
        exit_cursor,
        "the post-seal burst must be discarded, not appended: the ring is frozen at \
         exit.cursor forever",
    );
}

/// Attaches once and reports the ring's current `cursor_tail`.
async fn attach_tail(sock: &Path, proc_id: &str) -> u64 {
    let mut stream = attach(sock, proc_id).await;
    match timeout_read(&mut stream).await {
        ControlReply::AttachOk(attached) => attached.cursor_tail,
        other => panic!("unexpected attach reply: {other:?}"),
    }
}

async fn ensure_pty(sock: &Path, proc_id: &str, script: &str) {
    let mut stream = UnixStream::connect(sock).await.expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            envs: Vec::new(),
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: REPLAY_BYTES,
        }),
    )
    .await
    .expect("write ensure");
    match read_frame(&mut stream).await.expect("read spawned") {
        ControlReply::Spawned { .. } => {}
        other => panic!("unexpected first reply: {other:?}"),
    }
    match read_frame(&mut stream).await.expect("read ready") {
        ControlReply::Ready => {}
        other => panic!("unexpected second reply: {other:?}"),
    }
}

async fn attach(sock: &Path, proc_id: &str) -> UnixStream {
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
    stream
}

async fn write_stdin(sock: &Path, proc_id: &str, bytes: &[u8]) {
    let mut control = UnixStream::connect(sock).await.expect("connect control");
    write_frame(
        &mut control,
        &ControlMsg::WriteStdin(WriteStdinRequest {
            proc_id: proc_id.into(),
            bytes: bytes.to_vec(),
            write_seq: Some(1),
        }),
    )
    .await
    .expect("write stdin");
    match timeout_read(&mut control).await {
        ControlReply::WriteAck { write_seq } => assert_eq!(write_seq, 1),
        other => panic!("expected WriteAck, got {other:?}"),
    }
}

async fn timeout_read(stream: &mut UnixStream) -> ControlReply {
    tokio::time::timeout(LIVENESS_BUDGET, read_frame(stream))
        .await
        .expect("timed out reading reply")
        .expect("read reply")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

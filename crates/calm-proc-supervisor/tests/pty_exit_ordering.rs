//! The pty reader thread and the waiter are not synchronised: `Exited` must be the last frame on an attach stream and `exit.cursor` must be the real final byte count,
//! both on the happy path (pinned to one CPU to make the race deterministic) and on the degraded path where a grandchild holds the slave open.
//! Linux-only: `sched_setaffinity` and Linux pty semantics; gated at crate root so a non-Linux target compiles to zero tests rather than a green stub.
#![cfg(target_os = "linux")]

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, WriteStdinRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

/// Anti-hang guard only; no case here claims the supervisor reacts within this budget, so a slow-but-correct run must still pass.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

/// Budget for "the supervisor closes the attach stream right after `Exited`": promptness is the contract here, not an anti-hang guard.
/// Do not widen it to `LIVENESS_BUDGET` — a budget longer than the fixture's lifetime degrades `after.is_err()` into "we waited for the process to finish".
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

/// Confine this thread — and every thread/process it later spawns — to a single CPU: under contention the waiter can win against the reader, which is the race under test.
/// Scheduling only: a supervisor that drains the pty before publishing the exit passes with or without the pin.
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

    // `Exited` is terminal by construction (the supervisor closes the stream), so nothing may follow it.
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

    // Let the child exit and the reader drain the master to EOF, so the ring provably holds every byte.
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

/// The parent backgrounds a subshell that keeps the pty slave open and writes for ~5s, so the master never EOFs and the drain gate must time out.
/// `trap '' HUP` is load-bearing: the session leader's exit SIGHUPs the foreground group, and without the trap the test degenerates into a happy-path case; `sleep 0.2` closes the fork/trap race.
fn grandchild_script(pid_file: &Path) -> String {
    format!(
        "read x; (trap '' HUP; i=0; while [ $i -lt 100 ]; do printf 'X'; sleep 0.05; \
         i=$((i+1)); done) & printf '%s' \"$!\" > {}; sleep 0.2; \
         printf '\\n{PARENT_SENTINEL}\\n'; exit 0",
        pid_file.display(),
    )
}

/// True when `pid` exists and is not a zombie: a zombie has already closed every fd, so it proves nothing about the pty slave.
fn process_is_alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `comm` may contain spaces and parentheses — split at the LAST ')'.
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

/// Degraded path: a surviving grandchild holds the slave open, the drain gate times out, and the waiter must still make `Exited` final via the ring seal.
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

    // Liveness: the master never EOFs, so `Exited` only arrives because the grace expires. `LIVENESS_BUDGET` is the whole "does not wedge forever" assertion — a tighter elapsed bound would be a false failure under CI pauses.
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
    // Degradation self-check, causal rather than timed: the grandchild is the only holder of the slave, so if it is still alive here — strictly after `Exited` — the master provably could not have EOFed when the gate ran.
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

    // The seal is the real assertion: the grandchild is still writing 'X' every 50ms, but the ring must stay frozen at the exit cursor.
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

/// Bytes the grandchild bursts after the seal; must comfortably exceed the kernel's ~64 KiB pty buffer so an unread master would block the writer.
const POST_SEAL_BURST: usize = 300_000;

/// Anything at or above the ~64 KiB tty queue really had to be drained by the reader rather than merely buffered.
const KERNEL_TTY_QUEUE: usize = 64 * 1024;

/// Like `grandchild_script`, but the grandchild stays quiet until well past the seal, then bursts `POST_SEAL_BURST` bytes and writes the actual byte count (`${#blob}`) to `done_file`,
/// so a truncated `printf` cannot turn the test green.
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

/// Sealing the ring must stop publishing, not reading: no production path sends `Cleanup`, so a reader that stopped at the seal would leave a grandchild blocked in `write()` on a full tty queue forever.
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

    // The grandchild has not started its burst yet (it sleeps 0.6s, the seal lands ~0.25s in), so this is the sealed tail.
    assert_eq!(
        attach_tail(supervisor.sock(), proc_id).await,
        exit_cursor,
        "ring tail right after Exited must equal exit.cursor",
    );

    // The marker appears only after the tty accepted every burst byte; a reader that stops at the seal wedges the grandchild after ~64 KiB.
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
    // ...and it really was a burst: anything below the tty queue fits in the buffer whether or not the reader kept draining.
    assert!(
        written >= KERNEL_TTY_QUEUE,
        "the post-seal burst must exceed the kernel tty queue ({KERNEL_TTY_QUEUE} bytes) \
         for this test to prove anything about draining, but the grandchild only wrote \
         {written} bytes (expected {POST_SEAL_BURST})",
    );

    // ...and the drained bytes were discarded, not published — which also proves the burst landed after the seal.
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

//! Repro for #993: the pty reader thread and the waitpid thread are not
//! synchronised, so `child.wait()` can win the race against draining the pty
//! master. Two observable invariants break:
//!
//! A. `Exited` must be the last frame on an attach stream — every byte the
//!    process wrote must reach an online attacher *before* `Exited`.
//! B. `exit.cursor` must equal the process' real final byte count.
//!
//! Both tests make the race deterministic by having the child burst a large
//! blob and exit immediately afterwards, with the whole test (and every thread
//! it spawns) pinned to a single CPU — see `pin_to_single_cpu`.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, WriteStdinRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

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
        tokio::time::timeout(Duration::from_secs(2), read_frame(&mut attach))
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
    tokio::time::timeout(Duration::from_secs(10), read_frame(stream))
        .await
        .expect("timed out reading reply")
        .expect("read reply")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

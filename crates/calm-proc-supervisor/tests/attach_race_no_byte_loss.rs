use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, CleanupRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProcSignal,
    SignalRequest,
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

/// Number of chunks the child emits, one every [`CHUNK_INTERVAL`]. The
/// product is the width of the window during which the attach below must
/// land: long enough that a scheduling hiccup on the test task cannot
/// push the attach past the end of production (which would silently turn
/// this back into a replay-only case), short enough to stay a cheap test.
const CHUNKS: usize = 100;
/// Wall-clock gap the child shell sleeps between chunks.
const CHUNK_INTERVAL_SECS: &str = "0.05";
/// How long the test waits before attaching. Only a couple of chunks are
/// in the ring by then, so the overwhelming majority of the byte stream
/// is produced *after* the attach request is written — which is the point.
const ATTACH_AFTER: Duration = Duration::from_millis(50);

/// The attach must lose no bytes and duplicate none **while the child is
/// actively writing**. The child therefore emits its chunks spread over
/// several seconds and the test attaches near the start, so the seam
/// between the replay snapshot and the live broadcast subscription is
/// crossed with output genuinely in flight. (This case previously let the
/// child write every chunk before the attach and then sleep, so every
/// asserted byte came out of the replay buffer and the handoff window was
/// never open at all — a widened window in `handle_attach` could not have
/// failed it.)
///
/// The race is asserted, not assumed: the `AttachOk` replay snapshot must
/// be missing the final chunk, which is only true if production was still
/// running when the attach registered.
#[tokio::test]
async fn attach_race_no_byte_loss() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-attach-race";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        "/bin/sh",
        &[
            "-c",
            // The trailing sleep must outlast `LIVENESS_BUDGET`: the loop below
            // treats any non-`Output` frame as a hard error, so a child that
            // exits inside the budget turns a lost-bytes failure into a
            // misleading "unexpected attach frame: Exited" panic.
            &format!(
                "i=1; while [ $i -le {CHUNKS} ]; do printf \"chunk-%d-\" \"$i\"; \
                 i=$((i+1)); sleep {CHUNK_INTERVAL_SECS}; done; sleep 600"
            ),
        ],
    )
    .await;

    tokio::time::sleep(ATTACH_AFTER).await;

    let mut attach = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: Some(0),
            reader_id: "test".into(),
        }),
    )
    .await
    .expect("write attach");

    let mut bytes = match read_frame(&mut attach).await.expect("read attach ok") {
        ControlReply::AttachOk(attached) => attached.replay,
        other => panic!("unexpected attach reply: {other:?}"),
    };
    let last_chunk = format!("chunk-{CHUNKS}-");
    assert!(
        !contains(&bytes, last_chunk.as_bytes()),
        "the child must still be writing when the attach registers — otherwise \
         every asserted byte comes from replay and no handoff race is exercised; \
         replay already held {last_chunk:?}: {:?}",
        String::from_utf8_lossy(&bytes)
    );
    let expected: Vec<u8> = (1..=CHUNKS)
        .flat_map(|i| format!("chunk-{i}-").into_bytes())
        .collect();
    let expected = expected.as_slice();
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    while !contains(&bytes, expected) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(50), read_frame(&mut attach)).await {
            Ok(Ok(ControlReply::Output {
                proc_id: _,
                cursor: _,
                bytes: output,
            })) => bytes.extend_from_slice(&output),
            Ok(Ok(other)) => panic!("unexpected attach frame before signal: {other:?}"),
            Ok(Err(err)) => panic!("read attach frame: {err}"),
            Err(_) => {}
        }
    }

    assert!(
        contains(&bytes, expected),
        "attached stream should contain the complete chunk sequence; got {:?}",
        String::from_utf8_lossy(&bytes)
    );
    for i in 1..=CHUNKS {
        let chunk = format!("chunk-{i}-");
        assert_eq!(
            occurrence_count(&bytes, chunk.as_bytes()),
            1,
            "attached stream should contain {chunk:?} exactly once; got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    signal(supervisor.sock(), proc_id, ProcSignal::Kill).await;
    loop {
        match timeout_read(&mut attach).await {
            ControlReply::Exited { signalled, .. } => {
                assert!(signalled, "expected signal-killed exit");
                break;
            }
            ControlReply::Output { .. } => {}
            other => panic!("unexpected attach frame after signal: {other:?}"),
        }
    }
    cleanup(supervisor.sock(), proc_id).await;
}

async fn ensure_pty(sock: &Path, proc_id: &str, program: &str, args: &[&str]) {
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
    match read_frame(&mut stream).await.expect("read spawned") {
        ControlReply::Spawned { .. } => {}
        other => panic!("unexpected first reply: {other:?}"),
    }
    match read_frame(&mut stream).await.expect("read ready") {
        ControlReply::Ready => {}
        other => panic!("unexpected second reply: {other:?}"),
    }
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
    match read_frame(&mut stream).await.expect("read signal") {
        ControlReply::SignalOk => {}
        other => panic!("unexpected signal reply: {other:?}"),
    }
}

async fn cleanup(sock: &Path, proc_id: &str) {
    let mut stream = UnixStream::connect(sock).await.expect("connect cleanup");
    write_frame(
        &mut stream,
        &ControlMsg::Cleanup(CleanupRequest {
            proc_id: proc_id.into(),
        }),
    )
    .await
    .expect("write cleanup");
    match read_frame(&mut stream).await.expect("read cleanup") {
        ControlReply::CleanupOk => {}
        other => panic!("unexpected cleanup reply: {other:?}"),
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

fn occurrence_count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

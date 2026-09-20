use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, CleanupRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProcSignal,
    SignalRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

/// Anti-hang guard only; no case claims the supervisor reacts within this budget, so a slow-but-correct run must still pass.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

/// The product with [`CHUNK_INTERVAL_SECS`] is the window the attach must land in: a scheduling hiccup must not push it past the end of production.
const CHUNKS: usize = 100;
/// Wall-clock gap the child shell sleeps between chunks.
const CHUNK_INTERVAL_SECS: &str = "0.05";
/// Only a couple of chunks are in the ring by then, so most of the byte stream is produced after the attach — which is the point.
const ATTACH_AFTER: Duration = Duration::from_millis(50);

/// The race is asserted, not assumed: the `AttachOk` replay snapshot must be missing the final chunk, which is only true if production was still running when the attach registered.
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
            // The trailing sleep must outlast `LIVENESS_BUDGET`: the loop below treats any non-`Output` frame as a hard error.
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

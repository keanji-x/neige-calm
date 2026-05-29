use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, CleanupRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProcSignal,
    SignalRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

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
            "for i in 1 2 3 4 5 6 7 8 9; do printf \"chunk-%d-\" \"$i\"; done; sleep 30",
        ],
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

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
    let expected = b"chunk-1-chunk-2-chunk-3-chunk-4-chunk-5-chunk-6-chunk-7-chunk-8-chunk-9-";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
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
    for i in 1..=9 {
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
    tokio::time::timeout(Duration::from_secs(2), read_frame(stream))
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

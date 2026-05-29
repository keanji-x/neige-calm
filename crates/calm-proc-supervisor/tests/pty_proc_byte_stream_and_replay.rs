use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, CleanupRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode,
    ProbeRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

#[tokio::test]
async fn pty_proc_byte_stream_and_replay() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-replay";
    ensure_pty(supervisor.sock(), proc_id, "/bin/sh", &["-c", "printf abc"]).await;

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
    let replay = match read_frame(&mut attach).await.expect("read attach ok") {
        ControlReply::AttachOk(attached) => attached.replay,
        other => panic!("unexpected attach reply: {other:?}"),
    };
    assert!(
        contains(&replay, b"abc"),
        "replay should contain child output; got {replay:?}"
    );
    match timeout_read(&mut attach).await {
        ControlReply::Exited {
            status, signalled, ..
        } => {
            assert_eq!(status, Some(0));
            assert!(!signalled);
        }
        other => panic!("expected Exited, got {other:?}"),
    }

    cleanup(supervisor.sock(), proc_id).await;
    let mut probe = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect probe");
    write_frame(
        &mut probe,
        &ControlMsg::Probe(ProbeRequest {
            proc_id: proc_id.into(),
        }),
    )
    .await
    .expect("write probe");
    match read_frame(&mut probe).await.expect("read probe") {
        ControlReply::ProbeOk { proc_running, .. } => assert!(!proc_running),
        other => panic!("unexpected probe reply: {other:?}"),
    }
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

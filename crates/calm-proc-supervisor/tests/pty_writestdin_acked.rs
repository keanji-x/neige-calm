use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, WriteStdinRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

#[tokio::test]
async fn pty_writestdin_acked() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-stdin";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        "/bin/sh",
        &["-c", "read x; echo \"got:$x\""],
    )
    .await;

    let mut attach = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: None,
            reader_id: "test".into(),
        }),
    )
    .await
    .expect("write attach");
    match read_frame(&mut attach).await.expect("read attach ok") {
        ControlReply::AttachOk(_) => {}
        other => panic!("unexpected attach reply: {other:?}"),
    }

    let mut control = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect control");
    write_frame(
        &mut control,
        &ControlMsg::WriteStdin(WriteStdinRequest {
            proc_id: proc_id.into(),
            bytes: b"hello\n".to_vec(),
            write_seq: Some(1),
        }),
    )
    .await
    .expect("write stdin");
    match timeout_read(&mut control).await {
        ControlReply::WriteAck { write_seq } => assert_eq!(write_seq, 1),
        other => panic!("expected WriteAck, got {other:?}"),
    }

    let mut output = Vec::new();
    loop {
        match timeout_read(&mut attach).await {
            ControlReply::Output { bytes, .. } => {
                output.extend(bytes);
                if contains(&output, b"got:hello") {
                    break;
                }
            }
            ControlReply::Exited { .. } if contains(&output, b"got:hello") => break,
            other => panic!("unexpected attach frame before got output: {other:?}"),
        }
    }
    loop {
        match timeout_read(&mut attach).await {
            ControlReply::Exited { status, .. } => {
                assert_eq!(status, Some(0));
                break;
            }
            ControlReply::Output { .. } => {}
            other => panic!("unexpected attach frame before exit: {other:?}"),
        }
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

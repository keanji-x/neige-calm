//! #993 semantics lock, guarding the #1013 fix.
//!
//! `terminate_all_process_groups` signals the *process group*, not the direct
//! child, so a surviving grandchild sharing the leader's pgid still gets the
//! SIGTERM. #993's review rejected narrowing this to a `pty_running()`-style
//! predicate for exactly that reason.
//!
//! The #1013 fix adds a liveness guard on the same code path, and the obvious
//! wrong way to write that guard — requiring `process_group_leader()` to still
//! report a pgid — would silently reintroduce the grandchild leak. This test
//! is green before the fix and must stay green after it.
//!
//! Non-interactive `sh` has job control off, so the backgrounded `sleep` stays
//! in the leader's process group; that shared pgid is the whole point.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

mod proc_probe;
use proc_probe::{alive, await_death};

#[tokio::test]
async fn terminate_all_kills_grandchild_of_live_leader() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-grandchild";

    let leader = ensure_pty(
        supervisor.sock(),
        proc_id,
        "/bin/sh",
        &["-c", "sleep 300 & echo GC=$!; wait"],
    )
    .await;
    let mut attach = attach(supervisor.sock(), proc_id).await;
    let grandchild = read_grandchild_pid(&mut attach).await;

    assert_ne!(
        grandchild, leader,
        "fixture must produce a distinct grandchild, not the leader itself"
    );
    assert!(
        alive(grandchild),
        "grandchild {grandchild} must be alive before shutdown — otherwise this \
         test would pass without terminate_all doing anything"
    );
    assert!(
        alive(leader),
        "leader {leader} must still be alive: the guard being locked in here only \
         applies to entries with no recorded exit"
    );

    // Production shutdown wiring: `Drop` calls
    // `terminate_all_process_groups_sync`, the same call `main.rs` makes.
    drop(supervisor);

    assert!(
        await_death(grandchild, Duration::from_secs(5)),
        "grandchild {grandchild} survived terminate_all_process_groups — the \
         process-group SIGTERM was skipped or retargeted"
    );
}

async fn read_grandchild_pid(stream: &mut UnixStream) -> u32 {
    let mut buf = String::new();
    loop {
        match timeout_read(stream).await {
            ControlReply::Output { bytes, .. } => {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                if let Some(rest) = buf.split("GC=").nth(1) {
                    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    // Wait for the terminator so a chunk boundary mid-number
                    // cannot truncate the pid we parse.
                    if digits.len() < rest.len() && !digits.is_empty() {
                        return digits.parse().expect("grandchild pid");
                    }
                }
            }
            other => panic!("unexpected frame while waiting for grandchild pid: {other:?}"),
        }
    }
}

async fn ensure_pty(sock: &Path, proc_id: &str, program: &str, args: &[&str]) -> u32 {
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
            from_cursor: Some(0),
            reader_id: "test".into(),
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

async fn timeout_read(stream: &mut UnixStream) -> ControlReply {
    tokio::time::timeout(Duration::from_secs(5), read_frame(stream))
        .await
        .expect("timed out reading reply")
        .expect("read reply")
}

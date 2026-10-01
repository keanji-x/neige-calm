//! Stop confirmation fences delayed launches and processes that escaped the leader group.
use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use tokio::net::UnixStream;

#[tokio::test]
async fn terminal_stop_tombstone_refuses_delayed_ensure() {
    let host = InProcessProcSupervisor::start().await.unwrap();
    let mut conn = UnixStream::connect(host.sock()).await.unwrap();
    write_frame(
        &mut conn,
        &ControlMsg::StopAndConfirm {
            proc_id: "term:stop-before-start".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::Stopped
    ));
    write_frame(
        &mut conn,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: "term:stop-before-start".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            envs: vec![],
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 1024,
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::SpawnFailed { .. }
    ));
    assert_eq!(host.registry().debug_entry_count(), 0);
}

#[tokio::test]
async fn terminal_stop_confirms_escaped_descendant_after_leader_exit() {
    let host = InProcessProcSupervisor::start().await.unwrap();
    let mut foreign = std::process::Command::new("/bin/sleep")
        .arg("30")
        .env_clear()
        .spawn()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let pidfile = root.path().join("child.pid");
    let script = format!(
        "setsid sh -c 'echo $$ > {}; exec sleep 300' </dev/null >/dev/null 2>&1 & sleep 0.2; exit",
        pidfile.display()
    );
    let mut conn = UnixStream::connect(host.sock()).await.unwrap();
    write_frame(
        &mut conn,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: "term:escaped-stop".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script],
            envs: vec![("NEIGE_EXECUTION_OP".into(), "forged".into())],
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 1024,
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::Spawned { .. }
    ));
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::Ready
    ));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !pidfile.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        while !host
            .registry()
            .debug_entry_stats("term:escaped-stop")
            .is_some_and(|s| s.exit_observed)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let child: i32 = std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(std::path::Path::new(&format!("/proc/{child}")).exists());
    write_frame(
        &mut conn,
        &ControlMsg::StopAndConfirm {
            proc_id: "term:escaped-stop".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::Stopped
    ));
    assert!(
        foreign.try_wait().unwrap().is_none(),
        "stop must preserve a foreign sibling execution"
    );
    foreign.kill().unwrap();
    foreign.wait().unwrap();
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{child}/stat")) {
        assert!(
            stat.rsplit_once(')').unwrap().1.trim().starts_with('Z'),
            "escaped child remains live: {stat}"
        );
    }
}

//! Owned protocol fault fixtures; all process creation remains in the real supervisor.
use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProbeRequest};
use calm_session::{read_frame, write_frame};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(crate) struct AckProxy {
    pub(crate) sock: PathBuf,
    _dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
    pub(crate) ensures: Arc<AtomicUsize>,
    pub(crate) spawned: Arc<tokio::sync::Notify>,
}
impl AckProxy {
    pub(crate) async fn start(upstream: &Path, marker: PathBuf, hold_ack: bool) -> Self {
        let dir = calm_test_sockets::socket_dir("launch");
        let sock = dir.path().join("proxy.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let upstream = upstream.to_path_buf();
        let ensures = Arc::new(AtomicUsize::new(0));
        let count = ensures.clone();
        let spawned = Arc::new(tokio::sync::Notify::new());
        let observed = spawned.clone();
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                let upstream = upstream.clone();
                let marker = marker.clone();
                let count = count.clone();
                let observed = observed.clone();
                clients.spawn(async move {
                    let Ok(message) = read_frame::<ControlMsg, _>(&mut client).await else {
                        return;
                    };
                    if matches!(message, ControlMsg::EnsureProc(_)) {
                        if count.fetch_add(1, Ordering::SeqCst) == 0 {
                            let mut actual =
                                tokio::net::UnixStream::connect(upstream).await.unwrap();
                            write_frame(&mut actual, &message).await.unwrap();
                            let spawned_reply =
                                read_frame::<ControlReply, _>(&mut actual).await.unwrap();
                            assert!(matches!(spawned_reply, ControlReply::Spawned { .. }));
                            tokio::time::timeout(Duration::from_secs(3), async {
                                while !marker.exists() {
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                }
                            })
                            .await
                            .unwrap();
                            observed.notify_one();
                            if hold_ack {
                                std::future::pending::<()>().await;
                            } else {
                                // Forward the actual recorded PID, after the
                                // bounded command has demonstrably executed.
                                let stats =
                                    read_frame::<ControlReply, _>(&mut actual).await.unwrap();
                                // Spawned was saved below before waiting for the witness.
                                write_frame(&mut client, &spawned_reply).await.unwrap();
                                let _ = write_frame(&mut client, &stats).await;
                                let _ =
                                    tokio::io::copy_bidirectional(&mut client, &mut actual).await;
                            }
                            drop((client, actual));
                        } else {
                            write_frame(
                                &mut client,
                                &ControlReply::SpawnFailed {
                                    error: "fixture rejects duplicate EnsureProc".into(),
                                    child_already_reaped: true,
                                },
                            )
                            .await
                            .unwrap();
                        }
                    } else if !hold_ack {
                        let mut actual = tokio::net::UnixStream::connect(upstream).await.unwrap();
                        write_frame(&mut actual, &message).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut actual).await;
                    } else {
                        let _ = write_frame(
                            &mut client,
                            &ControlReply::Error {
                                kind: calm_session::control::ControlErrorKind::UnknownProc,
                                message: "fixture unresolved launch".into(),
                            },
                        )
                        .await;
                    }
                });
            }
        });
        Self {
            sock,
            _dir: dir,
            task,
            ensures,
            spawned,
        }
    }
}
impl Drop for AckProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn install_commit_fault(pool: &sqlx::SqlitePool) {
    sqlx::raw_sql(r#"
        CREATE TABLE launch_commit_fault (operation_id TEXT REFERENCES operations(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER fail_launch_commit AFTER UPDATE OF tx_output_json ON operations
        WHEN json_extract(NEW.tx_output_json, '$.data.launch_admission') IS NOT NULL
        BEGIN INSERT INTO launch_commit_fault VALUES('missing-launch-operation'); END;
    "#).execute(pool).await.unwrap();
}

pub(crate) async fn spawn_sibling(sock: &Path, cwd: &Path) -> String {
    let proc_id = format!("sibling:{}", crate::model::new_id());
    let mut connection = tokio::net::UnixStream::connect(sock).await.unwrap();
    write_frame(
        &mut connection,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.clone(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 120".into()],
            envs: vec![("PATH".into(), "/usr/bin:/bin".into())],
            cwd: cwd.to_str().unwrap().into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 1024,
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut connection)
            .await
            .unwrap(),
        ControlReply::Spawned { .. }
    ));
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut connection)
            .await
            .unwrap(),
        ControlReply::Ready
    ));
    proc_id
}

pub(crate) async fn probe_running(sock: &Path, proc_id: &str) -> bool {
    let mut connection = tokio::net::UnixStream::connect(sock).await.unwrap();
    write_frame(
        &mut connection,
        &ControlMsg::Probe(ProbeRequest {
            proc_id: proc_id.into(),
        }),
    )
    .await
    .unwrap();
    matches!(
        read_frame::<ControlReply, _>(&mut connection)
            .await
            .unwrap(),
        ControlReply::ProbeOk {
            proc_running: true,
            ..
        }
    )
}

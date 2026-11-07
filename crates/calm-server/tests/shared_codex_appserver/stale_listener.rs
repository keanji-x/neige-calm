//! Linux stale-listener replacement fences through the real supervisor entry points.
use super::*;
use calm_server::error::CalmError;
use calm_server::proc_identity::{ProcessObservation, observe_process};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use tokio::net::UnixStream;

async fn listener(sock: &Path, unknown: bool) -> (tokio::process::Child, i32) {
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let mut command = Command::new(fake_codex_bin());
    command
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    if unknown {
        command
            .env("FAKE_CODEX_IGNORE_SIGTERM", "1")
            .env("FAKE_CODEX_NON_UTF8_COMM", "1");
    }
    let child = command.spawn().expect("spawn private listener");
    let pid = child.id().unwrap() as i32;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !sock.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("private listener binds after installing signal/name fixtures");
    assert_eq!(unsafe { libc::getpgid(pid) }, pid);
    assert_eq!(socket_peer(sock).await, Some(pid));
    if unknown {
        assert_eq!(
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(observe_process(pid), ProcessObservation::Unknown);
    }
    (child, pid)
}

async fn socket_peer(sock: &Path) -> Option<i32> {
    UnixStream::connect(sock)
        .await
        .ok()?
        .peer_cred()
        .ok()?
        .pid()
}

async fn unknown_blocks_replacement(ensure: bool) {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    let (mut child, pid) = listener(&sock, true).await;
    let inode = std::fs::metadata(&sock).unwrap().ino();
    let repo = repo().await;

    // Observe even a short-lived replacement, without leaking a daemon on a red run.
    let replacement = root.path().join("replacement");
    let marker = root.path().join("replacement-started");
    std::fs::write(
        &replacement,
        "#!/bin/sh\n: > \"$(dirname \"$0\")/replacement-started\"\nexit 39\n",
    )
    .unwrap();
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Record transient Starting identities too: a final Failed row alone could hide an overwrite.
    for statement in [
        "CREATE TABLE listener_identity_writes (pid INTEGER NOT NULL)",
        "CREATE TRIGGER listener_identity_insert AFTER INSERT ON codex_backend_daemons \
         WHEN NEW.pid IS NOT NULL BEGIN INSERT INTO listener_identity_writes VALUES (NEW.pid); END",
        "CREATE TRIGGER listener_identity_update AFTER UPDATE ON codex_backend_daemons \
         WHEN NEW.pid IS NOT NULL BEGIN INSERT INTO listener_identity_writes VALUES (NEW.pid); END",
    ] {
        repo.db()
            .writer()
            .execute(sqlx::query(statement))
            .await
            .unwrap();
    }
    let mut config = cfg(&root);
    config.codex_bin = replacement.to_str().unwrap().into();
    config.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    config.shared_codex_appserver_restart_max_delay_ms = 60_000;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        config.data_dir_resolved().join("codex-home"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&config, Arc::new(home), repo.clone());
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        if ensure {
            daemon.ensure_running().await
        } else {
            daemon.start_or_takeover().await
        }
    })
    .await;
    let peer = socket_peer(&sock).await;
    let retained_inode = std::fs::metadata(&sock).ok().map(|metadata| metadata.ino());
    let alive = child.try_wait().unwrap().is_none();
    let observation = observe_process(pid);
    let record = repo.shared_daemon_runtime_get("default").await.unwrap();
    let identity_writes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM listener_identity_writes")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let snapshot = daemon.status_snapshot();
    child.kill().await.unwrap();
    child.wait().await.unwrap();

    assert!(matches!(result, Ok(Err(CalmError::CodexAppServer(_)))));
    assert!(
        alive,
        "Unknown cannot authorize escalation or a member sweep"
    );
    assert_eq!(observation, ProcessObservation::Unknown);
    assert_eq!(
        peer,
        Some(pid),
        "original socket must still reach the old listener"
    );
    assert_eq!(
        retained_inode,
        Some(inode),
        "original socket must not be unlinked"
    );
    assert!(!marker.exists(), "replacement executable must never start");
    assert_eq!(
        identity_writes, 0,
        "replacement identity must never be persisted"
    );
    assert_eq!(record.state, SharedDaemonState::Failed);
    assert_eq!(
        (
            record.pid,
            record.pgid,
            record.process_start_time,
            record.boot_id
        ),
        (None, None, None, None)
    );
    assert_eq!(snapshot.state, SharedDaemonState::Failed);
    assert!(snapshot.runtime.is_none());
}

#[tokio::test]
async fn stale_listener_unknown_blocks_start_without_unlink_or_spawn() {
    unknown_blocks_replacement(false).await;
}

#[tokio::test]
async fn stale_listener_unknown_blocks_ensure_running_without_unlink_or_spawn() {
    unknown_blocks_replacement(true).await;
}

#[tokio::test]
async fn stale_listener_pid_fallback_unknown_returns_error_without_escalation() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    let (mut child, pid) = listener(&sock, true).await;
    let daemon = server(&root, repo().await).await;
    let result = daemon
        .reap_stale_listener_with_pgid_for_test(Err(std::io::Error::from_raw_os_error(libc::ESRCH)))
        .await;
    let alive = child.try_wait().unwrap().is_none();
    let peer = socket_peer(&sock).await;
    let observation = observe_process(pid);
    child.kill().await.unwrap();
    child.wait().await.unwrap();

    assert!(matches!(result, Err(CalmError::CodexAppServer(_))));
    assert!(alive, "pid fallback must not escalate after Unknown");
    assert_eq!(peer, Some(pid));
    assert_eq!(observation, ProcessObservation::Unknown);
}

#[tokio::test]
async fn stale_listener_pid_fallback_proven_exit_succeeds() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    let (mut child, _) = listener(&sock, false).await;
    let daemon = server(&root, repo().await).await;
    let result = daemon
        .reap_stale_listener_with_pgid_for_test(Err(std::io::Error::from_raw_os_error(libc::ESRCH)))
        .await;
    let exit = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    // kill_on_drop is the cleanup belt if an erroneous result failed to terminate this child.
    assert!(result.is_ok());
    assert!(matches!(exit, Ok(Ok(_))));
}

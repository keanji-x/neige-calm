//! Real production-entry regressions for scope and lifecycle authority.
use super::*;
use calm_truth::db::DaemonRecovery;
use std::os::unix::fs::PermissionsExt;

async fn receive<T>(rx: tokio::sync::oneshot::Receiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap()
}

async fn first_read_window(state: SharedDaemonState) {
    for recorded in [false, true] {
        let mut f = Fixture::with_storage(false, true).await;
        f.persist(state, f.start).await;
        if recorded {
            f.repo.db().writer().execute(sqlx::query("UPDATE codex_backend_daemons SET recovery_phase='recorded', recovery_id=? WHERE backend_id='default'").bind(calm_truth::db::RecoveryId::fresh().to_string())).await.unwrap();
        }
        let old = f.repo.shared_daemon_runtime_get("default").await.unwrap();
        f.repo
            .db()
            .writer()
            .execute(sqlx::query(
                "ALTER TABLE codex_backend_daemons RENAME TO paused_daemons",
            ))
            .await
            .unwrap();
        let (entered, paused) = tokio::sync::oneshot::channel();
        let (release, resume) = tokio::sync::oneshot::channel();
        *f.daemon.before_failure_finalize.lock().unwrap() = Some((entered, resume));
        let owner = f.daemon.clone();
        let start = tokio::spawn(async move { owner.start_or_takeover().await });
        receive(paused).await;
        f.repo
            .db()
            .writer()
            .execute(sqlx::query(
                "ALTER TABLE paused_daemons RENAME TO codex_backend_daemons",
            ))
            .await
            .unwrap();
        release.send(()).unwrap();
        assert!(start.await.unwrap().is_err());
        let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(
            saved.state, old.state,
            "first read failure must not acquire write authority"
        );
        assert_eq!(saved.recovery, old.recovery);
        assert_eq!(
            (
                saved.pid,
                saved.pgid,
                saved.process_start_time,
                saved.boot_id
            ),
            (old.pid, old.pgid, old.process_start_time, old.boot_id)
        );
        assert!(f.paths.paths().socket.exists());
        assert!(f.leader.alive() && f.member.alive() && f.unrelated.alive());
    }
}

#[tokio::test]
async fn native_macos_recovery_first_read_window_running() {
    first_read_window(SharedDaemonState::Running).await;
}
#[tokio::test]
async fn native_macos_recovery_first_read_window_starting() {
    first_read_window(SharedDaemonState::Starting).await;
}

#[tokio::test]
async fn native_macos_recovery_connect_error_keeps_new_pending_after_old_settlement() {
    let mut f = Fixture::with_storage(false, true).await;
    f.persist(SharedDaemonState::Failed, f.start).await;
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    assert!(macos_process::group_empty(f.leader.pid()));
    let (entered, paused) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    *f.daemon.recovery_hooks.after_begin.lock().unwrap() = Some((entered, resume));
    let owner = f.daemon.clone();
    let start = tokio::spawn(async move { owner.start_or_takeover().await });
    receive(paused).await;
    let pending = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(pending.recovery, DaemonRecovery::Pending { .. }));
    let mut g = Fixture::with_pollution(false).await;
    std::fs::remove_file(&f.paths.paths().socket).unwrap();
    std::fs::rename(&g.paths.paths().socket, &f.paths.paths().socket).unwrap();
    std::fs::set_permissions(
        &f.paths.paths().socket,
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let control = std::os::unix::net::UnixStream::connect(&f.paths.paths().socket).unwrap_err();
    assert_eq!(
        control.raw_os_error(),
        Some(libc::EACCES),
        "must reproduce actual filesystem permission denial"
    );
    release.send(()).unwrap();
    assert!(start.await.unwrap().is_err());
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(
        saved.recovery, pending.recovery,
        "old settlement cannot clear the new scope"
    );
    assert!(f.paths.paths().socket.exists());
    std::fs::set_permissions(
        &f.paths.paths().socket,
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(g.leader.alive() && g.member.alive() && f.unrelated.alive());
    super::spawn_obligation_tests::reopen(&mut f).await;
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert_eq!(
        f.repo
            .shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        pending.recovery
    );
    assert!(g.leader.alive() && g.member.alive());
}

#[tokio::test]
async fn native_macos_socket_cleanup_refuses_live_chmod_full_backlog_and_rebound() {
    use super::super::socket_recovery::{Endpoint, conditional_delete};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut f = Fixture::with_storage(false, true).await;
    let reservation = f.daemon.begin_recovery(&f.paths).await.unwrap();
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Busy
    );
    std::fs::set_permissions(
        &f.paths.paths().socket,
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Busy
    );
    std::fs::set_permissions(
        &f.paths.paths().socket,
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(f.leader.alive() && f.member.alive());
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Removed
    );
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Missing
    );
    let listener = std::os::unix::net::UnixListener::bind(&f.paths.paths().socket).unwrap();
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
    let mut clients = Vec::new();
    let mut refused = false;
    // Exercise the native nonblocking syscall without Tokio's readiness retry.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    let path = f.paths.paths().socket.as_os_str().as_bytes();
    assert!(path.len() < address.sun_path.len());
    for (slot, byte) in address.sun_path.iter_mut().zip(path) {
        *slot = *byte as _;
    }
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1) as libc::socklen_t;
    address.sun_len = len as _;
    for _ in 0..4 {
        let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(raw >= 0);
        let client = unsafe { OwnedFd::from_raw_fd(raw) };
        assert_eq!(
            unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        if unsafe { libc::connect(raw, &address as *const _ as *const libc::sockaddr, len) } == 0 {
            clients.push(client);
        } else {
            let error = std::io::Error::last_os_error();
            assert_eq!(error.raw_os_error(), Some(libc::ECONNREFUSED));
            refused = true;
        }
    }
    assert!(
        !clients.is_empty(),
        "backlog must first accept a connection"
    );
    assert!(refused, "qualification must exercise a full live backlog");
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Busy
    );
    drop(clients);
    drop(listener);
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Removed
    );
    // Replace the stale inode with a new live listener under the same name.
    let stale = std::os::unix::net::UnixListener::bind(&f.paths.paths().socket).unwrap();
    drop(stale);
    std::fs::remove_file(&f.paths.paths().socket).unwrap();
    let rebound = std::os::unix::net::UnixListener::bind(&f.paths.paths().socket).unwrap();
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Busy
    );
    assert!(f.paths.paths().socket.exists());
    drop(rebound);
    assert_eq!(
        conditional_delete(&f.paths, &reservation).unwrap(),
        Endpoint::Removed
    );
    std::fs::write(&f.paths.paths().socket, b"regular control").unwrap();
    assert!(conditional_delete(&f.paths, &reservation).is_err());
    assert!(f.paths.paths().socket.is_file());
    std::fs::remove_file(&f.paths.paths().socket).unwrap();
    std::os::unix::fs::symlink(f.root.path().join("target"), &f.paths.paths().socket).unwrap();
    assert!(conditional_delete(&f.paths, &reservation).is_err());
    f.daemon
        .cancel_reserved(reservation, "qualification complete")
        .await
        .unwrap();
}

#[tokio::test]
async fn native_macos_recovery_unsupported_pre_entry_cancels_reservation() {
    let mut f = Fixture::with_storage(false, true).await;
    f.persist(SharedDaemonState::Failed, f.start).await;
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    for _ in 0..2 {
        *f.daemon.recovery_hooks.socket_cleanup_error.lock().unwrap() =
            Some(std::io::ErrorKind::Unsupported);
        assert!(f.daemon.start_or_takeover().await.is_err());
        assert_eq!(
            f.repo
                .shared_daemon_runtime_get("default")
                .await
                .unwrap()
                .recovery,
            DaemonRecovery::Clear
        );
        assert!(f.paths.paths().socket.exists());
        assert!(f.unrelated.alive());
    }
}

#[tokio::test]
async fn native_macos_missing_command_retry_observes_a_new_listener() {
    let mut f = Fixture::with_storage(false, true).await;
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    std::fs::remove_file(&f.paths.paths().socket).unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    let first = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert_eq!(
        f.repo
            .shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .updated_at,
        first.updated_at
    );

    let mut g = Fixture::with_pollution(false).await;
    std::fs::rename(&g.paths.paths().socket, &f.paths.paths().socket).unwrap();
    let (entered, acquired) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    *f.daemon.recovery_hooks.after_acquire.lock().unwrap() = Some((entered, resume));
    let owner = f.daemon.clone();
    let start = tokio::spawn(async move { owner.start_or_takeover().await });
    assert_eq!(receive(acquired).await, g.leader.pid());
    let pending = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(pending.recovery, DaemonRecovery::Pending { .. }));
    assert_ne!(pending.recovery, first.recovery);
    assert!(g.leader.alive() && g.member.alive());
    release.send(()).unwrap();
    assert!(start.await.unwrap().is_err()); // missing producer still refuses entry
    assert!(macos_process::group_empty(g.leader.pid()));
    assert_eq!(
        f.repo
            .shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        DaemonRecovery::Clear
    );
    assert!(f.unrelated.alive() && g.unrelated.alive());
}

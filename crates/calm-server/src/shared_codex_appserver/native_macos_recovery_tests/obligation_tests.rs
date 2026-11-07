//! Real listener publication failures; all processes and the database belong to the fixture.
use super::*;

async fn listener_first_write_failure(old_scope: bool) {
    let mut f = Fixture::with_storage(false, true).await;
    if old_scope {
        let mut old = Owned(
            Command::new("/bin/sleep")
                .arg("60")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let start = read_proc_start_time(old.pid()).unwrap();
        let boot = read_boot_id().expect("native boot UUID required");
        // H is a real, independently owned, now-empty scope. Its absence says nothing about G.
        old.0.kill().unwrap();
        old.0.wait().unwrap();
        f.repo.db().writer().execute(sqlx::query(
            "UPDATE codex_backend_daemons SET state='idle', pid=?1, pgid=?1, process_start_time=?2, boot_id=?3 WHERE backend_id='default'",
        ).bind(old.pid()).bind(start as i64).bind(boot)).await.unwrap();
    }
    let pid = f.leader.pid();
    *f.daemon.before_identity_capture.lock().unwrap() = Some(Box::new(move |peer| {
        assert_eq!(peer, pid);
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !proc_pid_is_zombie(pid) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }));
    f.repo.db().writer().execute(sqlx::query(&format!(
        "CREATE TRIGGER fail_listener_publish BEFORE UPDATE ON codex_backend_daemons WHEN NEW.pid = {pid} BEGIN SELECT RAISE(ABORT, 'forced listener publish failure'); END",
    ))).await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    *f.daemon.before_failure_finalize.lock().unwrap() = Some((entered_tx, resume_rx));
    let start = f.daemon.start_or_takeover();
    let release = async {
        tokio::time::timeout(Duration::from_secs(10), entered_rx)
            .await
            .unwrap()
            .unwrap();
        // The failed transaction cannot decrement a counter. Remove the actual trigger
        // only after the first publication failed and before the common finalizer runs.
        f.repo
            .db()
            .writer()
            .execute(sqlx::query("DROP TRIGGER fail_listener_publish"))
            .await
            .unwrap();
        resume_tx.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(start, release);
    assert!(result.is_err());
    let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(
        record.pid,
        Some(pid),
        "the finalizer must retain the newly observed G, not old absence"
    );
    assert_eq!(record.pgid, Some(pid));
    assert!(f.member.alive() && f.unrelated.alive());
    f.leader.0.wait().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        f.root.path().join("recovery.db").display()
    );
    f.repo.db().close().await;
    f.repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    f.rebuild_owner();
    for _ in 0..2 {
        assert!(f.daemon.start_or_takeover().await.is_err());
        f.assert_fenced().await;
        assert!(f.member.alive() && f.unrelated.alive());
    }
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(record.pid.is_none() && record.pgid.is_none());
    assert!(
        !f.paths.paths().socket.exists(),
        "settled G permits its socket cleanup"
    );
    assert!(f.unrelated.alive());
}

#[tokio::test]
async fn native_macos_recovery_listener_first_write_failure_retains_obligation() {
    listener_first_write_failure(false).await;
}

#[tokio::test]
async fn native_macos_recovery_listener_first_write_failure_old_empty_scope() {
    listener_first_write_failure(true).await;
}

#[tokio::test]
async fn native_macos_recovery_listener_repeated_write_failure_fences_rebuilt_owner() {
    let mut f = Fixture::with_storage(false, true).await;
    let pid = f.leader.pid();
    *f.daemon.before_identity_capture.lock().unwrap() = Some(Box::new(move |peer| {
        assert_eq!(peer, pid);
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !proc_pid_is_zombie(pid) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }));
    f.repo.db().writer().execute(sqlx::query("CREATE TRIGGER fail_publish BEFORE UPDATE ON codex_backend_daemons WHEN NEW.pid IS NOT NULL BEGIN SELECT RAISE(ABORT,'publication refused'); END")).await.unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(
        saved.recovery,
        calm_truth::db::DaemonRecovery::Pending { .. }
    ));
    assert!(saved.pid.is_none());
    f.leader.0.wait().unwrap();
    f.repo.db().close().await;
    let url = format!(
        "sqlite://{}?mode=rwc",
        f.root.path().join("recovery.db").display()
    );
    f.repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    f.rebuild_owner();
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert!(f.member.alive() && f.unrelated.alive());
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert_eq!(
        f.repo
            .shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        saved.recovery
    );
    assert!(f.paths.paths().socket.exists());
    assert!(f.unrelated.alive());
}

#[tokio::test]
async fn native_macos_recovery_prepare_write_failure_has_no_process_effects() {
    let mut f = Fixture::with_storage(false, true).await;
    *f.daemon.before_identity_capture.lock().unwrap() =
        Some(Box::new(|_| panic!("capture before committed permit")));
    f.repo.db().writer().execute(sqlx::query("CREATE TRIGGER fail_begin BEFORE UPDATE ON codex_backend_daemons WHEN NEW.recovery_phase='pending' BEGIN SELECT RAISE(ABORT,'begin refused'); END")).await.unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert!(f.leader.alive() && f.member.alive() && f.unrelated.alive());
    assert!(f.paths.paths().socket.exists());
    assert_eq!(
        f.repo
            .shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        calm_truth::db::DaemonRecovery::LegacyUntracked
    );
}

#[tokio::test]
async fn native_macos_recovery_settlement_write_failure_blocks_next_scope() {
    let mut f = Fixture::with_storage(false, true).await;
    f.persist(SharedDaemonState::Failed, f.start).await;
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    f.repo.db().writer().execute(sqlx::query("CREATE TRIGGER fail_settle BEFORE UPDATE ON codex_backend_daemons WHEN NEW.recovery_phase='clear' BEGIN SELECT RAISE(ABORT,'settle refused'); END")).await.unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(
        saved.recovery,
        calm_truth::db::DaemonRecovery::Recorded { .. }
    ));
    assert_eq!(saved.pid, Some(f.leader.pid()));
    assert!(f.paths.paths().socket.exists());
    f.repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER fail_settle"))
        .await
        .unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err()); // missing executable, after settlement
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(saved.recovery, calm_truth::db::DaemonRecovery::Clear);
    assert!(saved.pid.is_none());
    assert!(!f.paths.paths().socket.exists());
    assert!(f.unrelated.alive());
}

#[tokio::test]
async fn native_macos_recovery_cancelled_pending_intent_fences_owner() {
    let mut f = Fixture::with_storage(false, true).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (_resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    *f.daemon.recovery_hooks.after_begin.lock().unwrap() = Some((entered_tx, resume_rx));
    let owner = f.daemon.clone();
    let caller = tokio::spawn(async move { owner.start_or_takeover().await });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(f.daemon.abort_detached_spawn_transition_for_test());
    assert!(caller.await.unwrap().is_err());
    assert!(f.leader.alive() && f.member.alive() && f.unrelated.alive());
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(
        saved.recovery,
        calm_truth::db::DaemonRecovery::Pending { .. }
    ));
    f.repo.db().close().await;
    let url = format!(
        "sqlite://{}?mode=rwc",
        f.root.path().join("recovery.db").display()
    );
    f.repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    f.rebuild_owner();
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert_eq!(
        f.repo
            .shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        saved.recovery
    );
    assert!(f.leader.alive() && f.member.alive() && f.unrelated.alive());
    assert!(f.paths.paths().socket.exists());
}

#[tokio::test]
async fn native_macos_recovery_record_read_failure_keeps_committed_scope() {
    let mut f = Fixture::with_storage(false, true).await;
    f.persist(SharedDaemonState::Failed, f.start).await;
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(
        saved.recovery,
        calm_truth::db::DaemonRecovery::Recorded { .. }
    ));
    f.repo.pool().close().await;
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert!(f.member.alive() && f.unrelated.alive());
    f.repo.db().close().await;
    let url = format!(
        "sqlite://{}?mode=rwc",
        f.root.path().join("recovery.db").display()
    );
    f.repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    f.rebuild_owner();
    assert!(f.daemon.start_or_takeover().await.is_err());
    let current = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(current.recovery, saved.recovery);
    assert_eq!(current.pid, saved.pid);
    assert_eq!(current.pgid, saved.pgid);
    assert!(f.paths.paths().socket.exists());
    assert!(f.member.alive() && f.unrelated.alive());
}

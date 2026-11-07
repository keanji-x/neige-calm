//! Each ordinary listener failure must retain the scope observed at that boundary.
use super::spawn_obligation_tests::{SpawnFixture, reopen, wait_reaped};
use super::*;
use calm_truth::db::DaemonRecovery;

fn pause_finalization(
    owner: &SharedCodexAppServer,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered, paused) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    *owner.before_failure_finalize.lock().unwrap() = Some((entered, resume));
    (paused, release)
}

async fn receive<T>(rx: tokio::sync::oneshot::Receiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap()
}

async fn fail_new_listener<T>(
    f: &Fixture,
    g: &mut Fixture,
    release_old: tokio::sync::oneshot::Sender<()>,
    old: tokio::task::JoinHandle<Result<T>>,
) -> DaemonRecovery {
    std::fs::remove_file(&f.paths.paths().socket).unwrap();
    std::fs::rename(&g.paths.paths().socket, &f.paths.paths().socket).unwrap();
    let pid = g.leader.pid();
    f.repo.db().writer().execute(sqlx::query(&format!(
        "CREATE TRIGGER fail_listener_publish BEFORE UPDATE ON codex_backend_daemons WHEN NEW.pid = {pid} BEGIN SELECT RAISE(ABORT, 'forced listener publish failure'); END",
    ))).await.unwrap();
    let (entered, paused) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    *f.daemon
        .recovery_hooks
        .after_listener_publish_failure
        .lock()
        .unwrap() = Some((entered, resume));
    release_old.send(()).unwrap();
    // The first transition may settle H, but must never reserve/capture G.
    // Wait for its failure finalizer to return without a listener publication.
    assert!(old.await.unwrap().is_err());
    let before = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(before.recovery, DaemonRecovery::Clear);
    assert!(g.leader.alive() && g.member.alive());
    let owner = f.daemon.clone();
    let next = tokio::spawn(async move { owner.start_or_takeover().await });
    receive(paused).await;
    let pending = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(pending.recovery, DaemonRecovery::Pending { .. }));
    assert!(pending.pid.is_none());
    // The failed transaction has really returned. Withdraw only that fault, then
    // make G's leader absent while its separately owned TERM-ignoring M stays live.
    f.repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER fail_listener_publish"))
        .await
        .unwrap();
    g.leader.0.kill().unwrap();
    g.leader.0.wait().unwrap();
    release.send(()).unwrap();
    assert!(next.await.unwrap().is_err());
    pending.recovery
}

async fn assert_recoverable(f: &mut Fixture, g: &mut Fixture, pending: DaemonRecovery) {
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(saved.recovery.scope_id(), pending.scope_id());
    assert!(
        matches!(saved.recovery, DaemonRecovery::Recorded { .. }),
        "ordinary failure lost the observed G: {saved:?}"
    );
    assert_eq!(
        (saved.pid, saved.pgid, saved.process_start_time),
        (Some(g.leader.pid()), Some(g.leader.pid()), Some(g.start))
    );
    assert_eq!(saved.boot_id, read_boot_id());
    f.config.codex_bin = "/missing-native-recovery-codex".into();
    reopen(f).await;
    for _ in 0..2 {
        assert!(f.daemon.start_or_takeover().await.is_err());
        let current = f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(current.recovery, saved.recovery);
        assert_eq!(current.pid, saved.pid);
        assert!(f.paths.paths().socket.exists());
        assert!(g.member.alive() && f.unrelated.alive() && g.unrelated.alive());
    }
    g.member.0.kill().unwrap();
    g.member.0.wait().unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err()); // missing executable after same-id settlement
    let cleared = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(cleared.recovery, DaemonRecovery::Clear);
    assert!(cleared.pid.is_none() && cleared.pgid.is_none());
    assert!(!f.paths.paths().socket.exists());
    assert!(f.unrelated.alive() && g.unrelated.alive());
}

#[tokio::test]
async fn native_macos_recovery_finalizer_new_listener_first_publish_failure() {
    let mut s = SpawnFixture::new().await;
    let (paused, release) = pause_finalization(&s.f.daemon);
    let owner = s.f.daemon.clone();
    let start = tokio::spawn(async move { owner.start_or_takeover().await });
    receive(paused).await;
    wait_reaped(s.observed().pid).await;
    assert!(macos_process::group_empty(s.observed().pgid));
    let mut g = Fixture::with_pollution(false).await;
    let pending = fail_new_listener(&s.f, &mut g, release, start).await;
    assert_eq!(s.spawn_count(), 1);
    assert_recoverable(&mut s.f, &mut g, pending).await;
}

#[tokio::test]
async fn native_macos_recovery_guard_new_listener_first_publish_failure() {
    let mut f = Fixture::with_storage(false, true).await;
    f.persist(SharedDaemonState::Running, f.start).await;
    // Reuse the real WebSocket fixture for the installed client. H's process
    // identity is native and all replacement/guard work uses production methods.
    let (client, _peer, forwarder) = native_owner_tests::connect_unpublished(&f.daemon).await;
    let runtime = SharedDaemonRuntime {
        pid: f.leader.pid(),
        pgid: f.leader.pid(),
        boot_id: read_boot_id().unwrap(),
        process_start_time: f.start,
        started_at: now_ms(),
    };
    f.daemon
        .start_new_process_typestate(|_| async move {
            Ok(LaunchedSharedDaemon {
                recovery: DaemonRecovery::LegacyUntracked,
                child: None,
                client,
                runtime,
                replay: ConnectionReplay::Complete,
                watcher: SupervisorWatcher {
                    kind: WatcherKind::TakenOverPid,
                    handle: forwarder,
                },
            })
        })
        .await
        .unwrap();
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    let unknown = macos_process::GroupObservationError::inject(f.leader.pid());
    let (paused, release) = pause_finalization(&f.daemon);
    let owner = f.daemon.clone();
    let start = tokio::spawn(async move {
        owner
            .transition_replace(
                "fixture replacement",
                ReplacePrecondition::Always,
                true,
                None,
            )
            .await
    });
    receive(paused).await; // the real replacement's first cleanup could not certify H
    drop(unknown);
    let mut g = Fixture::with_pollution(false).await;
    let pending = fail_new_listener(&f, &mut g, release, start).await;
    assert_recoverable(&mut f, &mut g, pending).await;
}

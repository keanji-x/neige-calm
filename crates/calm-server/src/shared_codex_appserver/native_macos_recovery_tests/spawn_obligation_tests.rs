//! Exercise owned spawn and cancellation through the detached production transition.
use super::*;
use calm_truth::db::{DaemonRecovery, SharedCodexDaemonRecord};
use plugin::proc_identity::{MacProcess, observe_macos_group, read_macos_process};
use std::os::unix::fs::PermissionsExt;

pub(super) struct SpawnFixture {
    pub(super) f: Fixture,
    captured: Arc<std::sync::Mutex<Option<MacProcess>>>,
}
impl SpawnFixture {
    pub(super) async fn new() -> Self {
        let mut f = Fixture::with_storage(false, true).await;
        f.leader.0.kill().unwrap();
        f.leader.0.wait().unwrap();
        f.member.0.kill().unwrap();
        f.member.0.wait().unwrap();
        std::fs::remove_file(&f.paths.paths().socket).unwrap();
        let executable = f.root.path().join("owned-producer");
        // A real command with an observable entry and listener. TERM is ignored so
        // cancellation's Drop belt cannot accidentally stand in for an empty proof.
        std::fs::write(
            &executable,
            r#"#!/usr/bin/python3
import os,pathlib,signal,socket,sys,time
base = pathlib.Path(__file__)
def stop(*_):
    if not base.with_suffix('.hold-term').exists(): return
    base.with_suffix('.term-seen').touch()
    while not base.with_suffix('.release-term').exists(): time.sleep(0.005)
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
with open(pathlib.Path(__file__).with_suffix('.started'), 'a') as f:
    f.write(str(os.getpid()) + '\n')
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[3][7:])
s.listen()
while True: time.sleep(1)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        f.config.codex_bin = executable.to_str().unwrap().into();
        f.rebuild_owner();
        let fixture = Self {
            f,
            captured: Arc::new(std::sync::Mutex::new(None)),
        };
        fixture.arm_capture();
        fixture
    }
    fn arm_capture(&self) {
        let captured = self.captured.clone();
        let socket = self.f.paths.paths().socket.clone();
        *self.f.daemon.before_identity_capture.lock().unwrap() = Some(Box::new(move |pid| {
            // Keep a birth-checked cleanup handle before any fallible assertion.
            *captured.lock().unwrap() = Some(read_macos_process(pid).unwrap().unwrap());
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !socket.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "owned producer did not bind"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }));
    }
    pub(super) fn spawn_count(&self) -> usize {
        std::fs::read_to_string(self.f.root.path().join("owned-producer.started"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
    pub(super) fn observed(&self) -> MacProcess {
        self.captured.lock().unwrap().unwrap()
    }
    pub(super) fn capture_owned(&self, pid: i32) {
        *self.captured.lock().unwrap() = Some(read_macos_process(pid).unwrap().unwrap());
    }
    async fn kill_owned(&self) {
        let process = self.observed();
        assert_eq!(read_macos_process(process.pid).unwrap(), Some(process));
        assert_eq!(unsafe { libc::kill(process.pid, libc::SIGKILL) }, 0);
        wait_reaped(process.pid).await;
        assert!(macos_process::group_empty(process.pgid));
    }
}
impl Drop for SpawnFixture {
    fn drop(&mut self) {
        if let Some(owned) = *self.captured.lock().unwrap()
            && let Ok(Some(current)) = read_macos_process(owned.pid)
            && current.start_time == owned.start_time
            && current.pgid == owned.pgid
        {
            // Only the directly spawned Python process is owned; no inferred members.
            unsafe {
                libc::kill(owned.pid, libc::SIGKILL);
            }
        }
    }
}

pub(super) async fn wait_reaped(pid: i32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while read_macos_process(pid).unwrap().is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "owned child was not reaped"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) async fn reopen(f: &mut Fixture) {
    f.repo.db().close().await;
    let url = format!(
        "sqlite://{}?mode=rwc",
        f.root.path().join("recovery.db").display()
    );
    f.repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    f.rebuild_owner();
}

async fn assert_pending_reopen(f: &mut Fixture, saved: &SharedCodexDaemonRecord) {
    assert!(matches!(saved.recovery, DaemonRecovery::Pending { .. }));
    assert!(saved.pid.is_none() && saved.pgid.is_none());
    reopen(f).await;
    for _ in 0..2 {
        assert!(f.daemon.start_or_takeover().await.is_err());
        let current = f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(current.recovery, saved.recovery);
        assert!(current.pid.is_none() && current.pgid.is_none());
        assert!(
            f.paths.paths().socket.exists(),
            "Pending cannot authorize unlink"
        );
        assert!(f.unrelated.alive());
    }
}

pub(super) fn pause_acquisition(
    owner: &SharedCodexAppServer,
) -> (
    tokio::sync::oneshot::Receiver<i32>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered, acquired) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    *owner.recovery_hooks.after_acquire.lock().unwrap() = Some((entered, resume));
    (acquired, release)
}

#[tokio::test]
async fn native_macos_recovery_owned_spawn_begin_failure_has_no_effects() {
    let mut s = SpawnFixture::new().await;
    let before = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
    s.f.repo.db().writer().execute(sqlx::query("CREATE TRIGGER fail_owned_begin BEFORE UPDATE ON codex_backend_daemons WHEN NEW.recovery_phase='pending' BEGIN SELECT RAISE(ABORT,'begin refused'); END")).await.unwrap();
    assert!(s.f.daemon.start_or_takeover().await.is_err());
    assert_eq!(s.spawn_count(), 0, "the prepared executable must never run");
    assert!(s.captured.lock().unwrap().is_none());
    assert!(!s.f.paths.paths().socket.exists());
    assert!(
        UnixStream::connect(&s.f.paths.paths().socket)
            .await
            .is_err()
    );
    let after = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.recovery, before.recovery);
    assert_eq!(
        (
            after.pid,
            after.pgid,
            after.process_start_time,
            after.boot_id
        ),
        (
            before.pid,
            before.pgid,
            before.process_start_time,
            before.boot_id
        )
    );
    assert_eq!(
        (after.sock_path, after.codex_home_path),
        (before.sock_path, before.codex_home_path)
    );
    assert!(s.f.unrelated.alive());
    // Removing only the DB fault must permit the exact prepared command to execute.
    s.f.repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER fail_owned_begin"))
        .await
        .unwrap();
    assert!(s.f.daemon.start_or_takeover().await.is_err()); // real child, readiness timeout
    assert_eq!(s.spawn_count(), 1);
    wait_reaped(s.observed().pid).await;
    assert!(macos_process::group_empty(s.observed().pgid));
}

async fn starting_write_failure(repeated: bool) {
    let mut s = SpawnFixture::new().await;
    let (acquired, release_acquire) = pause_acquisition(&s.f.daemon);
    let (entered, finalizing) = tokio::sync::oneshot::channel();
    let (release_finalize, resume) = tokio::sync::oneshot::channel();
    *s.f.daemon.before_failure_finalize.lock().unwrap() = Some((entered, resume));
    s.f.repo.db().writer().execute(sqlx::query("CREATE TRIGGER fail_starting_publish BEFORE UPDATE ON codex_backend_daemons WHEN NEW.pid IS NOT NULL BEGIN SELECT RAISE(ABORT,'Starting publication refused'); END")).await.unwrap();
    let owner = s.f.daemon.clone();
    let caller = tokio::spawn(async move { owner.start_or_takeover().await });
    let pid = tokio::time::timeout(Duration::from_secs(10), acquired)
        .await
        .unwrap()
        .unwrap();
    let actual = s.observed();
    assert_eq!(actual.pid, pid);
    let boot = read_boot_id().expect("native current boot UUID");
    let unknown = macos_process::GroupObservationError::inject(pid);
    release_acquire.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), finalizing)
        .await
        .unwrap()
        .unwrap();
    // This is after the actual Starting SQL error and the actual guard's graceful reap.
    // Its void return is no emptiness receipt: the finalizer still cannot observe G.
    wait_reaped(pid).await;
    assert!(observe_macos_group(pid).unwrap().is_empty());
    assert!(!macos_process::group_empty(pid));
    let pending = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(pending.recovery, DaemonRecovery::Pending { .. }));
    assert!(pending.pid.is_none());
    assert!(s.f.paths.paths().socket.exists());
    if !repeated {
        s.f.repo
            .db()
            .writer()
            .execute(sqlx::query("DROP TRIGGER fail_starting_publish"))
            .await
            .unwrap();
    }
    release_finalize.send(()).unwrap();
    let error = caller
        .await
        .unwrap()
        .expect_err("Starting publication failed");
    assert!(
        matches!(
            s.f.daemon.core.lock().await.state,
            SupervisorState::Failed {
                class: FailureClass::Unreconciled,
                ..
            }
        ),
        "unknown scope must retain the reconciliation-only heal lane"
    );
    assert_eq!(
        error.code(),
        "db_error",
        "preserve the original publication error"
    );
    let saved = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(saved.recovery.scope_id(), pending.recovery.scope_id());
    assert_eq!(s.spawn_count(), 1);
    assert!(s.f.paths.paths().socket.exists());
    assert!(s.f.unrelated.alive());
    if repeated {
        assert_pending_reopen(&mut s.f, &saved).await;
        s.f.repo
            .db()
            .writer()
            .execute(sqlx::query("DROP TRIGGER fail_starting_publish"))
            .await
            .unwrap();
        drop(unknown);
        // Neither restored observation nor a now-writable DB can reconstruct unsaved facts.
        assert!(s.f.daemon.start_or_takeover().await.is_err());
        assert_eq!(
            s.f.repo
                .shared_daemon_runtime_get("default")
                .await
                .unwrap()
                .recovery,
            pending.recovery
        );
        assert!(s.f.paths.paths().socket.exists());
        assert_eq!(s.spawn_count(), 1);
    } else {
        assert!(matches!(saved.recovery, DaemonRecovery::Recorded { .. }));
        assert_eq!(
            (
                saved.pid,
                saved.pgid,
                saved.process_start_time,
                saved.boot_id
            ),
            (
                Some(pid),
                Some(actual.pgid),
                Some(actual.start_time),
                Some(boot)
            )
        );
        reopen(&mut s.f).await;
        assert!(s.f.daemon.start_or_takeover().await.is_err());
        let retained = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(retained.recovery.scope_id(), pending.recovery.scope_id());
        assert_eq!(retained.pid, Some(pid));
        assert!(s.f.paths.paths().socket.exists());
        assert_eq!(s.spawn_count(), 1);
        drop(unknown);
        // A later real empty observation and successful same-id settlement permit reuse.
        s.arm_capture();
        assert!(s.f.daemon.start_or_takeover().await.is_err()); // second real spawn times out
        assert_eq!(s.spawn_count(), 2);
        wait_reaped(s.observed().pid).await;
        let settled = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(settled.recovery, DaemonRecovery::Clear);
        assert!(settled.pid.is_none() && settled.pgid.is_none());
        assert!(
            s.f.paths.paths().socket.exists(),
            "finalization must leave the stale endpoint for an explicit entry"
        );
        assert!(s.f.unrelated.alive());
        s.arm_capture();
        assert!(s.f.daemon.start_or_takeover().await.is_err());
        assert_eq!(
            s.spawn_count(),
            3,
            "the next entry must remove the stale endpoint and execute exactly once"
        );
        wait_reaped(s.observed().pid).await;
        assert!(macos_process::group_empty(s.observed().pgid));
        assert_eq!(
            s.f.repo
                .shared_daemon_runtime_get("default")
                .await
                .unwrap()
                .recovery,
            DaemonRecovery::Clear
        );
    }
    assert!(s.f.unrelated.alive());
}

#[tokio::test]
async fn native_macos_recovery_starting_first_write_failure_preserves_observed_until_known_empty() {
    starting_write_failure(false).await;
}
#[tokio::test]
async fn native_macos_recovery_starting_repeated_write_failure_keeps_pending_after_reopen() {
    starting_write_failure(true).await;
}

#[tokio::test]
async fn native_macos_recovery_cancelled_acquired_peer_keeps_pending_after_reopen() {
    let mut f = Fixture::with_storage(false, true).await;
    let (acquired, _release) = pause_acquisition(&f.daemon);
    let owner = f.daemon.clone();
    let caller = tokio::spawn(async move { owner.start_or_takeover().await });
    let pid = tokio::time::timeout(Duration::from_secs(10), acquired)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pid, f.leader.pid());
    assert!(f.daemon.abort_detached_spawn_transition_for_test());
    assert!(caller.await.unwrap().is_err());
    assert!(f.leader.alive() && f.member.alive());
    let saved = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_pending_reopen(&mut f, &saved).await;
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    assert!(macos_process::group_empty(pid));
    assert_pending_reopen(&mut f, &saved).await;
}

#[tokio::test]
async fn native_macos_recovery_cancelled_acquired_child_keeps_pending_after_reopen() {
    let mut s = SpawnFixture::new().await;
    let (acquired, _release) = pause_acquisition(&s.f.daemon);
    let owner = s.f.daemon.clone();
    let caller = tokio::spawn(async move { owner.start_or_takeover().await });
    let pid = tokio::time::timeout(Duration::from_secs(10), acquired)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.spawn_count(), 1);
    assert_eq!(s.observed().pid, pid);
    assert!(s.f.daemon.abort_detached_spawn_transition_for_test());
    assert!(caller.await.unwrap().is_err());
    assert_eq!(
        read_macos_process(pid).unwrap(),
        Some(s.observed()),
        "Drop TERM is not emptiness"
    );
    let saved = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_pending_reopen(&mut s.f, &saved).await;
    s.kill_owned().await;
    assert_pending_reopen(&mut s.f, &saved).await;
    assert_eq!(s.spawn_count(), 1);
}

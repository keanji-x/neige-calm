#![cfg(test)]
//! Native recovery regressions use only independently owned test processes.
use super::*;
use crate::db::sqlite::SqlxRepo;
use crate::db::{RepoOutOfDomain, RepoRead};
use clap::Parser;
use std::os::unix::{ffi::OsStrExt, process::CommandExt};
use std::process::{Child as StdChild, Command};

mod authority_tests;
mod finalization_tests;
mod listener_tests;
mod obligation_tests;
mod owned_member_tests;
mod spawn_obligation_tests;

struct Owned(StdChild);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().expect("reap owned native recovery process");
    }
}
impl Owned {
    fn pid(&self) -> i32 {
        self.0.id() as i32
    }
    fn alive(&mut self) -> bool {
        self.0.try_wait().unwrap().is_none()
    }
}

struct Fixture {
    root: tempfile::TempDir,
    config: Config,
    leader: Owned,
    start: u64,
    member: Owned,
    unrelated: Owned,
    daemon: Arc<SharedCodexAppServer>,
    repo: Arc<SqlxRepo>,
    paths: VerifiedRuntimePaths,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_pollution(true).await
    }
    async fn with_pollution(polluted: bool) -> Self {
        Self::with_storage(polluted, false).await
    }
    async fn with_storage(polluted: bool, file_backed: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = Config::parse_from([
            "calm-server",
            "--data-dir",
            root.path().to_str().unwrap(),
            "--codex-bin",
            "/missing-native-recovery-codex",
            "--shared-codex-appserver-stop-grace-secs",
            "1",
            "--shared-codex-appserver-start-timeout-secs",
            "0",
            "--shared-codex-appserver-restart-initial-delay-ms",
            "60000",
            "--shared-codex-appserver-restart-max-delay-ms",
            "120000",
        ]);
        let home = SharedCodexHome::new(root.path().join("codex-home"));
        home.seed_from(None).unwrap();
        if polluted {
            std::fs::write(
                home.path().join("config.toml"),
                "[mcp_servers.evil]\ncommand = '/missing'\n",
            )
            .unwrap();
        }
        let url = if file_backed {
            format!(
                "sqlite://{}?mode=rwc",
                root.path().join("recovery.db").display()
            )
        } else {
            "sqlite::memory:".to_string()
        };
        let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
        let daemon = SharedCodexAppServer::new(&config, Arc::new(home), repo.clone());
        let paths = daemon.verify_backend_isolation().await.unwrap();
        std::fs::create_dir_all(&paths.paths().run).unwrap();
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c", "import socket,signal,sys,time,pathlib\ndef stop(*_):\n if pathlib.Path(sys.argv[2]).exists():\n  pathlib.Path(sys.argv[3]).touch()\n  while not pathlib.Path(sys.argv[4]).exists(): time.sleep(0.005)\n sys.exit(0)\nsignal.signal(signal.SIGTERM,stop)\ns=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.listen(); clients=[]\nwhile True:\n c,_=s.accept(); clients.append(c); c.sendall(b'not-json\\n')\n"])
            .arg(&paths.paths().socket)
            .arg(root.path().join("hold-term"))
            .arg(root.path().join("term-seen"))
            .arg(root.path().join("release-term"))
            .process_group(0);
        let leader = Owned(command.spawn().unwrap());
        let ready = root.path().join("member-ready");
        let member = Owned(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "trap '' TERM; echo ready > \"$1\"; exec sleep 60",
                    "sh",
                ])
                .arg(&ready)
                .process_group(leader.pid())
                .spawn()
                .unwrap(),
        );
        let unrelated = Owned(
            Command::new("sleep")
                .arg("60")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        for _ in 0..200 {
            if ready.exists() && paths.paths().socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready.exists() && paths.paths().socket.exists());
        Self {
            root,
            config,
            start: read_proc_start_time(leader.pid()).unwrap(),
            leader,
            member,
            unrelated,
            daemon,
            repo,
            paths,
        }
    }
    fn rebuild_owner(&mut self) {
        self.daemon = SharedCodexAppServer::new(
            &self.config,
            Arc::new(SharedCodexHome::new(self.root.path().join("codex-home"))),
            self.repo.clone(),
        );
    }
    async fn persist(&self, state: SharedDaemonState, start: u64) {
        self.persist_identity(
            state,
            Some(start),
            Some(read_boot_id().expect("native boot UUID required")),
        )
        .await;
    }
    async fn persist_identity(
        &self,
        state: SharedDaemonState,
        start: Option<u64>,
        boot: Option<String>,
    ) {
        self.repo
            .shared_daemon_runtime_set(
                "default",
                SharedCodexDaemonUpdate {
                    recovery: calm_truth::db::RecoveryWrite::Legacy,
                    state,
                    pid: Some(self.leader.pid()),
                    pgid: Some(self.leader.pid()),
                    paths_are_legacy_display: false,
                    sock_path: Some(self.paths.paths().socket.as_os_str().as_bytes().to_vec()),
                    codex_home_path: Some(
                        self.root
                            .path()
                            .join("codex-home")
                            .as_os_str()
                            .as_bytes()
                            .to_vec(),
                    ),
                    process_start_time: start,
                    boot_id: boot,
                    started_at: Some(now_ms()),
                    last_error: None,
                    increment_restart_count: false,
                    daemon_env_signature: None,
                },
            )
            .await
            .unwrap();
    }
    async fn assert_fenced(&self) {
        let record = self
            .repo
            .shared_daemon_runtime_get("default")
            .await
            .unwrap();
        assert_eq!(record.state, SharedDaemonState::Failed);
        assert_eq!(record.pid, Some(self.leader.pid()));
        assert_eq!(record.pgid, Some(self.leader.pid()));
        assert_eq!(record.process_start_time, Some(self.start));
        assert_eq!(record.boot_id, read_boot_id());
        assert!(
            record
                .last_error
                .as_deref()
                .unwrap()
                .starts_with("unreconciled")
        );
        assert!(self.paths.paths().socket.exists());
    }
}

#[tokio::test]
async fn native_macos_recovery_stale_listener_cleans_term_ignoring_member() {
    let mut f = Fixture::new().await;
    reap_listener_if_alive(&f.daemon, &f.paths, Duration::from_millis(100))
        .await
        .unwrap();
    assert!(
        !f.member.alive(),
        "leader exit must not leave its TERM-ignoring member alive"
    );
    assert!(
        f.unrelated.alive(),
        "unrelated process must not receive a signal"
    );

    // A new member arriving after capture cannot inherit signal authority. The real
    // startup failure writer must preserve the listener identity, including on retry.
    let f = Fixture::new().await;
    std::fs::write(f.root.path().join("hold-term"), "").unwrap();
    let cleanup = f.daemon.start_new_process_typestate(|_| async {
        reap_listener_if_alive(&f.daemon, &f.paths, Duration::from_secs(5)).await?;
        Err(CalmError::CodexAppServer(
            "unexpected successful cleanup".into(),
        ))
    });
    let join_late = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !f.root.path().join("term-seen").exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "listener did not receive TERM"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        f.assert_fenced().await; // identity was durable before the first signal
        let late = Owned(
            Command::new("sleep")
                .arg("60")
                .process_group(f.leader.pid())
                .spawn()
                .unwrap(),
        );
        std::fs::write(f.root.path().join("release-term"), "").unwrap();
        late
    };
    let (result, mut late) = tokio::join!(cleanup, join_late);
    assert!(result.is_err());
    f.assert_fenced().await;
    assert!(
        late.alive(),
        "a later group scan must not grant signal authority"
    );
    for _ in 0..2 {
        assert!(f.daemon.start_or_takeover().await.is_err());
        f.assert_fenced().await;
        assert!(late.alive());
    }
}

#[tokio::test]
async fn native_macos_recovery_persisted_runtime_cleans_term_ignoring_member() {
    for (polluted, state) in [
        (true, SharedDaemonState::Running),
        (true, SharedDaemonState::Starting),
        (true, SharedDaemonState::Failed),
        (false, SharedDaemonState::Running),
        (false, SharedDaemonState::Starting),
        (false, SharedDaemonState::Failed),
    ] {
        let mut f = Fixture::with_pollution(polluted).await;
        f.persist(state, read_proc_start_time(f.leader.pid()).unwrap())
            .await;
        assert!(f.daemon.start_or_takeover().await.is_err()); // polluted home or the intentionally missing executable
        assert!(
            !f.member.alive(),
            "persisted recovery must settle the original group"
        );
        assert!(f.unrelated.alive());
        assert!(
            f.repo
                .shared_daemon_runtime_get("default")
                .await
                .unwrap()
                .pid
                .is_none()
        );
    }
}

#[tokio::test]
async fn native_macos_recovery_absent_leader_fences_retries_without_signaling_members() {
    for (polluted, state) in [
        (true, SharedDaemonState::Running),
        (true, SharedDaemonState::Starting),
        (true, SharedDaemonState::Failed),
        (false, SharedDaemonState::Running),
        (false, SharedDaemonState::Starting),
        (false, SharedDaemonState::Failed),
    ] {
        let mut f = Fixture::with_pollution(polluted).await;
        f.persist(state, read_proc_start_time(f.leader.pid()).unwrap())
            .await;
        f.leader.0.kill().unwrap();
        f.leader.0.wait().unwrap();
        for _ in 0..2 {
            assert!(f.daemon.start_or_takeover().await.is_err());
            f.assert_fenced().await;
            assert!(
                f.member.alive(),
                "a scan without leader ownership grants no signal authority"
            );
            assert!(f.unrelated.alive());
        }
        assert!(
            f.daemon
                .start_new_process_typestate(|_| async {
                    Err(CalmError::CodexAppServer(
                        "injected pre-spawn failure".into(),
                    ))
                })
                .await
                .is_err()
        );
        f.assert_fenced().await;
    }
}

#[tokio::test]
async fn native_macos_recovery_partial_identity_retains_surviving_group() {
    for polluted in [false, true] {
        for state in [
            SharedDaemonState::Running,
            SharedDaemonState::Starting,
            SharedDaemonState::Failed,
        ] {
            for (has_start, has_boot) in [(false, true), (true, false), (false, false)] {
                let mut f = Fixture::with_pollution(polluted).await;
                let start = has_start.then_some(f.start);
                let boot = has_boot.then(|| read_boot_id().expect("native boot UUID required"));
                f.persist_identity(state, start, boot.clone()).await;
                f.leader.0.kill().unwrap();
                f.leader.0.wait().unwrap();
                for _ in 0..2 {
                    assert!(f.daemon.start_or_takeover().await.is_err());
                    assert!(
                        f.daemon
                            .start_new_process_typestate(|_| async {
                                Err(CalmError::CodexAppServer("pre-spawn failure".into()))
                            })
                            .await
                            .is_err()
                    );
                    let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
                    assert_eq!(record.state, SharedDaemonState::Failed);
                    assert_eq!(record.pid, Some(f.leader.pid()));
                    assert_eq!(record.pgid, Some(f.leader.pid()));
                    assert_eq!(record.process_start_time, start);
                    assert_eq!(record.boot_id, boot);
                    assert!(!persisted_process_proven_absent(&record));
                    assert!(f.paths.paths().socket.exists());
                    assert!(
                        f.member.alive(),
                        "partial identity grants no signal authority"
                    );
                    assert!(f.unrelated.alive());
                }
                // Once the independently owned member is gone, observation can release
                // the fence without ever signaling a process from the partial identity.
                f.member.0.kill().unwrap();
                f.member.0.wait().unwrap();
                assert!(f.daemon.start_or_takeover().await.is_err());
                assert!(
                    f.repo
                        .shared_daemon_runtime_get("default")
                        .await
                        .unwrap()
                        .pid
                        .is_none()
                );
                assert!(f.unrelated.alive());
            }
        }
    }
}

#[tokio::test]
async fn native_macos_recovery_mismatched_identity_never_signals() {
    let mut f = Fixture::new().await;
    f.persist(
        SharedDaemonState::Running,
        read_proc_start_time(f.leader.pid()).unwrap() + 1,
    )
    .await;
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert!(f.leader.alive());
    assert!(f.member.alive());
    assert!(f.unrelated.alive());
}

#[tokio::test]
async fn native_macos_recovery_turnover_fences_full_and_partial_until_only_zombies_remain() {
    for polluted in [false, true] {
        for state in [
            SharedDaemonState::Running,
            SharedDaemonState::Starting,
            SharedDaemonState::Failed,
        ] {
            for (has_start, has_boot) in
                [(true, true), (false, true), (true, false), (false, false)]
            {
                let mut f = Fixture::with_pollution(polluted).await;
                let start = has_start.then_some(f.start);
                let boot = has_boot.then(|| read_boot_id().expect("native boot UUID required"));
                f.persist_identity(state, start, boot.clone()).await;
                f.leader.0.kill().unwrap();
                f.leader.0.wait().unwrap();
                let mut late = Owned(
                    Command::new("/bin/sleep")
                        .arg("60")
                        .process_group(f.leader.pid())
                        .spawn()
                        .unwrap(),
                );
                f.member.0.kill().unwrap();
                wait_for_owned_zombie(f.member.pid()).await;

                // The observer's in-enumeration race is exercised in plugin's native
                // tests. Here its consumers must retain every tuple shape through the
                // real failure writer, guard/takeover/reconcile paths and retries.
                assert!(
                    f.daemon
                        .start_new_process_typestate(|_| async {
                            Err(CalmError::CodexAppServer("pre-spawn failure".into()))
                        })
                        .await
                        .is_err()
                );
                for _ in 0..2 {
                    assert!(f.daemon.start_or_takeover().await.is_err());
                    let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
                    assert_eq!(record.state, SharedDaemonState::Failed);
                    assert_eq!(record.pid, Some(f.leader.pid()));
                    assert_eq!(record.pgid, Some(f.leader.pid()));
                    assert_eq!(record.process_start_time, start);
                    assert_eq!(record.boot_id, boot);
                    assert!(
                        record
                            .last_error
                            .as_deref()
                            .unwrap()
                            .starts_with("unreconciled")
                    );
                    assert!(!persisted_process_proven_absent(&record));
                    assert!(f.paths.paths().socket.exists());
                    assert!(
                        late.alive(),
                        "later observation must not add signal targets"
                    );
                    assert!(f.unrelated.alive());
                }

                // Leave both member identities pinned as zombies. Recovery must be
                // possible without requiring their independent parents to reap them.
                late.0.kill().unwrap();
                wait_for_owned_zombie(late.pid()).await;
                let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
                assert!(persisted_process_proven_absent(&record));
                assert!(f.daemon.start_or_takeover().await.is_err());
                let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
                assert!(record.pid.is_none() && record.pgid.is_none());
                assert!(record.process_start_time.is_none() && record.boot_id.is_none());
                assert!(f.unrelated.alive());
            }
        }
    }
}

async fn wait_for_owned_zombie(pid: i32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !plugin::proc_identity::read_macos_process(pid)
        .unwrap()
        .unwrap()
        .is_zombie
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "owned child did not become zombie"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[tokio::test]
async fn native_macos_recovery_capture_zombie_retains_observed_scope() {
    let mut f = Fixture::with_pollution(false).await;
    let pid = f.leader.pid();
    *f.daemon.before_identity_capture.lock().unwrap() = Some(Box::new(move |peer| {
        assert_eq!(peer, pid);
        // The peer credential has been read; the independently owned Child stays pinned.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !proc_pid_is_zombie(pid) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }));
    assert!(
        f.daemon
            .start_new_process_typestate(|_| async {
                reap_listener_if_alive(&f.daemon, &f.paths, Duration::from_millis(20)).await?;
                Err(CalmError::CodexAppServer(
                    "cleanup unexpectedly succeeded".into(),
                ))
            })
            .await
            .is_err()
    );
    f.assert_fenced().await;
    assert!(f.member.alive() && f.unrelated.alive());
    f.leader.0.wait().unwrap();
    f.rebuild_owner();
    for _ in 0..2 {
        assert!(f.daemon.start_or_takeover().await.is_err());
        f.assert_fenced().await;
        assert!(f.member.alive() && f.unrelated.alive());
    }
}

#[tokio::test]
async fn native_macos_recovery_invalid_boot_releases_only_after_group_absence() {
    for polluted in [false, true] {
        for state in [
            SharedDaemonState::Running,
            SharedDaemonState::Starting,
            SharedDaemonState::Failed,
        ] {
            for (boot, zero_start) in [("", false), ("not-a-uuid", false), ("", true)] {
                let mut f = Fixture::with_pollution(polluted).await;
                // The real producer historically wrote Some("") / Some(0); use its DB writer.
                f.persist_identity(
                    state,
                    Some(if zero_start { 0 } else { f.start }),
                    Some(boot.into()),
                )
                .await;
                assert!(f.daemon.start_or_takeover().await.is_err());
                assert!(f.leader.alive() && f.member.alive() && f.unrelated.alive());
                let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
                assert_eq!(record.boot_id.as_deref(), Some(boot));
                assert!(!persisted_process_proven_absent(&record));
                f.leader.0.kill().unwrap();
                f.leader.0.wait().unwrap();
                assert!(f.daemon.start_or_takeover().await.is_err());
                assert!(f.member.alive() && f.unrelated.alive());
                f.member.0.kill().unwrap();
                f.member.0.wait().unwrap();
                f.rebuild_owner();
                assert!(f.daemon.start_or_takeover().await.is_err());
                let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
                assert!(
                    record.pid.is_none() && record.pgid.is_none(),
                    "known empty group must be releasable: {record:?}"
                );
                assert!(record.process_start_time.is_none() && record.boot_id.is_none());
                assert!(f.unrelated.alive());
            }
        }
    }
}

#[tokio::test]
async fn native_macos_recovery_capture_read_failure_retains_scope_across_owner_rebuild() {
    use macos_process::CaptureFault;
    for fault in [CaptureFault::Boot, CaptureFault::Process] {
        let mut f = Fixture::with_pollution(false).await;
        *f.daemon.identity_capture_fault.lock().unwrap() = fault;
        assert!(
            f.daemon
                .start_new_process_typestate(|_| async {
                    reap_listener_if_alive(&f.daemon, &f.paths, Duration::from_millis(20)).await?;
                    Err(CalmError::CodexAppServer(
                        "cleanup unexpectedly succeeded".into(),
                    ))
                })
                .await
                .is_err()
        );
        let observed = f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(observed.pid, Some(f.leader.pid()));
        assert_eq!(
            observed.pgid,
            matches!(fault, CaptureFault::Boot).then_some(f.leader.pid())
        );
        assert_eq!(
            observed.process_start_time,
            matches!(fault, CaptureFault::Boot).then_some(f.start)
        );
        if matches!(fault, CaptureFault::Boot) {
            assert!(observed.boot_id.is_none());
        }
        assert!(f.leader.alive() && f.member.alive() && f.unrelated.alive());
        f.leader.0.kill().unwrap();
        f.leader.0.wait().unwrap();
        f.rebuild_owner();
        for _ in 0..2 {
            assert!(f.daemon.start_or_takeover().await.is_err());
            assert!(
                f.daemon
                    .start_new_process_typestate(|_| async {
                        Err(CalmError::CodexAppServer("later pre-spawn failure".into()))
                    })
                    .await
                    .is_err()
            );
            let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
            assert_eq!(
                FailedIdentity::retained(&record),
                FailedIdentity::retained(&observed)
            );
            assert!(!persisted_process_proven_absent(&record));
            assert!(f.paths.paths().socket.exists());
            assert!(f.member.alive() && f.unrelated.alive());
        }
        f.member.0.kill().unwrap();
        f.member.0.wait().unwrap();
        f.rebuild_owner();
        assert!(f.daemon.start_or_takeover().await.is_err());
        let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
        if matches!(fault, CaptureFault::Process) {
            assert_eq!(record.pid, observed.pid);
            assert_eq!(record.pgid, None);
            assert_eq!(
                f.daemon.resolve_persisted_shape(&f.paths, &record).await,
                ShapeResolution::Unreconciled {
                    needs_operator: true
                }
            );
            assert!(f.paths.paths().socket.exists());
        } else {
            assert!(record.pid.is_none() && record.pgid.is_none());
        }
        assert!(f.unrelated.alive());
    }
}

#[tokio::test]
async fn native_macos_recovery_producer_missing_metadata_keeps_guard_and_durable_scope() {
    use macos_process::CaptureFault;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicI32;
    for fault in [CaptureFault::Boot, CaptureFault::Process] {
        let mut f = Fixture::with_pollution(false).await;
        f.leader.0.kill().unwrap();
        f.leader.0.wait().unwrap();
        f.member.0.kill().unwrap();
        f.member.0.wait().unwrap();
        std::fs::remove_file(&f.paths.paths().socket).unwrap();
        let ready = f.root.path().join("producer-ready");
        let executable = f.root.path().join("owned-producer");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\ntrap '' TERM\necho ready > '{}'\nexec sleep 60\n",
                ready.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Arc::get_mut(&mut f.daemon).unwrap().codex_bin = executable.to_string_lossy().into_owned();
        *f.daemon.identity_capture_fault.lock().unwrap() = fault;
        let captured = Arc::new(AtomicI32::new(0));
        let hook_pid = captured.clone();
        *f.daemon.before_identity_capture.lock().unwrap() = Some(Box::new(move |pid| {
            hook_pid.store(pid, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !ready.exists() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }));
        let launch = f.daemon.start_new_process_reported(|_| async {
            let mut attempt = recovery::RecoveryAttempt::new();
            let result = f
                .daemon
                .launch_spawned_process(&f.paths, false, None, &mut attempt)
                .await;
            match result {
                Ok(launched) => Ok(launched),
                Err(error) => Err(f.daemon.finalize_recovery_failure(error, attempt).await),
            }
        });
        let observe = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            loop {
                let record = match f.repo.shared_daemon_runtime_get("default").await {
                    Ok(record) => record,
                    Err(error) => return Err(error),
                };
                // Begin also writes Starting, before a child exists. Observe the
                // published facts rather than racing that durable Pending intent.
                if record.state == SharedDaemonState::Starting
                    && matches!(
                        record.recovery,
                        calm_truth::db::DaemonRecovery::Recorded { .. }
                    )
                {
                    let absent = persisted_process_proven_absent(&record);
                    return Ok(Some((record, absent)));
                }
                if tokio::time::Instant::now() >= deadline {
                    return Ok(None);
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        // Finish the real launch error cleanup before any observation assertion
        // can unwind and cancel its owned Child guard's graceful reap.
        let (result, observed) = tokio::join!(launch, observe);
        assert!(result.is_err());
        let pid = captured.load(Ordering::SeqCst);
        assert!(pid > 1);
        let (observed, absent) = observed
            .expect("read durable Starting observation")
            .expect("missing durable Starting observation");
        assert_eq!(observed.pid, Some(pid));
        assert_eq!(observed.pgid, observed.pid);
        match fault {
            CaptureFault::Boot => assert!(observed.boot_id.is_none()),
            CaptureFault::Process => assert!(observed.process_start_time.is_none()),
            CaptureFault::None => unreachable!(),
        }
        assert!(!absent);
        assert!(
            plugin::proc_identity::read_macos_process(pid)
                .unwrap()
                .is_none(),
            "owned Child guard must reap after capture failure"
        );
        assert!(macos_process::group_empty(pid));
        let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert!(record.pid.is_none() && record.pgid.is_none());
        assert!(record.process_start_time.is_none() && record.boot_id.is_none());
        assert!(f.unrelated.alive());
    }
}

#[tokio::test]
async fn native_macos_recovery_zero_start_with_native_boot_never_signals() {
    let mut f = Fixture::new().await;
    f.persist(SharedDaemonState::Running, 0).await;
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert!(f.leader.alive() && f.member.alive() && f.unrelated.alive());
    f.leader.0.kill().unwrap();
    f.leader.0.wait().unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    assert!(f.member.alive());
    f.member.0.kill().unwrap();
    f.member.0.wait().unwrap();
    assert!(f.daemon.start_or_takeover().await.is_err());
    let record = f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(record.pid.is_none() && record.pgid.is_none());
    assert!(f.unrelated.alive());
}

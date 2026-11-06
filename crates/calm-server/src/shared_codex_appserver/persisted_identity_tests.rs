//! Persisted process evidence must remain distinct from signal authority.
use super::*;
use crate::db::{RepoOutOfDomain, RepoRead};

fn record() -> crate::db::SharedCodexDaemonRecord {
    crate::db::SharedCodexDaemonRecord {
        state: SharedDaemonState::Failed,
        pid: None,
        pgid: None,
        paths_are_legacy_display: false,
        sock_path: None,
        codex_home_path: None,
        process_start_time: None,
        boot_id: None,
        started_at: None,
        updated_at: 0,
        restart_count: 0,
        last_error: None,
        daemon_env_signature: None,
    }
}

#[test]
fn persisted_unknown_and_zombie_never_discharge_identity_shapes() {
    for shape in 0..16 {
        let mut record = record();
        record.pid = (shape & 1 != 0).then_some(42);
        record.pgid = (shape & 2 != 0).then_some(42);
        record.process_start_time = (shape & 4 != 0).then_some(7);
        record.boot_id = (shape & 8 != 0).then(|| "boot".into());
        for observation in [
            ProcessObservation::Unknown,
            ProcessObservation::Present {
                start_time: 7,
                is_zombie: false,
            },
            ProcessObservation::Present {
                start_time: 7,
                is_zombie: true,
            },
        ] {
            assert_eq!(
                persisted_process_proven_absent_from(&record, Some(observation), Some("boot")),
                shape == 0,
                "shape={shape} observation={observation:?}",
            );
        }
        assert_eq!(
            persisted_process_proven_absent_from(&record, Some(ProcessObservation::Absent), None),
            shape == 0 || shape & 1 != 0,
            "only a named absent PID or an empty tuple can discharge shape={shape}",
        );
    }
}

#[test]
fn persisted_full_identity_recycled_pid_and_old_boot_allow_recovery() {
    let mut record = record();
    record.pid = Some(42);
    record.pgid = Some(42);
    record.process_start_time = Some(7);
    record.boot_id = Some("boot".into());
    let live = ProcessObservation::Present {
        start_time: 8,
        is_zombie: false,
    };
    assert!(persisted_process_proven_absent_from(
        &record,
        Some(live),
        Some("boot")
    ));
    assert!(persisted_process_proven_absent_from(
        &record,
        Some(ProcessObservation::Unknown),
        Some("new-boot")
    ));
    assert!(!persisted_process_proven_absent_from(
        &record,
        Some(live),
        None
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn persisted_unknown_identity_blocks_each_state_without_signaling() {
    use clap::Parser;
    use std::os::unix::process::CommandExt;
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            self.0.wait().expect("reap owned test survivor");
        }
    }
    let mut child = OwnedChild(
        std::process::Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id() as i32;
    let start = read_proc_start_time(pid).expect("native start token");
    for state in [
        SharedDaemonState::Idle,
        SharedDaemonState::Starting,
        SharedDaemonState::Running,
        SharedDaemonState::Failed,
    ] {
        let root = tempfile::tempdir().unwrap();
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let cfg = Config::parse_from([
            "calm-server",
            "--data-dir",
            root.path().to_str().unwrap(),
            "--codex-bin",
            "/no-replacement-may-start",
        ]);
        let home = SharedCodexHome::new(root.path().join("codex-home"));
        home.seed().unwrap();
        let dynamic_repo: Arc<dyn Repo> = repo.clone();
        let isolation =
            crate::agent_backend_pool::identity::BackendIsolation::prepare(&cfg, &dynamic_repo)
                .await
                .unwrap();
        let daemon = SharedCodexAppServer::new_with_isolation(&cfg, repo.clone(), None, isolation);
        repo.shared_daemon_runtime_set(
            "default",
            SharedCodexDaemonUpdate {
                state,
                pid: Some(pid),
                pgid: Some(pid),
                paths_are_legacy_display: false,
                sock_path: Some(
                    root.path()
                        .join("run/codex-appserver.sock")
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                ),
                codex_home_path: None,
                process_start_time: Some(start),
                boot_id: Some(String::new()),
                started_at: Some(now_ms()),
                last_error: None,
                increment_restart_count: false,
                daemon_env_signature: None,
            },
        )
        .await
        .unwrap();
        let before = repo.shared_daemon_runtime_get("default").await.unwrap();
        let error = daemon
            .ensure_running()
            .await
            .expect_err("unknown identity must prevent replacement");
        assert!(matches!(error, CalmError::CodexAppServer(_)));
        let after = repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(after.state, SharedDaemonState::Failed);
        assert_eq!(after.pid, before.pid);
        assert_eq!(after.pgid, before.pgid);
        assert_eq!(after.process_start_time, before.process_start_time);
        assert_eq!(after.boot_id, before.boot_id);
        assert_eq!(after.sock_path, before.sock_path);
        assert!(
            after
                .last_error
                .as_deref()
                .unwrap()
                .starts_with(UNRECONCILED_PREFIX)
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "unknown identity granted signal authority"
        );
    }
}

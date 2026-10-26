use super::*;

fn non_utf8_cfg(root: &tempfile::TempDir) -> Config {
    use std::os::unix::ffi::OsStringExt;
    let mut cfg = cfg(root);
    let data = root
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'd', 0xff]));
    std::fs::create_dir(&data).unwrap();
    // The historical launcher formats --listen with display(). Give that
    // spelling an alias so these tests isolate persisted-path recovery.
    std::fs::create_dir(data.join("run")).unwrap();
    let listener_alias = std::path::PathBuf::from(data.to_string_lossy().as_ref());
    std::fs::create_dir(&listener_alias).unwrap();
    std::os::unix::fs::symlink(data.join("run"), listener_alias.join("run")).unwrap();
    cfg.data_dir = Some(data);
    cfg
}

#[tokio::test]
async fn agent_backend_non_utf8_runtime_crash_heals() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let cfg = non_utf8_cfg(&root);
    let daemon = isolated_server(&cfg, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let pid = daemon.status_snapshot().runtime.unwrap().pid;
    let _cleanup = DaemonGroup(pid);
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status = daemon.status_snapshot();
            if status.state == SharedDaemonState::Running && status.runtime.unwrap().pid != pid {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("non-UTF-8 runtime must heal after crash");
    let _replacement_cleanup = DaemonGroup(daemon.status_snapshot().runtime.unwrap().pid);
    assert!(daemon.readiness_receiver().borrow().running);
}

#[tokio::test]
async fn agent_backend_non_utf8_runtime_reconnects() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = isolated_server(&non_utf8_cfg(&root), repo).await;
    daemon.start_or_takeover().await.unwrap();
    let pid = daemon.status_snapshot().runtime.unwrap().pid;
    let _cleanup = DaemonGroup(pid);
    std::fs::write(
        daemon.sock_path().with_extension("disconnect-once"),
        "model/list",
    )
    .unwrap();
    assert!(
        daemon
            .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    daemon
        .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(daemon.status_snapshot().runtime.unwrap().pid, pid);
}

#[tokio::test]
async fn agent_backend_non_utf8_runtime_restart_takes_over() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let cfg = non_utf8_cfg(&root);
    let first = isolated_server(&cfg, repo.clone()).await;
    first.start_or_takeover().await.unwrap();
    let pid = first.status_snapshot().runtime.unwrap().pid;
    let _cleanup = DaemonGroup(pid);
    drop(first);
    let record = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(!record.paths_are_legacy_display);
    assert_eq!(
        record.sock_path.as_deref(),
        Some(
            cfg.data_dir_resolved()
                .join("run/codex-appserver.sock")
                .as_os_str()
                .as_bytes()
        )
    );
    assert_eq!(
        record.codex_home_path.as_deref(),
        Some(
            cfg.data_dir_resolved()
                .join("codex-home")
                .as_os_str()
                .as_bytes()
        )
    );
    let next = isolated_server(&cfg, repo).await;
    next.start_or_takeover().await.unwrap();
    assert_eq!(next.status_snapshot().runtime.unwrap().pid, pid);
}

#[tokio::test]
async fn agent_backend_non_utf8_runtime_upgrades_lossy_legacy_row() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let cfg = non_utf8_cfg(&root);
    let legacy = SharedCodexAppServer::new(
        &cfg,
        Arc::new(calm_server::shared_codex_home::SharedCodexHome::new(
            cfg.data_dir_resolved().join("codex-home"),
        )),
        repo.clone(),
    );
    legacy.start_or_takeover().await.unwrap();
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    let pid = before.pid.unwrap();
    let _cleanup = DaemonGroup(pid);
    drop(legacy);
    let upgraded = migrate_legacy_runtime(root.path(), &before).await;
    let migrated = upgraded.shared_daemon_runtime_get("default").await.unwrap();
    assert!(migrated.paths_are_legacy_display);
    assert_eq!(
        migrated.codex_home_path.as_deref(),
        Some(
            cfg.data_dir_resolved()
                .join("codex-home")
                .display()
                .to_string()
                .as_bytes()
        )
    );
    let next = isolated_server(&cfg, upgraded.clone()).await;
    next.start_or_takeover().await.unwrap();
    assert_eq!(next.status_snapshot().runtime.unwrap().pid, pid);
    let after = upgraded.shared_daemon_runtime_get("default").await.unwrap();
    assert!(after.paths_are_legacy_display);
    assert_eq!(after.codex_home_path, migrated.codex_home_path);
    assert_eq!(after.sock_path, migrated.sock_path);
}

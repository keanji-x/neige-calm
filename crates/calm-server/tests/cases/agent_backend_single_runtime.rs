use super::*;
use calm_server::agent_backend_pool::identity::BackendIsolation;

fn single_cfg(root: &tempfile::TempDir, explicit: bool) -> Config {
    let mut config = multi_backend_cfg(root);
    if explicit {
        let path = config.agent_backend_config.as_ref().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        value["backends"].as_array_mut().unwrap().truncate(1);
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    } else {
        config.agent_backend_config = None;
    }
    config
}

async fn wait_absent(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while read_proc_start_time(pid).is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn agent_backend_single_live_nonlegacy_shared_db_prepare_verify_reconnect_heal() {
    for explicit in [false, true] {
        let old = tempfile::tempdir().unwrap();
        let current = tempfile::tempdir().unwrap();
        let repo: Arc<dyn Repo> = repo().await;
        let config = single_cfg(&current, explicit);
        // A row created after preparation must not add an isolation refusal.
        let prepared = BackendIsolation::prepare(&config, &repo).await.unwrap();
        let first = isolated_server(&single_cfg(&old, explicit), repo.clone()).await;
        first.start_or_takeover().await.unwrap();
        let before = repo.shared_daemon_runtime_get("default").await.unwrap();
        assert!(!before.paths_are_legacy_display);
        let pid = before.pid.unwrap();
        let _cleanup = DaemonGroup(pid);
        let socket = first.sock_path().to_owned();
        drop(first);
        prepared.verify(repo.as_ref()).await.unwrap();
        let next = isolated_server(&config, repo.clone()).await;
        next.start_or_takeover().await.unwrap();
        assert_eq!(next.status_snapshot().runtime.unwrap().pid, pid);
        let after = repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(after.sock_path, before.sock_path);
        assert_eq!(after.codex_home_path, before.codex_home_path);
        assert_eq!(after.process_start_time, before.process_start_time);
        assert_eq!(after.boot_id, before.boot_id);
        assert_eq!(after.daemon_env_signature, before.daemon_env_signature);
        assert!(!after.paths_are_legacy_display);
        std::fs::write(socket.with_extension("disconnect-once"), "model/list").unwrap();
        assert!(
            next.model_list(tokio::time::Instant::now() + Duration::from_secs(2))
                .await
                .is_err()
        );
        // Takeover probes the recorded socket, but reconnect uses the current
        // configuration, as on main. That socket has no listener yet.
        assert!(!next.sock_path().exists());
        assert!(matches!(
            next.model_list(tokio::time::Instant::now() + Duration::from_secs(2))
                .await,
            Err(calm_server::error::CalmError::CodexAppServer(_))
        ));
        assert_eq!(next.status_snapshot().runtime.unwrap().pid, pid);
        assert!(!next.sock_path().with_extension("methods").exists());
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(runtime) = next.status_snapshot().runtime
                    && runtime.pid != pid
                    && next.status_snapshot().state == SharedDaemonState::Running
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("adopted daemon must heal using the current configuration");
        let _replacement = DaemonGroup(next.status_snapshot().runtime.unwrap().pid);
    }
}

#[tokio::test]
async fn agent_backend_single_dead_nonlegacy_dangling_historical_home_start_and_heal() {
    for explicit in [false, true] {
        let old = tempfile::tempdir().unwrap();
        let current = tempfile::tempdir().unwrap();
        let repo = repo().await;
        let first = isolated_server(&single_cfg(&old, explicit), repo.clone()).await;
        first.start_or_takeover().await.unwrap();
        let pid = first.status_snapshot().runtime.unwrap().pid;
        let _cleanup = DaemonGroup(pid);
        drop(first);
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        wait_absent(pid).await;
        let dangling = old.path().join("historical-home");
        std::os::unix::fs::symlink(old.path().join("missing"), &dangling).unwrap();
        sqlx::query(
            "UPDATE codex_backend_daemons SET codex_home_path=? WHERE backend_id='default'",
        )
        .bind(dangling.as_os_str().as_bytes())
        .execute(repo.pool())
        .await
        .unwrap();
        let next = isolated_server(&single_cfg(&current, explicit), repo.clone()).await;
        next.start_or_takeover().await.unwrap();
        let replacement = next.status_snapshot().runtime.unwrap().pid;
        let _replacement = DaemonGroup(replacement);
        // Hold the real transition lock so the watcher sees the historical row
        // on its first recovery attempt, rather than racing a test-only hook.
        let serial = next.lock_transition_serial_for_test().await;
        sqlx::query(
            "UPDATE codex_backend_daemons SET codex_home_path=? WHERE backend_id='default'",
        )
        .bind(dangling.as_os_str().as_bytes())
        .execute(repo.pool())
        .await
        .unwrap();
        unsafe {
            libc::kill(replacement, libc::SIGKILL);
        }
        wait_absent(replacement).await;
        drop(serial);
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(runtime) = next.status_snapshot().runtime
                    && runtime.pid != replacement
                    && next.status_snapshot().state == SharedDaemonState::Running
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dead row's dangling historical home must not block healing");
        let _healed = DaemonGroup(next.status_snapshot().runtime.unwrap().pid);
    }
}

#[tokio::test]
async fn agent_backend_multi_nonlegacy_different_directory_and_dangling_history_refused() {
    let old = tempfile::tempdir().unwrap();
    let current = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let cfg = multi_backend_cfg(&current);
    let prepared = BackendIsolation::prepare(&cfg, &(repo.clone() as Arc<dyn Repo>))
        .await
        .unwrap();
    let first = isolated_server(&single_cfg(&old, false), repo.clone()).await;
    first.start_or_takeover().await.unwrap();
    let pid = first.status_snapshot().runtime.unwrap().pid;
    let _cleanup = DaemonGroup(pid);
    drop(first);
    let next = SharedCodexAppServer::new_with_isolation(&cfg, repo.clone(), None, prepared.clone());
    assert!(next.start_or_takeover().await.is_err());
    assert!(next.ensure_running().await.is_err());
    assert!(prepared.verify(repo.as_ref()).await.is_err());
    assert!(
        BackendIsolation::prepare(&cfg, &(repo.clone() as Arc<dyn Repo>))
            .await
            .is_err()
    );
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    wait_absent(pid).await;
    let dangling = old.path().join("historical-home");
    std::os::unix::fs::symlink(old.path().join("missing"), &dangling).unwrap();
    sqlx::query("UPDATE codex_backend_daemons SET codex_home_path=? WHERE backend_id='default'")
        .bind(dangling.as_os_str().as_bytes())
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(prepared.verify(repo.as_ref()).await.is_err());
    assert!(
        BackendIsolation::prepare(&cfg, &(repo.clone() as Arc<dyn Repo>))
            .await
            .is_err()
    );
    assert!(next.start_or_takeover().await.is_err());
    assert!(next.ensure_running().await.is_err());
}

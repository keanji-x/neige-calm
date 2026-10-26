use super::*;

async fn legacy_scope_upgrade(
    alias: bool,
    exited: bool,
    different_home: bool,
    configured: bool,
    invalid_socket: Option<&str>,
) {
    let root = tempfile::tempdir().unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let mut cfg = cfg(&root);
    if alias {
        let link = canonical.join("data-alias");
        std::os::unix::fs::symlink(&canonical, &link).unwrap();
        cfg.data_dir = Some(link);
    } else {
        let mut relative = std::path::PathBuf::new();
        for _ in std::env::current_dir().unwrap().components().skip(1) {
            relative.push("..");
        }
        relative.push(canonical.strip_prefix("/").unwrap());
        cfg.data_dir = Some(relative);
    }
    let repo = repo().await;
    let legacy = SharedCodexAppServer::new(
        &cfg,
        Arc::new(calm_server::shared_codex_home::SharedCodexHome::new(
            cfg.data_dir_resolved().join("codex-home"),
        )),
        repo.clone(),
    );
    legacy.start_or_takeover().await.unwrap();
    let mut record = repo.shared_daemon_runtime_get("default").await.unwrap();
    let pid = record.pid.unwrap();
    let _cleanup = DaemonGroup(pid);
    drop(legacy);
    if exited {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while read_proc_start_time(pid).is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    if let Some(socket) = invalid_socket {
        record.sock_path = Some(canonical.join(socket).as_os_str().as_bytes().to_vec());
    }
    let upgraded = migrate_legacy_runtime(root.path(), &record).await;
    let home = canonical.join(if different_home {
        "other-home"
    } else {
        "codex-home"
    });
    // Upgrade with a new spelling of the same deployment directory.
    cfg.data_dir = Some(canonical.clone());
    let profiles = root.path().join("backends.json");
    let mut backends = vec![
        json!({"provider":"codex","id":"default","weight":1,"accepts_new_sessions":true,
        "credentials":{"kind":"codex_home","path":home}}),
    ];
    if invalid_socket.is_some() {
        backends.push(
            json!({"provider":"codex","id":"other","weight":1,"accepts_new_sessions":true,
            "credentials":{"kind":"codex_home","path":canonical.join("other-home")}}),
        );
    }
    std::fs::write(&profiles, serde_json::to_vec(&json!({
        "cooldown_after_concurrency_limit_secs":60,"cooldown_default_secs":300,"cooldown_max_secs":3600,
        "backends":backends
    })).unwrap()).unwrap();
    if configured {
        cfg.agent_backend_config = Some(profiles);
    }
    let before = upgraded.shared_daemon_runtime_get("default").await.unwrap();
    let seen = connection_methods_seen(&cfg.data_dir_resolved().join("run/codex-appserver.sock"));
    let prepared =
        calm_server::agent_backend_pool::identity::BackendIsolation::prepare(&cfg, &upgraded).await;
    if invalid_socket.is_some_and(|socket| socket != "run/codex-appserver.sock") {
        let error = prepared
            .err()
            .expect("prepare must refuse legacy scope mismatch");
        if let Some(socket) = invalid_socket {
            use calm_server::agent_backend_pool::identity::RuntimePathError;
            if socket.contains('\u{fffd}') {
                assert!(
                    matches!(error.downcast_ref(), Some(RuntimePathError::LossyLegacy)),
                    "{error:#}"
                );
            } else {
                assert!(
                    matches!(error.downcast_ref(), Some(RuntimePathError::Mismatch)),
                    "{error:#}"
                );
            }
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        assert_eq!(
            connection_methods_seen(&cfg.data_dir_resolved().join("run/codex-appserver.sock")),
            seen
        );
        let after = upgraded.shared_daemon_runtime_get("default").await.unwrap();
        macro_rules! unchanged {
            ($($field:ident),*) => { $(assert_eq!(after.$field, before.$field);)* };
        }
        unchanged!(
            state,
            pid,
            pgid,
            paths_are_legacy_display,
            sock_path,
            codex_home_path,
            process_start_time,
            boot_id,
            started_at,
            updated_at,
            restart_count,
            last_error,
            daemon_env_signature
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_backend_registry")
            .fetch_one(upgraded.sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
        if different_home {
            assert!(!home.exists());
        }
        return;
    }
    let next = SharedCodexAppServer::new_with_isolation(&cfg, upgraded, None, prepared.unwrap());
    next.start_or_takeover().await.unwrap();
    let running = next.status_snapshot().runtime.unwrap();
    let _replacement_cleanup = DaemonGroup(running.pid);
    assert_eq!(running.pid == pid, !exited);
    assert_eq!(next.status_snapshot().state, SharedDaemonState::Running);
    assert_eq!(
        next.status_snapshot().codex_home,
        home.display().to_string()
    );
}

#[tokio::test]
async fn agent_backend_legacy_relative_to_absolute_takes_over() {
    legacy_scope_upgrade(false, false, false, true, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_relative_to_absolute_recovers_exited() {
    legacy_scope_upgrade(false, true, false, true, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_alias_to_canonical_takes_over() {
    legacy_scope_upgrade(true, false, false, true, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_alias_to_canonical_recovers_exited() {
    legacy_scope_upgrade(true, true, false, true, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_single_other_home_takes_over() {
    legacy_scope_upgrade(false, false, true, true, None).await;
}

#[tokio::test]
async fn agent_backend_legacy_unconfigured_relative_to_absolute_takes_over() {
    legacy_scope_upgrade(false, false, false, false, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_unconfigured_relative_to_absolute_recovers_exited() {
    legacy_scope_upgrade(false, true, false, false, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_unconfigured_alias_to_canonical_takes_over() {
    legacy_scope_upgrade(true, false, false, false, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_unconfigured_alias_to_canonical_recovers_exited() {
    legacy_scope_upgrade(true, true, false, false, None).await;
}
#[tokio::test]
async fn agent_backend_legacy_multi_other_socket_refuses_without_effects() {
    legacy_scope_upgrade(
        false,
        false,
        false,
        true,
        Some("other/codex-appserver.sock"),
    )
    .await;
}
#[tokio::test]
async fn agent_backend_legacy_multi_lossy_socket_refuses_without_effects() {
    legacy_scope_upgrade(false, false, false, true, Some("run/\u{fffd}.sock")).await;
}

async fn legacy_other_directory_adoption(current_listener: bool) {
    let old = tempfile::tempdir().unwrap();
    let current = tempfile::tempdir().unwrap();
    let old_socket = old.path().join("run/codex-appserver.sock");
    let current_socket = current.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(old_socket.parent().unwrap()).unwrap();
    std::fs::create_dir_all(current_socket.parent().unwrap()).unwrap();
    let old_marker = old.path().join("term");
    let (mut child, pid, start) = spawn_unbound_child_with_term_marker(
        &old_socket,
        0,
        &old_marker,
        &old.path().join("ready"),
    )
    .await;
    wait_for_start_time_and_socket(pid, &old_socket).await;
    let original = repo().await;
    persist_running_daemon(&original, &old, pid, pid, &old_socket, start).await;
    let before = original.shared_daemon_runtime_get("default").await.unwrap();
    let upgraded = migrate_legacy_runtime(old.path(), &before).await;
    let other_marker = current.path().join("term");
    let other = if current_listener {
        let (child, pid, _) = spawn_unbound_child_with_term_marker(
            &current_socket,
            0,
            &other_marker,
            &current.path().join("ready"),
        )
        .await;
        wait_for_start_time_and_socket(pid, &current_socket).await;
        Some((child, pid))
    } else {
        None
    };
    let next = isolated_server(&cfg(&current), upgraded.clone()).await;
    next.start_or_takeover().await.unwrap();
    let _replacement_cleanup = DaemonGroup(next.status_snapshot().runtime.unwrap().pid);
    assert_eq!(next.status_snapshot().runtime.unwrap().pid, pid);
    assert!(!old_marker.exists());
    assert!(!other_marker.exists());
    assert_eq!(connection_methods_seen(&old_socket), vec!["initialize"]);
    assert!(!current_socket.with_extension("methods").exists());
    let after = upgraded.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.pgid, before.pgid);
    assert_eq!(after.process_start_time, before.process_start_time);
    assert_eq!(after.boot_id, before.boot_id);
    assert_eq!(after.sock_path, before.sock_path);
    assert_eq!(after.codex_home_path, before.codex_home_path);
    assert!(after.paths_are_legacy_display);
    // The installed connection must continue to address A, even with C listening.
    next.model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    let methods = std::fs::read_to_string(old_socket.with_extension("methods")).unwrap();
    assert!(methods.lines().any(|method| method == "model/list"));
    assert!(!current_socket.with_extension("methods").exists());
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    drop(next);
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    if let Some((mut other, other_pid)) = other {
        assert_eq!(unsafe { libc::kill(other_pid, 0) }, 0);
        other.kill().await.unwrap();
        other.wait().await.unwrap();
    }
}

#[tokio::test]
async fn agent_backend_legacy_other_directory_missing_current_socket_adopts_recorded() {
    legacy_other_directory_adoption(false).await;
}

#[tokio::test]
async fn agent_backend_legacy_other_directory_different_current_daemon_adopts_recorded() {
    legacy_other_directory_adoption(true).await;
}

#[tokio::test]
async fn agent_backend_legacy_multi_matching_lossless_socket_takes_over() {
    legacy_scope_upgrade(false, false, false, true, Some("run/codex-appserver.sock")).await;
}

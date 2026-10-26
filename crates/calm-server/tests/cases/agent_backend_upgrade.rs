use super::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;

struct DaemonGroup(i32);
impl Drop for DaemonGroup {
    fn drop(&mut self) {
        // SAFETY: this test owns the fake daemon's process group.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[tokio::test]
async fn agent_backend_upgrade_0755_takes_over_the_original_default_daemon() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let original_repo = repo().await;
    let daemon = server(&root, original_repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let record = original_repo
        .shared_daemon_runtime_get("default")
        .await
        .unwrap();
    let pid = record.pid.unwrap();
    let _cleanup = DaemonGroup(pid);
    drop(daemon);
    let upgraded = migrate_legacy_runtime(root.path(), &record).await;
    for directory in ["codex-home", "state", "logs", "logs/shared-codex-appserver"] {
        std::fs::create_dir_all(root.path().join(directory)).unwrap();
        std::fs::set_permissions(
            root.path().join(directory),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let next_boot = isolated_server(&cfg(&root), upgraded.clone()).await;
    next_boot.start_or_takeover().await.unwrap();
    assert_eq!(next_boot.status_snapshot().runtime.unwrap().pid, pid);
    let after = upgraded.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.codex_home_path, record.codex_home_path);
    assert_eq!(after.sock_path, record.sock_path);
    assert_eq!(after.boot_id, record.boot_id);
    assert_eq!(
        next_boot.status_snapshot().codex_home,
        String::from_utf8(record.codex_home_path.clone().unwrap()).unwrap()
    );
    for directory in ["codex-home", "state", "logs", "logs/shared-codex-appserver"] {
        assert_eq!(
            std::fs::metadata(root.path().join(directory))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755,
        );
    }
    assert_eq!(
        upgraded
            .shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .process_start_time,
        record.process_start_time
    );
}

async fn migrate_legacy_runtime(
    root: &Path,
    record: &calm_truth::db::SharedCodexDaemonRecord,
) -> Arc<dyn Repo> {
    use sqlx::migrate::Migrator;
    use std::borrow::Cow;
    let pid = record.pid.unwrap();
    let url = format!("sqlite://{}?mode=rwc", root.join("old.db").display());
    let old = Migrator {
        migrations: Cow::Owned(
            calm_truth::MIGRATOR
                .iter()
                .filter(|m| m.version < 198)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    old.run(&pool).await.unwrap();
    sqlx::query("UPDATE shared_codex_daemon SET state='running', pid=?, pgid=?, sock_path=?, codex_home_path=?, process_start_time=?, boot_id=?, started_at=?, daemon_env_signature=?")
        .bind(pid).bind(record.pgid).bind(record.sock_path.as_ref().map(|v| std::ffi::OsStr::from_bytes(v).to_string_lossy().into_owned()))
        .bind(record.codex_home_path.as_ref().map(|v| std::ffi::OsStr::from_bytes(v).to_string_lossy().into_owned()))
        .bind(record.process_start_time.map(|v| i64::try_from(v).unwrap())).bind(&record.boot_id)
        .bind(record.started_at).bind(&record.daemon_env_signature).execute(&pool).await.unwrap();
    pool.close().await;
    Arc::new(SqlxRepo::open(&url).await.unwrap())
}

async fn isolated_server(cfg: &Config, repo: Arc<dyn Repo>) -> Arc<SharedCodexAppServer> {
    let isolation =
        calm_server::agent_backend_pool::identity::BackendIsolation::prepare(cfg, &repo)
            .await
            .unwrap();
    SharedCodexAppServer::new_with_isolation(cfg, repo, None, isolation)
}

async fn legacy_paths_survive(cfg: Config) {
    let repo = repo().await;
    let home = cfg.data_dir_resolved().join("codex-home");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let legacy = SharedCodexAppServer::new(
        &cfg,
        Arc::new(calm_server::shared_codex_home::SharedCodexHome::new(
            home.clone(),
        )),
        repo.clone(),
    );
    legacy.start_or_takeover().await.unwrap();
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    let _cleanup = DaemonGroup(before.pid.unwrap());
    legacy.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "error".into(),
        params: json!({"error":{"message":"Sign in again", "codexErrorInfo":"unauthorized"}}),
    });
    let notice = legacy.authentication_notice().unwrap();
    let hold = legacy.authentication_hold();
    assert!(hold.is_some());
    drop(legacy);
    let upgraded = migrate_legacy_runtime(&cfg.data_dir_resolved(), &before).await;
    let next = isolated_server(&cfg, upgraded.clone()).await;
    let expected = home.to_str().unwrap();
    assert_eq!(
        next.authentication_notice().unwrap().revision,
        notice.revision
    );
    assert_eq!(next.authentication_notice().unwrap().kind, notice.kind);
    assert_eq!(next.authentication_hold(), hold);
    assert_eq!(
        next.spawn_env_for_test().await.unwrap()["CODEX_HOME"].as_deref(),
        Some(expected)
    );
    next.start_or_takeover().await.unwrap();
    assert_eq!(next.status_snapshot().state, SharedDaemonState::Running);
    assert_eq!(next.status_snapshot().codex_home, expected);
    let after = upgraded.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.codex_home_path, before.codex_home_path);
    assert_eq!(after.sock_path, before.sock_path);
    assert_eq!(after.pid, before.pid);
    if std::fs::symlink_metadata(&home)
        .unwrap()
        .file_type()
        .is_symlink()
    {
        let replacement = cfg.data_dir_resolved().join("replacement-home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&replacement)
            .unwrap();
        std::fs::remove_file(&home).unwrap();
        std::os::unix::fs::symlink(&replacement, &home).unwrap();
        next.start_or_takeover().await.unwrap();
        assert_eq!(
            next.status_snapshot().runtime.unwrap().pid,
            before.pid.unwrap()
        );
        next.mark_needs_respawn();
        next.ensure_respawn_for_current_settings().await.unwrap();
        let replacement_pid = next.status_snapshot().runtime.unwrap().pid;
        let _replacement = DaemonGroup(replacement_pid);
        assert_ne!(replacement_pid, before.pid.unwrap());
        assert_eq!(next.status_snapshot().state, SharedDaemonState::Running);
        let respawned = upgraded.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(
            respawned.codex_home_path,
            Some(home.as_os_str().as_bytes().to_vec())
        );
    }
}

#[tokio::test]
async fn agent_backend_upgrade_preserves_relative_default_paths() {
    let root = tempfile::Builder::new()
        .prefix("backend-")
        .tempdir_in(".")
        .unwrap();
    let mut cfg = cfg(&root);
    cfg.data_dir = Some(
        root.path()
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_owned(),
    );
    legacy_paths_survive(cfg).await;
}

#[tokio::test]
async fn agent_backend_upgrade_preserves_symlink_default_paths() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let mut cfg = cfg(&root);
    cfg.data_dir = Some(alias);
    legacy_paths_survive(cfg).await;
}

#[tokio::test]
async fn agent_backend_upgrade_preserves_symlinked_legacy_home_and_runtime_directories() {
    let root = tempfile::tempdir().unwrap();
    // Each legacy directory itself is an alias, not merely its data-dir parent.
    for relative in ["codex-home", "run", "state", "logs/shared-codex-appserver"] {
        let target = root.path().join("private").join(relative);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target)
            .unwrap();
        let alias = root.path().join(relative);
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, alias).unwrap();
    }
    legacy_paths_survive(cfg(&root)).await;
}

fn multi_backend_cfg(root: &tempfile::TempDir) -> Config {
    let mut cfg = cfg(root);
    let profiles = root.path().join("backends.json");
    std::fs::write(&profiles, serde_json::to_vec(&json!({
        "cooldown_after_concurrency_limit_secs":60,"cooldown_default_secs":300,"cooldown_max_secs":3600,
        "backends": (["default", "other"].map(|id| json!({
            "provider":"codex", "id":id, "weight":1, "accepts_new_sessions":true,
            "credentials":{"kind":"codex_home", "path":root.path().join(if id == "default" { "codex-home" } else { id })}
        })))
    })).unwrap()).unwrap();
    cfg.agent_backend_config = Some(profiles);
    cfg
}

async fn crash_refusal_heals(adopted: bool) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let cfg = multi_backend_cfg(&root);
    let first = isolated_server(&cfg, repo.clone()).await;
    first.start_or_takeover().await.unwrap();
    let pid = first.status_snapshot().runtime.unwrap().pid;
    let _cleanup = DaemonGroup(pid);
    let mut survivor = Command::new("sleep")
        .arg("120")
        .process_group(pid)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let daemon = if adopted {
        drop(first);
        let next = isolated_server(&cfg, repo.clone()).await;
        next.start_or_takeover().await.unwrap();
        assert!(next.taken_over_pid_watcher_active_for_test().await);
        next
    } else {
        first
    };
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    let home = root.path().join("codex-home");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon.status_snapshot().state != SharedDaemonState::Failed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dead daemon must leave Running after isolation refusal");
    assert!(!daemon.readiness_receiver().borrow().running);
    assert!(daemon.ensure_running().await.is_err());
    let refused = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(refused.pid, before.pid);
    assert_eq!(refused.process_start_time, before.process_start_time);
    assert_eq!(refused.updated_at, before.updated_at);
    assert_eq!(refused.state, before.state);
    assert_eq!(daemon.status_snapshot().restart_count, 0);
    assert!(
        survivor.try_wait().unwrap().is_none(),
        "refusal must not reap the recorded process group"
    );
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status = daemon.status_snapshot();
            if status.state == SharedDaemonState::Running {
                assert_ne!(status.runtime.unwrap().pid, pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("armed heal must recover after repair without manual restart");
    assert!(daemon.readiness_receiver().borrow().running);
    let new_pid = daemon.status_snapshot().runtime.unwrap().pid;
    unsafe {
        libc::kill(-new_pid, libc::SIGKILL);
    }
}

#[tokio::test]
async fn agent_backend_spawned_crash_refusal_repair_heals() {
    crash_refusal_heals(false).await;
}

#[tokio::test]
async fn agent_backend_adopted_crash_refusal_repair_heals() {
    crash_refusal_heals(true).await;
}

#[tokio::test]
async fn agent_backend_missing_relative_default_logs_resolve_from_cwd() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = tempfile::Builder::new()
        .prefix("backend-logs-")
        .tempdir_in(".")
        .unwrap();
    let mut cfg = cfg(&root);
    let home_root = root.path().canonicalize().unwrap();
    let profiles = root.path().join("backends.json");
    std::fs::write(&profiles, serde_json::to_vec(&json!({
        "cooldown_after_concurrency_limit_secs":60,"cooldown_default_secs":300,"cooldown_max_secs":3600,
        "backends": (["default", "other"].map(|id| json!({
            "provider":"codex", "id":id, "weight":1, "accepts_new_sessions":true,
            "credentials":{"kind":"codex_home", "path":home_root.join(if id == "default" { "codex-home" } else { id })}
        })))
    })).unwrap()).unwrap();
    cfg.agent_backend_config = Some(profiles);
    // No relative component exists yet: the longest existing ancestor is cwd.
    let missing = tempfile::Builder::new()
        .prefix("agent-backend-logs-")
        .tempdir_in(".")
        .unwrap();
    cfg.shared_codex_appserver_log_dir =
        Some(std::path::PathBuf::from(missing.path().file_name().unwrap()).join("not-yet/created"));
    // Keep the TempDir guard to clean up the runtime directory prepare creates.
    std::fs::remove_dir(missing.path()).unwrap();
    let logs = cfg.shared_codex_appserver_log_dir_resolved();
    assert!(!logs.exists());
    let repo: Arc<dyn Repo> = repo().await;
    calm_server::agent_backend_pool::identity::BackendIsolation::prepare(&cfg, &repo)
        .await
        .unwrap();
    for parent in [&logs, logs.parent().unwrap()] {
        let metadata = std::fs::metadata(parent).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }
    cfg.shared_codex_appserver_log_dir = Some(
        std::path::PathBuf::from(root.path().file_name().unwrap())
            .join("logs/shared-codex-appserver-other"),
    );
    assert!(
        calm_server::agent_backend_pool::identity::BackendIsolation::prepare(&cfg, &repo)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn agent_backend_data_alias_retarget_refuses_verify_start_replace_and_heal() {
    use calm_server::agent_backend_pool::identity::BackendIsolation;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let alias = root.path().join("data-alias");
    std::os::unix::fs::symlink(&data, &alias).unwrap();
    let home = root.path().join("default-home");
    let other = root.path().join("other-home");
    for path in [&home, &other] {
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    let mut cfg = cfg(&root);
    cfg.data_dir = Some(alias.clone());
    cfg.shared_codex_appserver_log_dir = Some(root.path().join("independent-logs"));
    let repo: Arc<dyn Repo> = repo().await;
    // Existing daemon record proves the independent default home on upgrade.
    let legacy = SharedCodexAppServer::new(
        &cfg,
        Arc::new(calm_server::shared_codex_home::SharedCodexHome::new(
            home.clone(),
        )),
        repo.clone(),
    );
    legacy.start_or_takeover().await.unwrap();
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    let pid = before.pid.unwrap();
    let _cleanup = DaemonGroup(pid);
    let mut survivor = Command::new("sleep")
        .arg("120")
        .process_group(pid)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let profiles = root.path().join("backends.json");
    std::fs::write(&profiles, serde_json::to_vec(&json!({
        "cooldown_after_concurrency_limit_secs":60,"cooldown_default_secs":300,"cooldown_max_secs":3600,
        "backends": ([("default", &home), ("other", &other)].map(|(id, path)| json!({
            "provider":"codex", "id":id, "weight":1, "accepts_new_sessions":true,
            "credentials":{"kind":"codex_home", "path":path}
        })))
    })).unwrap()).unwrap();
    cfg.agent_backend_config = Some(profiles);
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    let next =
        SharedCodexAppServer::new_with_isolation(&cfg, repo.clone(), None, isolation.clone());
    next.start_or_takeover().await.unwrap();
    drop(legacy);
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    assert!(isolation.verify(repo.as_ref()).await.is_err());
    let error = next.start_or_takeover().await.unwrap_err();
    assert!(error.to_string().contains("backend isolation refused"));
    next.mark_needs_respawn();
    let error = next
        .ensure_respawn_for_current_settings()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("backend isolation refused"));
    // Running is a side-effect-free no-op, as in the legacy supervisor.
    next.ensure_running().await.unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while next.status_snapshot().state != SharedDaemonState::Failed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("crash refusal must revoke Running");
    let error = next.ensure_running().await.unwrap_err();
    assert!(error.to_string().contains("backend isolation refused"));
    assert!(
        survivor.try_wait().unwrap().is_none(),
        "must not reap old group"
    );
    let after = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after.state, before.state);
    assert!(!other.join("run").exists());
    assert!(!other.join("state").exists());
    assert!(!other.join("config.toml").exists());
}

fn connection_methods_seen(socket: &Path) -> Vec<String> {
    std::fs::read_to_string(socket.with_extension("methods"))
        .unwrap()
        .lines()
        .filter(|method| matches!(*method, "initialize" | "thread/resume"))
        .map(str::to_owned)
        .collect()
}

async fn takeover_socket_refusal(untrusted_ancestor: bool) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let cfg = multi_backend_cfg(&root);
    let first = isolated_server(&cfg, repo.clone()).await;
    first.start_or_takeover().await.unwrap();
    let pid = first.status_snapshot().runtime.unwrap().pid;
    let _cleanup = DaemonGroup(pid);
    let mut survivor = Command::new("sleep")
        .arg("120")
        .process_group(pid)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Prepare before changing the persisted evidence: the process-entry check
    // must re-read it, rather than trust startup preparation.
    let next = isolated_server(&cfg, repo.clone()).await;
    let old = root.path().join("old-run");
    std::fs::create_dir(&old).unwrap();
    let socket = old.join("old.sock");
    let original = first.sock_path();
    std::os::unix::fs::symlink(original, &socket).unwrap();
    if untrusted_ancestor {
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o777)).unwrap();
    } else {
        // The recorded live process has a different socket. Without comparison
        // the handshake fails and takeover reaps that live group.
        std::fs::remove_file(&socket).unwrap();
        let _listener = tokio::net::UnixListener::bind(&socket).unwrap();
        drop(_listener);
    }
    sqlx::query("UPDATE codex_backend_daemons SET sock_path=? WHERE backend_id='default'")
        .bind(socket.as_os_str().as_bytes())
        .execute(repo.pool())
        .await
        .unwrap();
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    let seen = connection_methods_seen(original);
    assert!(next.start_or_takeover().await.is_err());
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "must refuse before reap");
    assert_eq!(
        connection_methods_seen(original),
        seen,
        "must refuse before initialize"
    );
    assert!(
        survivor.try_wait().unwrap().is_none(),
        "must refuse before group reap"
    );
    assert!(!next.readiness_receiver().borrow().running);
    assert_eq!(
        repo.shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .updated_at,
        before.updated_at
    );
}

#[tokio::test]
async fn agent_backend_takeover_rejects_different_persisted_socket_before_connect_or_reap() {
    takeover_socket_refusal(false).await;
}

#[tokio::test]
async fn agent_backend_takeover_rejects_untrusted_old_socket_ancestor_before_connect_or_reap() {
    takeover_socket_refusal(true).await;
}

async fn reconnect_path_refusal(socket_alias: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = isolated_server(&multi_backend_cfg(&root), repo).await;
    daemon.start_or_takeover().await.unwrap();
    let pid = daemon.status_snapshot().runtime.unwrap().pid;
    let _cleanup = DaemonGroup(pid);
    let socket = daemon.sock_path().to_owned();
    std::fs::write(socket.with_extension("disconnect-once"), "model/list").unwrap();
    assert!(
        daemon
            .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    let before = connection_methods_seen(&socket);
    let mut readiness = daemon.readiness_receiver();
    readiness.borrow_and_update();
    // Keep the daemon and its listener alive at another spelling; an unchecked
    // reconnect can still initialize and publish a fresh connection generation.
    if socket_alias {
        let moved = socket.with_extension("moved");
        std::fs::rename(&socket, &moved).unwrap();
        std::os::unix::fs::symlink(moved, &socket).unwrap();
    } else {
        let run = socket.parent().unwrap();
        let moved = root.path().join("moved-run");
        std::fs::rename(run, &moved).unwrap();
        std::os::unix::fs::symlink(moved, run).unwrap();
    }
    assert!(
        daemon
            .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    assert_eq!(
        connection_methods_seen(&socket),
        before,
        "no initialize or resume on refusal"
    );
    assert!(
        !readiness.has_changed().unwrap(),
        "no readiness publication on refusal"
    );
}

#[tokio::test]
async fn agent_backend_reconnect_rejects_run_retarget_before_initialize_resume_or_readiness() {
    reconnect_path_refusal(false).await;
}

#[tokio::test]
async fn agent_backend_reconnect_rejects_socket_retarget_before_initialize_resume_or_readiness() {
    reconnect_path_refusal(true).await;
}

#[tokio::test]
async fn agent_backend_stale_socket_recovers_after_boot_or_start_time_expiry() {
    for old_boot in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let repo = repo().await;
        // Use a LIVE unrelated pid, so only boot/start identity proves absence.
        let pid = i32::try_from(std::process::id()).unwrap();
        repo.shared_daemon_runtime_set(
            "default",
            SharedCodexDaemonUpdate {
                state: SharedDaemonState::Running,
                pid: Some(pid),
                pgid: Some(pid),
                paths_are_legacy_display: false,
                sock_path: Some(root.path().join("old.sock").as_os_str().as_bytes().to_vec()),
                codex_home_path: Some(
                    root.path()
                        .join("codex-home")
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                ),
                process_start_time: Some(read_proc_start_time(pid).unwrap() + u64::from(!old_boot)),
                boot_id: Some(if old_boot {
                    "previous-boot".into()
                } else {
                    read_boot_id().unwrap()
                }),
                started_at: Some(1),
                last_error: None,
                increment_restart_count: false,
                daemon_env_signature: None,
            },
        )
        .await
        .unwrap();
        let daemon = isolated_server(&cfg(&root), repo.clone()).await;
        daemon.start_or_takeover().await.unwrap();
        let running = daemon.status_snapshot().runtime.unwrap();
        let _cleanup = DaemonGroup(running.pid);
        assert_ne!(running.pid, pid);
        assert_eq!(daemon.status_snapshot().state, SharedDaemonState::Running);
        assert_eq!(
            repo.shared_daemon_runtime_get("default")
                .await
                .unwrap()
                .sock_path,
            Some(daemon.sock_path().as_os_str().as_bytes().to_vec())
        );
    }
}

#[tokio::test]
async fn agent_backend_single_reconnect_does_not_need_database_writer() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = isolated_server(&cfg(&root), repo.clone()).await;
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
    let writer = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        daemon.model_list(tokio::time::Instant::now() + Duration::from_secs(2)),
    )
    .await
    .expect("reconnect must not wait for the database writer")
    .unwrap();
    assert_eq!(daemon.status_snapshot().runtime.unwrap().pid, pid);
    writer.rollback().await.unwrap();
}

#[tokio::test]
async fn agent_backend_running_ensure_running_has_no_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = isolated_server(&cfg(&root), repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    let _cleanup = DaemonGroup(before.pid.unwrap());
    let mut readiness = daemon.readiness_receiver();
    readiness.borrow_and_update();
    let methods = connection_methods_seen(daemon.sock_path());
    // A Running no-op must not even wait for the transition serial.
    let serial = daemon.lock_transition_serial_for_test().await;
    let writer = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), daemon.ensure_running())
        .await
        .expect("Running must return without transition or registry work")
        .unwrap();
    writer.rollback().await.unwrap();
    drop(serial);
    assert!(!readiness.has_changed().unwrap());
    assert_eq!(connection_methods_seen(daemon.sock_path()), methods);
    let after = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after.restart_count, before.restart_count);
}

#[path = "agent_backend_non_utf8.rs"]
mod non_utf8;

#[path = "agent_backend_legacy_scope.rs"]
mod legacy_scope;

#[path = "agent_backend_single_runtime.rs"]
mod single_runtime;

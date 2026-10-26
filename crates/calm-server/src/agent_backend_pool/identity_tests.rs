use super::identity::{BackendIsolation, check_environment, default_id};
use super::paths::BackendPaths;
use crate::config::Config;
use calm_truth::db::{Repo, sqlite::SqlxRepo};
use clap::Parser;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::Arc;

fn config(root: &std::path::Path, homes: &[(&str, std::path::PathBuf)]) -> Config {
    let path = root.join("backends.json");
    std::fs::write(&path, serde_json::to_vec(&serde_json::json!({
        "cooldown_after_concurrency_limit_secs":60,"cooldown_default_secs":300,"cooldown_max_secs":3600,
        "backends":homes.iter().map(|(id,home)| serde_json::json!({"provider":"codex","id":id,"weight":1,"accepts_new_sessions":true,"credentials":{"kind":"codex_home","path":home}})).collect::<Vec<_>>()
    })).unwrap()).unwrap();
    Config::parse_from([
        "calm-server",
        "--data-dir",
        root.to_str().unwrap(),
        "--agent-backend-config",
        path.to_str().unwrap(),
    ])
}

#[test]
fn agent_backend_paths_isolate_socket_authentication_and_logs() {
    let root = std::path::Path::new("/data");
    let default = BackendPaths::new(
        root,
        &default_id(),
        &root.join("codex-home"),
        &root.join("logs/custom"),
    );
    assert_eq!(default.socket, root.join("run/codex-appserver.sock"));
    assert_eq!(
        default.authentication,
        root.join("state/codex-authentication.json")
    );
    assert_eq!(default.logs, root.join("logs/custom"));
    let a = BackendPaths::new(
        root,
        &"a".to_owned().try_into().unwrap(),
        &root.join("a"),
        &default.logs,
    );
    let b = BackendPaths::new(
        root,
        &"b".to_owned().try_into().unwrap(),
        &root.join("b"),
        &default.logs,
    );
    assert_ne!(a.socket, default.socket);
    assert_ne!(a.socket, b.socket);
    assert_ne!(a.authentication, default.authentication);
    assert_ne!(a.authentication, b.authentication);
    assert_ne!(a.logs, default.logs);
    assert_ne!(a.logs, b.logs);
}

#[test]
fn agent_backend_each_process_credential_rejected_including_empty() {
    for key in provider::codex::shared::CREDENTIAL_ENV_KEYS {
        for value in ["", "secret-test-value"] {
            let read = |candidate: &str| (candidate == *key).then(|| value.into());
            let error = check_environment(true, read).unwrap_err().to_string();
            assert!(error.contains(key));
            assert!(!error.contains("secret-test-value"));
            check_environment(false, read).unwrap();
        }
    }
    check_environment(true, |_| None).unwrap();
}

#[tokio::test]
async fn agent_backend_multi_home_0755_aliases_and_overlap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let default = root.join("codex-home");
    let second = root.join("second");
    let cfg = config(
        root,
        &[("default", default.clone()), ("second", second.clone())],
    );
    let identity = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    for home in [&default, &second] {
        assert_eq!(
            std::fs::metadata(home).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert!(!home.join("auth.json").exists());
    }
    std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(identity.verify(repo.as_ref()).await.is_err());
    assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o700)).unwrap();
    identity.verify(repo.as_ref()).await.unwrap();
    let alias = root.join("alias");
    symlink(&second, &alias).unwrap();
    for bad in [
        alias,
        second.join("../second"),
        default.join("nested"),
        root.join("logs/shared-codex-appserver/nested"),
    ] {
        let cfg = config(root, &[("default", default.clone()), ("second", bad)]);
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    }
}

#[tokio::test]
async fn agent_backend_multi_registration_rejects_removed_backend() {
    let tmp = tempfile::tempdir().unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let homes = [
        ("default", tmp.path().join("codex-home")),
        ("other", tmp.path().join("other")),
        ("third", tmp.path().join("third")),
    ];
    BackendIsolation::prepare(&config(tmp.path(), &homes), &repo)
        .await
        .unwrap();
    let error = BackendIsolation::prepare(&config(tmp.path(), &homes[..2]), &repo)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error.downcast_ref(),
        Some(calm_truth::db::sqlite::backend_registry::RegistryError::Removed(..))
    ));
    // Single-backend mode ignores even an existing multi-backend registration.
    let implicit = Config::parse_from(["calm-server", "--data-dir", tmp.path().to_str().unwrap()]);
    BackendIsolation::prepare(&implicit, &repo).await.unwrap();
}

#[tokio::test]
async fn agent_backend_takeover_revalidates_before_touching_runtime() {
    use calm_truth::db::{SharedCodexDaemonUpdate, SharedDaemonState};
    let tmp = tempfile::tempdir().unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let home = tmp.path().join("codex-home");
    let cfg = config(
        tmp.path(),
        &[
            ("default", home.clone()),
            ("other", tmp.path().join("other")),
        ],
    );
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    repo.shared_daemon_runtime_set(
        "default",
        SharedCodexDaemonUpdate {
            state: SharedDaemonState::Running,
            pid: Some(i32::try_from(std::process::id()).unwrap()),
            pgid: None,
            paths_are_legacy_display: false,
            sock_path: None,
            codex_home_path: Some(home.as_os_str().as_bytes().to_vec()),
            process_start_time: None,
            boot_id: None,
            started_at: None,
            last_error: None,
            increment_restart_count: false,
            daemon_env_signature: None,
        },
    )
    .await
    .unwrap();
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_with_isolation(
        &cfg,
        repo.clone(),
        None,
        isolation,
    );
    assert!(daemon.start_or_takeover().await.is_err());
    let after = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.state, before.state);
    assert_eq!(after.updated_at, before.updated_at);
}

#[tokio::test]
async fn agent_backend_upgrade_without_home_refuses_new_default_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cfg = config(
        tmp.path(),
        &[
            ("default", tmp.path().join("replacement")),
            ("other", tmp.path().join("other")),
        ],
    );
    assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    let cfg = config(
        tmp.path(),
        &[
            ("default", tmp.path().join("codex-home")),
            ("other", tmp.path().join("other")),
        ],
    );
    BackendIsolation::prepare(&cfg, &repo).await.unwrap();
}

#[tokio::test]
async fn agent_backend_heal_refusal_preserves_supervisor_and_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let home = tmp.path().join("codex-home");
    let cfg = config(
        tmp.path(),
        &[
            ("default", home.clone()),
            ("other", tmp.path().join("other")),
        ],
    );
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_with_isolation(
        &cfg,
        repo.clone(),
        None,
        isolation,
    );
    let before = daemon.status_snapshot();
    let record = repo.shared_daemon_runtime_get("default").await.unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(daemon.ensure_running().await.is_err());
    assert_eq!(daemon.status_snapshot().state, before.state);
    let after = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.state, record.state);
    assert_eq!(after.updated_at, record.updated_at);
}

#[tokio::test]
async fn agent_backend_restart_rejects_reassigned_home() {
    let tmp = tempfile::tempdir().unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let default = tmp.path().join("codex-home");
    let cfg = config(
        tmp.path(),
        &[
            ("default", default.clone()),
            ("other", tmp.path().join("other")),
        ],
    );
    BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    let cfg = config(
        tmp.path(),
        &[
            ("default", default),
            ("other", tmp.path().join("replacement")),
        ],
    );
    let error = BackendIsolation::prepare(&cfg, &repo).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<calm_truth::db::sqlite::backend_registry::RegistryError>(),
        Some(calm_truth::db::sqlite::backend_registry::RegistryError::Changed(..))
    ));
    assert!(
        repo.shared_daemon_runtime_get("other")
            .await
            .unwrap()
            .pid
            .is_none()
    );
}

#[tokio::test]
async fn agent_backend_rejects_runtime_symlink_and_custom_log_collision() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let cfg = config(
        root,
        &[
            ("default", root.join("codex-home")),
            ("other", root.join("other")),
        ],
    );
    std::fs::create_dir(root.join("run")).unwrap();
    let default_socket = root.join("run/codex-appserver.sock");
    std::fs::write(&default_socket, "").unwrap();
    symlink(&default_socket, root.join("run/codex-appserver-other.sock")).unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    std::fs::remove_file(root.join("run/codex-appserver-other.sock")).unwrap();
    let mut cfg = cfg;
    cfg.shared_codex_appserver_log_dir = Some(root.join("logs/shared-codex-appserver-other"));
    assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
}

#[tokio::test]
async fn agent_backend_rejects_replaceable_ancestors_on_startup_and_recheck() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let cfg = config(
        tmp.path(),
        &[
            ("default", tmp.path().join("codex-home")),
            ("other", parent.join("home")),
        ],
    );
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    for mode in [0o770, 0o707, 0o777] {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(isolation.verify(repo.as_ref()).await.is_err());
        assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    }
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();
    // A parent replaced after verification must be checked on every entry.
    std::fs::rename(&parent, tmp.path().join("original")).unwrap();
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(isolation.verify(repo.as_ref()).await.is_err());
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o1777)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();

    // The current configured home is safe, but an adopted daemon may still
    // reopen its old runtime alias. Check that chain too, before takeover.
    use calm_truth::db::{SharedCodexDaemonUpdate, SharedDaemonState};
    let alias_parent = tmp.path().join("legacy-alias");
    std::fs::create_dir(&alias_parent).unwrap();
    let alias = alias_parent.join("home");
    symlink(tmp.path().join("codex-home"), &alias).unwrap();
    repo.shared_daemon_runtime_set(
        "default",
        SharedCodexDaemonUpdate {
            state: SharedDaemonState::Idle,
            pid: None,
            pgid: None,
            paths_are_legacy_display: false,
            sock_path: None,
            codex_home_path: Some(alias.as_os_str().as_bytes().to_vec()),
            process_start_time: None,
            boot_id: None,
            started_at: None,
            last_error: None,
            increment_restart_count: false,
            daemon_env_signature: None,
        },
    )
    .await
    .unwrap();
    let before = repo.shared_daemon_runtime_get("default").await.unwrap();
    std::fs::set_permissions(&alias_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(isolation.verify(repo.as_ref()).await.is_err());
    assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    let after = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after.codex_home_path, before.codex_home_path);
    std::fs::set_permissions(&alias_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();

    // Neither the lexical alias parents nor the final canonical home expose
    // this writable intermediate target parent without following link targets.
    let intermediate = tmp.path().join("intermediate");
    std::fs::create_dir(&intermediate).unwrap();
    let target = intermediate.join("home");
    symlink(tmp.path().join("codex-home"), &target).unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&target, &alias).unwrap();
    std::fs::set_permissions(&intermediate, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(isolation.verify(repo.as_ref()).await.is_err());
    assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    std::fs::set_permissions(&intermediate, std::fs::Permissions::from_mode(0o700)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();
}

#[tokio::test]
async fn agent_backend_single_home_alias_retarget_allows_verify() {
    use std::os::unix::fs::DirBuilderExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("codex-home");
    let target = tmp.path().join("private/home");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&target)
        .unwrap();
    symlink(&target, &home).unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cfg = Config::parse_from(["calm-server", "--data-dir", tmp.path().to_str().unwrap()]);
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    assert_eq!(isolation.default_home(), home);
    // Single-backend upgrades preserve legacy permissions and aliases.
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();
    let parent = target.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o777)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();
    let explicit = config(tmp.path(), &[("default", home.clone())]);
    let fresh: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    BackendIsolation::prepare(&explicit, &fresh).await.unwrap();
    let replacement = tmp.path().join("replacement");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&replacement)
        .unwrap();
    std::fs::remove_file(&home).unwrap();
    symlink(&replacement, &home).unwrap();
    isolation.verify(repo.as_ref()).await.unwrap();
    BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    assert!(!replacement.join("config.toml").exists());
}

#[tokio::test]
async fn agent_backend_runtime_alias_retarget_rejects_disjoint_namespaces() {
    // Every retarget remains secure and disjoint. Only the frozen identity
    // invariant can refuse it; overlap and permission checks are insufficient.
    for namespace in [
        "data",
        "run",
        "state",
        "logs/shared-codex-appserver",
        "run/codex-appserver.sock",
        "state/codex-authentication.json",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("data");
        std::fs::create_dir(&root).unwrap();
        let mut cfg = config(
            &root,
            &[
                ("default", root.join("codex-home")),
                ("other", tmp.path().join("other-home")),
            ],
        );
        let replacement = tmp.path().join("replacement");
        let alias = if namespace == "data" {
            std::fs::create_dir(&replacement).unwrap();
            let alias = tmp.path().join("data-alias");
            symlink(&root, &alias).unwrap();
            cfg.data_dir = Some(alias.clone());
            alias
        } else {
            let target = tmp.path().join("target");
            if namespace.ends_with(".sock") || namespace.ends_with(".json") {
                std::fs::write(&target, "").unwrap();
                std::fs::write(&replacement, "").unwrap();
            } else {
                std::fs::create_dir(&target).unwrap();
                std::fs::create_dir(&replacement).unwrap();
            }
            let alias = root.join(namespace);
            std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
            symlink(&target, &alias).unwrap();
            alias
        };
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
        std::fs::remove_file(&alias).unwrap();
        symlink(&replacement, &alias).unwrap();
        assert!(
            isolation.verify(repo.as_ref()).await.is_err(),
            "{namespace}"
        );
    }
}

#[tokio::test]
async fn agent_backend_runtime_missing_parents_created_private() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(
        tmp.path(),
        &[
            ("default", tmp.path().join("codex-home")),
            ("other", tmp.path().join("other")),
        ],
    );
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    for parent in ["run", "state", "logs", "logs/shared-codex-appserver", "mcp"] {
        let metadata = std::fs::metadata(tmp.path().join(parent)).unwrap();
        assert_eq!(metadata.uid(), unsafe { nix::libc::geteuid() });
        assert_eq!(metadata.mode() & 0o7777, 0o700, "{parent}");
    }
}

#[tokio::test]
async fn agent_backend_runtime_writable_parents_refused_without_chmod() {
    for parent in ["run", "state", "logs", "logs/shared-codex-appserver", "mcp"] {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config(
            tmp.path(),
            &[
                ("default", tmp.path().join("codex-home")),
                ("other", tmp.path().join("other")),
            ],
        );
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
        let parent = tmp.path().join(parent);
        for mode in [0o1777, 0o770, 0o707, 0o777] {
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(mode)).unwrap();
            for error in [
                isolation.verify(repo.as_ref()).await.err().unwrap(),
                BackendIsolation::prepare(&cfg, &repo).await.err().unwrap(),
            ] {
                assert!(
                    error
                        .to_string()
                        .contains(&format!("chmod 700 {}", parent.display()))
                );
            }
            assert_eq!(
                std::fs::metadata(&parent).unwrap().permissions().mode() & 0o7777,
                mode
            );
        }
        // Existing deployment directories with read/execute access remain valid.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        isolation.verify(repo.as_ref()).await.unwrap();
        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }
}

#[tokio::test]
async fn agent_backend_runtime_symlink_parent_refused_without_chmod() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("unrelated");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o1777)).unwrap();
    symlink(&target, tmp.path().join("logs")).unwrap();
    let cfg = config(
        tmp.path(),
        &[
            ("default", tmp.path().join("codex-home")),
            ("other", tmp.path().join("other")),
        ],
    );
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    assert!(BackendIsolation::prepare(&cfg, &repo).await.is_err());
    assert_eq!(
        std::fs::metadata(target).unwrap().permissions().mode() & 0o7777,
        0o1777
    );
}

#[tokio::test]
async fn agent_backend_single_0755_preserves_permissions_without_runtime_creation() {
    for explicit in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        for path in ["codex-home", "state", "logs", "logs/shared-codex-appserver"] {
            let path = tmp.path().join(path);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cfg = if explicit {
            config(tmp.path(), &[("default", tmp.path().join("codex-home"))])
        } else {
            Config::parse_from(["calm-server", "--data-dir", tmp.path().to_str().unwrap()])
        };
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
        isolation.verify(repo.as_ref()).await.unwrap();
        assert_eq!(isolation.default_home(), tmp.path().join("codex-home"));
        for path in ["codex-home", "state", "logs", "logs/shared-codex-appserver"] {
            assert_eq!(
                std::fs::metadata(tmp.path().join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o755
            );
        }
        assert!(!tmp.path().join("run").exists());
        assert!(!tmp.path().join("mcp").exists());
        assert!(
            repo.shared_daemon_runtime_get("default")
                .await
                .unwrap()
                .pid
                .is_none()
        );
    }
}

#[tokio::test]
async fn agent_backend_non_utf8_single_scope_survives_repeated_prepare() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    // Single-backend runtime paths retain non-UTF-8 bytes and allow a fresh scope.
    for alias in [false, true] {
        let data = tmp
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'd', 0xff]));
        std::fs::create_dir_all(&data).unwrap();
        let mut cfg = Config::parse_from(["calm-server"]);
        cfg.data_dir = Some(data.clone());
        if alias {
            let target = tmp
                .path()
                .join(std::ffi::OsString::from_vec(vec![b'h', 0xfe]));
            std::fs::create_dir(&target).unwrap();
            symlink(target, data.join("codex-home")).unwrap();
        }
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
        isolation.verify(repo.as_ref()).await.unwrap();
        BackendIsolation::prepare(&cfg, &repo).await.unwrap();
        let replacement = tmp
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'd', 0xfe]));
        cfg.data_dir = Some(replacement);
        BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    }
}

#[tokio::test]
async fn agent_backend_single_shared_database_skips_registry_access() {
    let tmp = tempfile::tempdir().unwrap();
    let concrete = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = concrete.clone();
    // Removing the registry makes both reads and writes fail if startup touches it.
    sqlx::query("DROP TABLE agent_backend_registry")
        .execute(concrete.pool())
        .await
        .unwrap();
    for explicit in [false, true] {
        for instance in ["first", "second"] {
            let root = tmp.path().join(format!("{explicit}-{instance}"));
            std::fs::create_dir(&root).unwrap();
            let cfg = if explicit {
                config(&root, &[("default", root.join("custom-home"))])
            } else {
                Config::parse_from(["calm-server", "--data-dir", root.to_str().unwrap()])
            };
            let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
            isolation.verify(repo.as_ref()).await.unwrap();
        }
    }
}

#[tokio::test]
async fn agent_backend_multi_shared_database_rejects_different_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let multi = |root: &std::path::Path| {
        config(
            root,
            &[
                ("default", root.join("codex-home")),
                ("other", root.join("other")),
            ],
        )
    };
    // Single startup leaves no persistent identity to constrain the first multi startup.
    let single = Config::parse_from(["calm-server", "--data-dir", first.to_str().unwrap()]);
    BackendIsolation::prepare(&single, &repo).await.unwrap();
    BackendIsolation::prepare(&multi(&second), &repo)
        .await
        .unwrap();
    let error = BackendIsolation::prepare(&multi(&first), &repo)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error.downcast_ref(),
        Some(calm_truth::db::sqlite::backend_registry::RegistryError::Changed(..))
    ));
}

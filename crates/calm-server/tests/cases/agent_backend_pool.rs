//! Infrastructure rollout through production construction and supervisor entry points.
use super::agent_backend_upgrade::{DaemonGroup, multi_backend_cfg};
use super::*;
use calm_server::agent_backend_pool::{codex::CodexBackends, identity::BackendIsolation};
use calm_server::event::EventBus;
use calm_server::pending_codex_threads::PendingThreadStartRegistry;

pub(super) async fn pool(config: &Config, repo: Arc<dyn Repo>) -> Arc<CodexBackends> {
    let isolation = BackendIsolation::prepare(config, &repo).await.unwrap();
    let pending = Arc::new(PendingThreadStartRegistry::new(
        repo.clone(),
        EventBus::new(),
    ));
    CodexBackends::build(config, repo, pending, isolation)
}

#[tokio::test]
async fn agent_backend_unconfigured_pool_starts_one_daemon_without_binding_or_registry() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let backends = pool(&cfg(&root), repo.clone()).await;
    assert_eq!(backends.instances().len(), 1);
    backends.default_only().boot().await.unwrap();
    let _cleanup = DaemonGroup(
        backends
            .default_only()
            .status_snapshot()
            .runtime
            .unwrap()
            .pid,
    );
    assert!(Arc::ptr_eq(
        backends.for_session("any-worker").await.unwrap(),
        backends.default_only()
    ));
    for table in ["worker_session_backends", "agent_backend_registry"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn agent_backend_two_daemons_keep_runtime_credentials_and_authentication_separate() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let backends = pool(&multi_backend_cfg(&root), repo.clone()).await;
    let mut cleanup = Vec::new();
    for instance in backends.instances() {
        let marker = format!("credential-{}", instance.backend_id().as_str());
        std::fs::write(instance.shared_home().path().join("auth.json"), &marker).unwrap();
        instance.boot().await.unwrap();
        let runtime = instance.status_snapshot().runtime.unwrap();
        cleanup.push(DaemonGroup(runtime.pid));
        let record = repo
            .shared_daemon_runtime_get(instance.backend_id().as_str())
            .await
            .unwrap();
        assert_eq!(record.pid, Some(runtime.pid));
        assert_eq!(
            record.sock_path,
            Some(instance.sock_path().as_os_str().as_bytes().to_vec())
        );
        assert_eq!(
            record.codex_home_path,
            Some(
                instance
                    .shared_home()
                    .path()
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            )
        );
        let env = instance.spawn_env_for_test().await.unwrap();
        assert_eq!(
            env["CODEX_HOME"].as_deref(),
            instance.shared_home().path().to_str()
        );
        assert_eq!(
            std::fs::read_to_string(instance.shared_home().path().join("auth.json")).unwrap(),
            marker
        );
        assert!(backends.for_session("absent-worker").await.is_err());
    }
    let default = backends.default_only();
    let other = &backends.instances()[1];
    assert_ne!(default.sock_path(), other.sock_path());
    assert_ne!(
        default.status_snapshot().runtime.unwrap().pid,
        other.status_snapshot().runtime.unwrap().pid
    );
    for suffix in ["", "-other"] {
        assert!(
            root.path()
                .join(format!("logs/shared-codex-appserver{suffix}/stdout.log"))
                .exists()
        );
    }
    default.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "error".into(),
        params: json!({"error":{"message":"Sign in again", "codexErrorInfo":"unauthorized"}}),
    });
    assert!(default.authentication_hold().is_some());
    assert!(other.authentication_hold().is_none());
    assert!(root.path().join("state/codex-authentication.json").exists());
    assert!(
        !root
            .path()
            .join("state/codex-authentication-other.json")
            .exists()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM worker_session_backends")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn agent_backend_b7_legacy_threads_only_resume_on_default() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card = seed_card(&repo, 1).await;
    seed_runtime_thread(&repo, &card, "legacy-thread").await;
    let backends = pool(&multi_backend_cfg(&root), repo).await;
    let mut cleanup = Vec::new();
    for instance in backends.instances() {
        instance.boot().await.unwrap();
        cleanup.push(DaemonGroup(instance.status_snapshot().runtime.unwrap().pid));
        let candidates = instance.resume_candidates_for_test();
        if instance.backend_id().as_str() == "default" {
            assert_eq!(candidates, vec![("legacy-thread".into(), card.clone())]);
            assert!(
                std::fs::read_to_string(instance.sock_path().with_extension("methods"))
                    .unwrap()
                    .lines()
                    .any(|method| method == "thread/resume")
            );
        } else {
            assert!(candidates.is_empty());
            assert!(
                !std::fs::read_to_string(instance.sock_path().with_extension("methods"))
                    .unwrap()
                    .lines()
                    .any(|method| method == "thread/resume")
            );
        }
    }
}

#[tokio::test]
async fn agent_backend_b7_explicit_bindings_restore_only_their_own_instance() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let backends = pool(&multi_backend_cfg(&root), repo.clone()).await;
    let card = seed_card(&repo, 1).await;
    let session = seed_runtime_thread(&repo, &card, "bound-thread").await;
    // A fixture represents the future session owner ordering: binding before native ID.
    let mut tx = repo.pool().begin().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=NULL WHERE id=?")
        .bind(&session)
        .execute(&mut *tx)
        .await
        .unwrap();
    calm_truth::db::sqlite::session_backend::insert_tx(&mut tx, &session, "codex", "other")
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id='bound-thread' WHERE id=?")
        .bind(&session)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut cleanup = Vec::new();
    for instance in backends.instances() {
        instance.boot().await.unwrap();
        cleanup.push(DaemonGroup(instance.status_snapshot().runtime.unwrap().pid));
        if instance.backend_id().as_str() == "default" {
            assert!(instance.resume_candidates_for_test().is_empty());
        } else {
            assert_eq!(
                instance.resume_candidates_for_test(),
                vec![("bound-thread".into(), card.clone())]
            );
        }
    }
    // Existing sessions resolve their immutable backend.
    assert!(Arc::ptr_eq(
        backends.for_session(&session).await.unwrap(),
        &backends.instances()[1]
    ));
}

pub(super) async fn bind_fixture(repo: &SqlxRepo, session: &str, thread: &str, backend: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=NULL WHERE id=?")
        .bind(session)
        .execute(&mut *tx)
        .await
        .unwrap();
    calm_truth::db::sqlite::session_backend::insert_tx(&mut tx, session, "codex", backend)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=? WHERE id=?")
        .bind(thread)
        .bind(session)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

struct BootFixture {
    state: calm_server::state::AppState,
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    cards: Vec<String>,
    _cleanup: Vec<DaemonGroup>,
}

async fn boot_fixture(root: &tempfile::TempDir) -> BootFixture {
    let mut config = multi_backend_cfg(root);
    config.db_url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("boot.db").display()
    );
    config.workspace_root = Some(root.path().join("workspaces"));
    config.plugins_dir = Some(root.path().join("plugins"));
    config.plugins_data_dir = Some(root.path().join("plugins-data"));
    config.auth_username = Some("fixture-owner".into());
    config.auth_password = Some("fixture-password".into());
    config.auth_dev_autologin = false;

    let repo = Arc::new(SqlxRepo::open(&config.db_url).await.unwrap());
    // Register infrastructure without starting it; represent PR2-owned bindings in fixture rows.
    let _pool = pool(&config, repo.clone()).await;
    let mut cards = Vec::new();
    for (index, backend) in ["default", "other"].into_iter().enumerate() {
        let card = seed_card(&repo, index).await;
        let thread = format!("thread-{backend}");
        let session = seed_runtime_thread(&repo, &card, &thread).await;
        bind_fixture(&repo, &session, &thread, backend).await;
        cards.push(card);
    }
    let (state, _auth) = calm_server::state::AppState::boot(&config)
        .await
        .unwrap()
        .into_parts();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if state
                .codex_backends
                .instances()
                .iter()
                .all(|instance| instance.status_snapshot().state == SharedDaemonState::Running)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let cleanup = state
        .codex_backends
        .instances()
        .iter()
        .map(|instance| DaemonGroup(instance.status_snapshot().runtime.unwrap().pid))
        .collect();
    let app = super::agent_backend_auth_router::application(state.clone()).await;
    BootFixture {
        state,
        app,
        repo,
        cards,
        _cleanup: cleanup,
    }
}

fn toolset(instance: &SharedCodexAppServer) -> Option<String> {
    let raw = std::fs::read_to_string(instance.shared_home().path().join("config.toml")).ok()?;
    let config: toml::Value = toml::from_str(&raw).ok()?;
    config
        .get("mcp_servers")?
        .get("neige")?
        .get("env")?
        .get("NEIGE_MCP_TOOLSET")?
        .as_str()
        .map(str::to_owned)
}

async fn wait_toolset_change(instance: &SharedCodexAppServer, previous: Option<String>) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let next = toolset(instance);
            if next.is_some() && next != previous {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn plugin_change(state: &calm_server::state::AppState) {
    state.events.emit(
        calm_server::ids::ActorId::Kernel,
        calm_server::event::Event::PluginState {
            id: "fixture-plugin".into(),
            state: "running".into(),
            last_error: None,
        },
    );
}

#[tokio::test]
async fn agent_backend_production_boot_plugin_refresh_fans_out_and_home_failure_is_independent() {
    let root = tempfile::tempdir().unwrap();
    let fx = boot_fixture(&root).await;
    let instances = fx.state.codex_backends.instances();
    for instance in instances {
        wait_toolset_change(instance, None).await;
    }
    let before: Vec<_> = instances.iter().map(|instance| toolset(instance)).collect();
    plugin_change(&fx.state);
    for (instance, previous) in instances.iter().zip(before) {
        wait_toolset_change(instance, previous).await;
    }
    let path = instances[0].shared_home().path().join("config.toml");
    let original = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let before = toolset(&instances[1]);
    plugin_change(&fx.state);
    wait_toolset_change(&instances[1], before).await;
    assert!(path.is_dir());
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, original).unwrap();
    let before = toolset(&instances[0]);
    plugin_change(&fx.state);
    wait_toolset_change(&instances[0], before).await;
}

async fn http(app: axum::Router, method: &str, uri: &str, body: Value) -> axum::http::StatusCode {
    use tower::ServiceExt;
    app.oneshot(
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn agent_backend_production_settings_respawns_each_daemon_and_sweeper_releases_each_cache() {
    let root = tempfile::tempdir().unwrap();
    let fx = boot_fixture(&root).await;
    let instances = fx.state.codex_backends.instances();
    let before: Vec<_> = instances
        .iter()
        .map(|instance| instance.status_snapshot().runtime.unwrap().pid)
        .collect();
    assert_eq!(
        http(
            fx.app.clone(),
            "PUT",
            "/api/settings",
            json!({"settings":{"http_proxy":"http://127.0.0.1:3128"}})
        )
        .await,
        axum::http::StatusCode::OK
    );
    let mut cleanup = Vec::new();
    for (instance, old) in instances.iter().zip(before) {
        instance
            .ensure_respawn_for_current_settings()
            .await
            .unwrap();
        let pid = instance.status_snapshot().runtime.unwrap().pid;
        cleanup.push(DaemonGroup(pid));
        assert_ne!(pid, old);
    }
    for card in &fx.cards {
        let mut tx = fx.repo.pool().begin().await.unwrap();
        calm_server::db::sqlite::session_complete_for_card_tx(
            &mut tx,
            card,
            WorkerSessionState::Exited,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    calm_server::terminal_sweeper::sweep(&fx.state)
        .await
        .unwrap();
    for (instance, card) in instances.iter().zip(&fx.cards) {
        let thread = format!("thread-{}", instance.backend_id().as_str());
        assert_eq!(instance.cached_card_for_thread(&thread), None, "{card}");
        let released =
            std::fs::read_to_string(instance.sock_path().with_extension("unsubscribed")).unwrap();
        assert!(released.lines().any(|id| id == thread));
    }
}

#[tokio::test]
async fn agent_backend_production_track_and_area_delete_forget_all_instances() {
    use calm_server::db::RepoRead;
    for area_delete in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let fx = boot_fixture(&root).await;
        for (instance, card) in fx.state.codex_backends.instances().iter().zip(&fx.cards) {
            let thread = format!("thread-{}", instance.backend_id().as_str());
            assert_eq!(instance.cached_card_for_thread(&thread), Some(card.clone()));
            let card_row = fx.repo.card_get(card).await.unwrap().unwrap();
            let track = fx
                .repo
                .track_get(card_row.track_id.as_str())
                .await
                .unwrap()
                .unwrap();
            let uri = if area_delete {
                format!("/api/areas/{}", track.area_id)
            } else {
                format!("/api/tracks/{}", track.id)
            };
            let status = http(fx.app.clone(), "DELETE", &uri, json!({})).await;
            assert!(status.is_success(), "{uri}: {status}");
            for backend in fx.state.codex_backends.instances() {
                assert_eq!(backend.cached_card_for_thread(&thread), None);
                assert!(backend.active_turn_id_for_thread(&thread).is_none());
            }
        }
    }
}

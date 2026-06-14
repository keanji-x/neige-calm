use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx, session_prepare_deferred_spec_tx};
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_types::worker::{WorkerSessionId, WorkerSessionState};
use serde_json::json;

const BOOT_ASSERT_ENV: &str = "NEIGE_ASSERT_WORKER_SESSIONS_PARITY_ON_BOOT";

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: these tests hold ENV_LOCK while mutating and reading this
        // process-global variable.
        unsafe { std::env::set_var(key, value) };
        Self { key, original }
    }

    fn unset(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: see `set`.
        unsafe { std::env::remove_var(key) };
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: EnvGuard is only used while ENV_LOCK is held in this test
        // binary, and Drop runs before that guard is released.
        unsafe {
            if let Some(value) = &self.original {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[tokio::test]
async fn worker_sessions_parity_sweep_detects_unmirrored_runtime() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    // This test deliberately seeds an unmirrored runtime so the sweep can flag it.
    repo.disable_worker_session_parity_on_drop_for_test();
    let card_id = create_codex_card(&repo).await;
    insert_unmirrored_runtime(&repo, card_id.as_str()).await;

    let counter = AtomicU64::new(0);
    let divergences = calm_server::worker_sessions_parity_sweep::sweep(repo.pool(), &counter)
        .await
        .unwrap();

    assert_eq!(divergences, 1);
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn boot_worker_sessions_parity_assertion_env_set_divergence_returns_err() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::set(BOOT_ASSERT_ENV, "1");
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    repo.disable_worker_session_parity_on_drop_for_test();
    let card_id = create_codex_card(&repo).await;
    insert_unmirrored_runtime(&repo, card_id.as_str()).await;
    let state = test_state(repo.clone()).await;

    let err = calm_server::assert_worker_sessions_parity_on_boot(&state)
        .await
        .expect_err("env-gated boot assertion should reject divergence");

    assert!(
        matches!(err, calm_server::error::CalmError::Internal(ref message) if message.contains("worker_sessions parity assertion failed on boot")),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn boot_worker_sessions_parity_assertion_env_set_clean_returns_ok() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::set(BOOT_ASSERT_ENV, "1");
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = create_codex_card(&repo).await;
    insert_mirrored_runtime(&repo, card_id.as_str()).await;
    let state = test_state(repo.clone()).await;

    calm_server::assert_worker_sessions_parity_on_boot(&state)
        .await
        .expect("env-gated boot assertion should pass when parity is clean");
}

#[tokio::test]
async fn boot_worker_sessions_parity_assertion_env_unset_divergence_returns_ok() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::unset(BOOT_ASSERT_ENV);
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    repo.disable_worker_session_parity_on_drop_for_test();
    let card_id = create_codex_card(&repo).await;
    insert_unmirrored_runtime(&repo, card_id.as_str()).await;
    let state = test_state(repo.clone()).await;

    calm_server::assert_worker_sessions_parity_on_boot(&state)
        .await
        .expect("unset boot assertion env must be a no-op");
}

#[tokio::test]
async fn deferred_spec_placeholder_session_is_not_parity_divergence() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let card_id = create_codex_card(&repo).await;
    let wave_id: String = sqlx::query_scalar("SELECT wave_id FROM cards WHERE id = ?1")
        .bind(&card_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    session_prepare_deferred_spec_tx(
        &mut tx,
        &RuntimeInit {
            id: runtime_id.clone(),
            card_id: card_id.clone(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(json!({"mode": "harness"})),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let runtime_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtimes WHERE id = ?1")
        .bind(&runtime_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        runtime_count, 0,
        "placeholder must not create a runtime row"
    );
    let session = repo
        .session_get(&WorkerSessionId::from(runtime_id.clone()))
        .await
        .unwrap()
        .expect("deferred placeholder session");
    assert_eq!(session.state, WorkerSessionState::Starting);
    assert_eq!(session.wave_id.as_str(), wave_id);

    let counter = AtomicU64::new(0);
    let divergences = calm_server::worker_sessions_parity_sweep::sweep(repo.pool(), &counter)
        .await
        .unwrap();
    assert_eq!(divergences, 0);
    assert_eq!(counter.load(Ordering::Relaxed), 0);
}

async fn create_codex_card(repo: &SqlxRepo) -> String {
    let cove = repo
        .cove_create(NewCove {
            name: "parity-sweep".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "parity sweep".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    repo.card_create(NewCard {
        wave_id: wave.id,
        kind: "codex".into(),
        sort: None,
        payload: json!({"schemaVersion": 1}),
    })
    .await
    .unwrap()
    .id
    .to_string()
}

async fn insert_unmirrored_runtime(repo: &SqlxRepo, card_id: &str) -> String {
    let runtime_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO runtimes
           (id, card_id, kind, agent_provider, status, created_at_ms, updated_at_ms)
           VALUES (?1, ?2, 'codex', 'codex', 'running', ?3, ?3)"#,
    )
    .bind(&runtime_id)
    .bind(card_id)
    .bind(now)
    .execute(repo.pool())
    .await
    .unwrap();
    runtime_id
}

async fn insert_mirrored_runtime(repo: &SqlxRepo, card_id: &str) -> String {
    let runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card_id.to_string(),
            kind: RuntimeKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Running,
            terminal_run_id: None,
            thread_id: Some("thread-clean".into()),
            session_id: Some("session-clean".into()),
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runtime_id
}

async fn test_state(repo: Arc<SqlxRepo>) -> AppState {
    let cache = calm_server::card_role_cache::CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    let events = EventBus::new();
    AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join(format!("calm-plugins-data-parity-{}", new_id())),
            Vec::new(),
            events.clone(),
            calm_server::state::WriteContext::new(cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache),
        Some(wave_cove_cache),
    )
}

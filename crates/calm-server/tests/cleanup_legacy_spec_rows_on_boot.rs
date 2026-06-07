use std::path::PathBuf;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::event::EventBus;
use calm_server::model::{CardPatch, CardRole, NewCard, NewCove, NewWave, WaveLifecycle};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::theme::RequestTheme;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::{Value, json};

async fn state(repo: Arc<SqlxRepo>) -> AppState {
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let repo_dyn: Arc<dyn calm_server::db::Repo> = repo.clone();

    AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: std::env::temp_dir()
                .join(format!(
                    "calm-cleanup-legacy-spec-daemon-{}",
                    uuid::Uuid::new_v4()
                ))
                .join("terminals"),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn,
            PathBuf::new(),
            std::env::temp_dir().join(format!(
                "calm-cleanup-legacy-spec-plugin-data-{}",
                uuid::Uuid::new_v4()
            )),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    )
}

async fn seed_spec(repo: &SqlxRepo, payload: Value, lifecycle: WaveLifecycle) -> String {
    let cove = repo
        .cove_create(NewCove {
            name: "cleanup".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "cleanup".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    if lifecycle != WaveLifecycle::Draft {
        sqlx::query("UPDATE waves SET lifecycle = ?1 WHERE id = ?2")
            .bind(lifecycle)
            .bind(wave.id.as_str())
            .execute(repo.pool())
            .await
            .unwrap();
    }
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = 'spec' WHERE id = ?1")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    card.id.to_string()
}

async fn seed_thread_mapping(repo: &SqlxRepo, card_id: &str, thread_id: &str) {
    let card = repo.card_get(card_id).await.unwrap().unwrap();
    repo.card_codex_thread_upsert(
        card_id,
        thread_id,
        CardRole::Spec,
        Some(card.wave_id.as_str()),
    )
    .await
    .unwrap();
}

async fn seed_active_shared_spec_runtime(repo: &SqlxRepo, card_id: &str, thread_id: Option<&str>) {
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: calm_server::model::new_id(),
            card_id: card_id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Running,
            terminal_run_id: None,
            thread_id: thread_id.map(str::to_string),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_does_not_persist_failed_status() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({"codex_source": "legacy", "prompt": "pre-pr8"}),
        WaveLifecycle::Draft,
    )
    .await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_unlinks_owned_appserver_sock() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({
            "codex_source": "legacy",
            "prompt": "pre-pr8"
        }),
        WaveLifecycle::Draft,
    )
    .await;
    let state = state(repo.clone()).await;
    let sock_dir = state.daemon.appserver_sock_dir(&card_id);
    std::fs::create_dir_all(&sock_dir).expect("create owned legacy sock dir");
    let sock = sock_dir.join("sock");
    std::fs::write(&sock, b"stale socket placeholder").expect("create stale sock file");
    repo.card_update(
        &card_id,
        CardPatch {
            payload: Some(json!({
                "codex_source": "legacy",
                "prompt": "pre-pr8",
                "appserver_sock": sock.to_string_lossy()
            })),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
    assert!(
        !sock.exists(),
        "stale persisted appserver_sock was not removed"
    );
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_rejects_traversal_appserver_sock() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let tmp = tempfile::tempdir().expect("tempdir for unrelated legacy sock");
    let unrelated = tmp.path().join("totally-unrelated");
    std::fs::write(&unrelated, b"must not be deleted").expect("create unrelated file");
    let card_id = seed_spec(
        &repo,
        json!({
            "codex_source": "legacy",
            "prompt": "pre-pr8",
            "appserver_sock": unrelated.to_string_lossy()
        }),
        WaveLifecycle::Draft,
    )
    .await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
    assert!(
        unrelated.exists(),
        "unrelated appserver_sock path was incorrectly removed"
    );
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_skips_reap_for_unverified_pgid() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({
            "codex_source": "legacy",
            "prompt": "pre-pr8",
            "appserver_pgid": 2,
            "appserver_start_time": 0,
            "appserver_boot_id": "not-this-boot"
        }),
        WaveLifecycle::Draft,
    )
    .await;
    seed_thread_mapping(&repo, &card_id, "thread-unverified").await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
    assert!(
        repo.card_codex_thread_get_by_card(&card_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_skips_reap_for_init_pgid() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({
            "codex_source": "legacy",
            "prompt": "pre-pr8",
            "appserver_pgid": 1,
            "appserver_start_time": 0,
            "appserver_boot_id": "not-this-boot"
        }),
        WaveLifecycle::Draft,
    )
    .await;
    seed_thread_mapping(&repo, &card_id, "thread-init").await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
    assert!(
        repo.card_codex_thread_get_by_card(&card_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_treats_payload_shared_without_runtime_as_legacy() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({"codex_source": "shared", "codex_thread_id": "T1"}),
        WaveLifecycle::Draft,
    )
    .await;
    seed_thread_mapping(&repo, &card_id, "T1").await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
    assert!(
        repo.card_codex_thread_get_by_card(&card_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_skips_active_shared_spec_runtime() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({"codex_source": "shared", "codex_thread_id": "T1"}),
        WaveLifecycle::Draft,
    )
    .await;
    seed_active_shared_spec_runtime(&repo, &card_id, Some("T1")).await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
    let runtime = repo
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active shared runtime");
    assert_eq!(runtime.status, RunStatus::Running);
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_skips_done_waves() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({"codex_source": "legacy", "prompt": "complete"}),
        WaveLifecycle::Done,
    )
    .await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
}

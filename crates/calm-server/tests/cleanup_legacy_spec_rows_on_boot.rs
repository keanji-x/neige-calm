use std::path::PathBuf;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, WaveLifecycle};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::theme::RequestTheme;
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
        Arc::new(DaemonClient::new_stub()),
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
            card_role_cache.clone(),
            wave_cove_cache.clone(),
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

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_marks_legacy_specs_as_failed_to_spawn() {
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
    assert_eq!(card.payload["codex_thread_status"], "failed_to_spawn");
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
    assert_eq!(card.payload["codex_thread_status"], "failed_to_spawn");
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
    assert_eq!(card.payload["codex_thread_status"], "failed_to_spawn");
    assert!(
        repo.card_codex_thread_get_by_card(&card_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cleanup_legacy_spec_rows_on_boot_skips_shared_specs() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_spec(
        &repo,
        json!({"codex_source": "shared", "codex_thread_id": "T1"}),
        WaveLifecycle::Draft,
    )
    .await;
    let state = state(repo.clone()).await;

    calm_server::cleanup_legacy_spec_rows_on_boot(&state).await;

    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    assert!(card.payload.get("codex_thread_status").is_none());
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

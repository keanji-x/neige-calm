use std::path::PathBuf;
use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx};
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewWave};
use calm_server::operation::OperationKey;
use calm_server::operation::forge_action_adapter::FORGE_ACTION_KIND;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::wave_area_cache::WaveAreaCache;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};

async fn app_state(repo: Arc<SqlxRepo>) -> AppState {
    let repo_dyn: Arc<dyn Repo> = repo;
    AppState::from_parts(
        repo_dyn.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn,
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                WaveAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
}

#[tokio::test]
async fn boot_assert_card_id_complete_still_runs_post_9b_iv() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "boot-invariant".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            area_id: area.id,
            title: "boot invariant".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let mut tx = repo.pool().begin().await.unwrap();
    session_insert_tx(
        &mut tx,
        WorkerSession {
            id: WorkerSessionId::from("ws-null-active"),
            wave_id: wave.id,
            provider: WorkerProviderKind::Codex,
            mode: SessionMode::Resumable,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Running,
            mcp_token_hash: None,
            thread_id: Some("thread-null-active".into()),
            agent_session_id: None,
            active_turn_id: None,
            terminal_run_id: None,
            card_id: None,
            handle_state_json: None,
            liveness: LivenessTag::Unknown,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let state = app_state(repo).await;
    let err = calm_server::assert_worker_sessions_card_id_complete_on_boot(&state)
        .await
        .expect_err("active NULL-card worker session must fail boot assertion");
    assert!(
        err.to_string().contains("worker_sessions.card_id"),
        "unexpected boot assertion error: {err}"
    );
}

/// Seed `n` areas and give each one the matching path via the
/// **unchecked** repo primitive (`area_folder_create`), which is exactly
/// the writer that a pre-#275 database's overlapping rows came from —
/// today's `area_folder_create_checked` would refuse them, so the fence
/// could not be tested through it.
/// Returns the seeded `area_id`s, positionally matching `paths`.
async fn seed_folders(repo: &SqlxRepo, paths: &[&str]) -> Vec<String> {
    let mut area_ids = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        let area = repo
            .area_create(NewArea {
                name: format!("area-{i}"),
                color: "#222222".into(),
                sort: None,
            })
            .await
            .unwrap();
        repo.area_folder_create(area.id.as_str(), path)
            .await
            .unwrap();
        area_ids.push(area.id.as_str().to_string());
    }
    area_ids
}

#[tokio::test]
async fn boot_fence_rejects_overlapping_area_folder_claims() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area_ids = seed_folders(&repo, &["/a", "/a/b"]).await;

    let state = app_state(repo).await;
    let err = calm_server::assert_area_folders_disjoint_on_boot(&state)
        .await
        .expect_err("overlapping area_folders claims must fail the boot fence");
    let msg = err.to_string();
    assert!(
        msg.contains("area_folders boot fence failed"),
        "unexpected fence error: {msg}"
    );
    // Actionability: both sides of the pair must be nameable by an
    // operator straight from the message.
    assert!(msg.contains("path=`/a`"), "fence must name /a: {msg}");
    assert!(msg.contains("path=`/a/b`"), "fence must name /a/b: {msg}");
    for area_id in &area_ids {
        assert!(
            msg.contains(area_id.as_str()),
            "fence must name area_id {area_id}: {msg}"
        );
    }
    // Row ids too, so the operator can `DELETE ... WHERE id = ?`.
    assert!(msg.contains("id=1"), "fence must name row ids: {msg}");
    assert!(msg.contains("id=2"), "fence must name row ids: {msg}");
}

#[tokio::test]
async fn boot_fence_passes_on_disjoint_area_folder_claims() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    seed_folders(&repo, &["/a", "/b", "/c/d/e"]).await;

    let state = app_state(repo).await;
    calm_server::assert_area_folders_disjoint_on_boot(&state)
        .await
        .expect("disjoint area_folders claims must pass the boot fence");
}

/// The adjacent case the fence must NOT trip on: sibling paths that
/// share a *string* prefix but not a *path* prefix. Tripping here would
/// brick booting on a perfectly valid table.
#[tokio::test]
async fn boot_fence_passes_on_string_prefix_siblings() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    seed_folders(&repo, &["/a", "/ab", "/home/ken", "/home/kenji"]).await;

    let state = app_state(repo).await;
    calm_server::assert_area_folders_disjoint_on_boot(&state)
        .await
        .expect("string-prefix siblings are disjoint paths and must pass the fence");
}

/// An empty table is trivially disjoint — a fresh install must boot.
#[tokio::test]
async fn boot_fence_passes_on_empty_area_folders() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = app_state(repo).await;
    calm_server::assert_area_folders_disjoint_on_boot(&state)
        .await
        .expect("empty area_folders must pass the boot fence");
}

/// `UNIQUE(area_folders.path)` makes the exact-equal overlap
/// unreachable even through the unchecked primitive. Pinning that here
/// documents *why* the DB-level fence test cannot cover `Equal` (the
/// pure-function test in `area_folder_claim` does).
#[tokio::test]
async fn equal_paths_are_unreachable_even_through_the_unchecked_primitive() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    seed_folders(&repo, &["/a"]).await;
    let area = repo
        .area_create(NewArea {
            name: "dup".into(),
            color: "#333333".into(),
            sort: None,
        })
        .await
        .unwrap();
    repo.area_folder_create(area.id.as_str(), "/a")
        .await
        .expect_err("UNIQUE(area_folders.path) rejects an exactly-equal claim");
}

#[tokio::test]
async fn production_operation_runtime_recognizes_forge_action_kind() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = app_state(repo).await;
    let err = state
        .operation_runtime
        .submit(
            FORGE_ACTION_KIND,
            OperationKey {
                operation_key: "forge-action-production-runtime".into(),
                idempotency_key: Some("forge-action-production-runtime".into()),
                payload_hash: "forge-action-production-runtime:null".into(),
            },
            serde_json::Value::Null,
        )
        .await
        .expect_err("null forge-action payload should be rejected by the adapter");
    assert!(
        !err.to_string()
            .contains("unknown operation kind forge-action"),
        "forge-action must be registered in build_operation_adapters: {err}"
    );
}

use std::path::PathBuf;
use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx};
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::wave_cove_cache::WaveCoveCache;
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
                WaveCoveCache::new(),
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
    let cove = repo
        .cove_create(NewCove {
            name: "boot-invariant".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "boot invariant".into(),
            sort: None,
            cwd: "/tmp".into(),
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

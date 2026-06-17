use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn fresh() -> (axum::Router, Arc<SqlxRepo>, String) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "threads claude".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "thread claude map".into(),
            sort: None,
            cwd: "/workspace".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let app = axum::Router::new()
        .merge(routes::router())
        .with_state(state);
    (app, repo, wave.id.to_string())
}

async fn create_claude_card(repo: &SqlxRepo, wave_id: &str) -> String {
    let cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave_id.into(),
            kind: "claude".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Worker,
        true,
        &cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    card.id.to_string()
}

async fn bind_claude_session(repo: &SqlxRepo, card_id: &str, session_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: new_id(),
            card_id: card_id.to_string(),
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: None,
            session_id: Some(session_id.to_string()),
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn get(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn resolve_card_for_claude_session_requires_claude_provider() {
    let (app, repo, wave_id) = fresh().await;
    let card_id = create_claude_card(&repo, &wave_id).await;
    let session_id = "11111111-1111-4111-8111-111111111111";
    bind_claude_session(&repo, &card_id, session_id).await;

    let (status, body) = get(
        app.clone(),
        format!("/api/threads/{session_id}/card?provider=claude"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body:?}");
    assert_eq!(body["thread_id"], session_id);
    assert_eq!(body["card_id"], card_id);
    assert_eq!(body["role"], "worker");
    assert_eq!(body["wave_id"], wave_id);

    let (status, body) = get(app, format!("/api/threads/{session_id}/card")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body:?}");
}

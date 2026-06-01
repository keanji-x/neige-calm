use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx};
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn fresh() -> (axum::Router, Arc<SqlxRepo>, String) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "threads".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "thread map".into(),
            sort: None,
            cwd: "/workspace".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
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

async fn create_card(repo: &SqlxRepo, wave_id: &str, role: CardRole) -> String {
    let cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave_id.into(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        },
        role,
        true,
        &cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    card.id.to_string()
}

async fn get(app: axum::Router, thread_id: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/threads/{thread_id}/card"))
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
async fn resolve_card_for_thread_returns_mapping() {
    let (app, repo, wave_id) = fresh().await;
    let card_id = create_card(&repo, &wave_id, CardRole::Plain).await;
    repo.card_codex_thread_upsert(&card_id, "thread-hit", CardRole::Plain, None)
        .await
        .unwrap();

    let (status, body) = get(app, "thread-hit").await;
    assert_eq!(status, StatusCode::OK, "body={body:?}");
    assert_eq!(body["thread_id"], "thread-hit");
    assert_eq!(body["card_id"], card_id);
}

#[tokio::test]
async fn resolve_card_for_thread_404s_for_missing_thread() {
    let (app, _repo, _wave_id) = fresh().await;
    let (status, body) = get(app, "missing-thread").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body:?}");
}

#[tokio::test]
async fn resolve_card_for_thread_preserves_role() {
    let (app, repo, wave_id) = fresh().await;
    let card_id = create_card(&repo, &wave_id, CardRole::Worker).await;
    repo.card_codex_thread_upsert(&card_id, "thread-worker", CardRole::Worker, Some(&wave_id))
        .await
        .unwrap();

    let (status, body) = get(app, "thread-worker").await;
    assert_eq!(status, StatusCode::OK, "body={body:?}");
    assert_eq!(body["role"], "worker");
}

#[tokio::test]
async fn resolve_card_for_thread_preserves_wave_id() {
    let (app, repo, wave_id) = fresh().await;
    let card_id = create_card(&repo, &wave_id, CardRole::Spec).await;
    repo.card_codex_thread_upsert(&card_id, "thread-spec", CardRole::Spec, Some(&wave_id))
        .await
        .unwrap();

    let (status, body) = get(app, "thread-spec").await;
    assert_eq!(status, StatusCode::OK, "body={body:?}");
    assert_eq!(body["wave_id"], wave_id);
}

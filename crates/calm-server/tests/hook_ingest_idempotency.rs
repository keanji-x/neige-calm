use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use calm_server::actor::actor_middleware;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use tower::ServiceExt;

#[tokio::test]
async fn duplicate_codex_hook_is_acked_without_second_event() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    let cache = calm_server::card_role_cache::CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

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
            events.clone(),
            calm_server::state::WriteContext::new(cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache),
        Some(wave_cove_cache),
    );
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);
    let mut rx = events.subscribe();

    let body = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "session-1",
        "transcript_path": "/tmp/neige-session-1.jsonl",
        "transcript_size_bytes": 4096,
    })
    .to_string();
    let uri = format!("/internal/codex/hook?card_id={}", card.id);

    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 204);
    }

    let env = rx.recv().await.expect("first event emitted");
    match env.event {
        Event::CodexHook {
            card_id,
            kind,
            hook_idempotency_key,
            ..
        } => {
            assert_eq!(card_id.as_str(), card.id.as_str());
            assert_eq!(kind, "hook.codex.stop");
            assert!(!hook_idempotency_key.is_empty());
        }
        other => panic!("expected CodexHook, got {other:?}"),
    }

    let second = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        second.is_err(),
        "duplicate hook post emitted an unexpected second event: {second:?}"
    );
}

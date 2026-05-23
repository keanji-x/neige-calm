//! Verifies the codex hook ingest path: POSTing a fake codex hook
//! payload to the internal endpoint produces a `codex.hook` event on the
//! bus, with the snake_case `hook.codex.<event>` discriminator.
//!
//! Doesn't spawn an actual `codex` CLI — that's separately covered by
//! the unit tests in `routes/codex.rs::tests` (hooks.json shape +
//! snake_case derivation).

use std::sync::Arc;

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
async fn ingest_emits_codex_hook_event() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    // PR3 (#136) — the ingest path stamps `ActorId::AiCodex(card_id)` and
    // the role gate refuses unknown cards. Seed a real card so the gate
    // lets the write through.
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
    repo.seed_card_role_cache(&cache).await.unwrap();

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
            cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache),
    );
    // Scope β: the actor middleware must be present — the `ingest_hook`
    // handler now extracts `Actor` from request extensions to honor the
    // `X-Calm-Actor` header the bridge sends. Without the middleware the
    // extractor returns 500 ("middleware not applied").
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);
    let mut rx = events.subscribe();

    let body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "ls -la" },
    })
    .to_string();

    let uri = format!("/internal/codex/hook?card_id={}", card.id);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let env = rx.recv().await.expect("event emitted");
    // Sync engine phase 1: bus carries `BroadcastEnvelope { id, event }`.
    // CodexHook is persisted via `log_pure_event`, so `id` must be > 0.
    assert!(env.id > 0, "expected real events.id, got {}", env.id);
    match env.event {
        Event::CodexHook {
            card_id,
            kind,
            payload,
        } => {
            assert_eq!(card_id.as_str(), card.id.as_str());
            assert_eq!(kind, "hook.codex.pre_tool_use");
            assert_eq!(payload["tool_name"], "Bash");
        }
        other => panic!("expected CodexHook, got {other:?}"),
    }
}

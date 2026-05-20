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
use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use tower::ServiceExt;

#[tokio::test]
async fn ingest_emits_codex_hook_event() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let state = AppState {
        repo: repo.clone(),
        events: events.clone(),
        daemon: Arc::new(DaemonClient::new_stub()),
        plugin: Arc::new(PluginHost::new(Arc::new(PluginRegistry::empty()), repo)),
        codex: Arc::new(CodexClient::new_stub()),
    };
    let app = axum::Router::new()
        .merge(routes::router())
        .with_state(state);
    let mut rx = events.subscribe();

    let body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "ls -la" },
    })
    .to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/codex/hook?card_id=card_42")
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
            assert_eq!(card_id, "card_42");
            assert_eq!(kind, "hook.codex.pre_tool_use");
            assert_eq!(payload["tool_name"], "Bash");
        }
        other => panic!("expected CodexHook, got {other:?}"),
    }
}

#[tokio::test]
async fn create_codex_rejects_non_codex_card() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    // Seed a cove + wave + terminal card so we can fail the kind check.
    let cove = repo
        .cove_create(calm_server::model::NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(calm_server::model::NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();

    let state = AppState {
        repo: repo.clone(),
        events: EventBus::new(),
        daemon: Arc::new(DaemonClient::new_stub()),
        plugin: Arc::new(PluginHost::new(Arc::new(PluginRegistry::empty()), repo)),
        codex: Arc::new(CodexClient::new_stub()),
    };
    // Scope G: production wiring includes the actor middleware on the REST
    // router; without it the `Actor` extractor returns 500 (its "middleware
    // not applied" branch). Mirror main.rs.
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/codex", card.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "initial_prompt": "hi" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

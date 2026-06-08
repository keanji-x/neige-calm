//! Verifies the codex hook ingest path: POSTing a fake codex hook
//! payload to the internal endpoint produces a `codex.hook` event on the
//! bus, with the snake_case `hook.codex.<event>` discriminator.
//!
//! Doesn't spawn an actual `codex` CLI — the hook source itself lives in
//! `docker/codex-requirements.toml` (policy-managed, bind-mounted into
//! the container), and the snake_case derivation is covered by the unit
//! tests in `routes/codex.rs::tests`.

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
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn ingest_emits_codex_hook_event() {
    let (app, _repo, events, card_id) = test_app().await;
    let mut rx = events.subscribe();

    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "ls -la" },
    });

    post_and_assert(
        &app,
        &mut rx,
        card_id.as_str(),
        payload,
        "hook.codex.pre_tool_use",
    )
    .await;
}

#[tokio::test]
async fn codex_ingest_stop_hook() {
    let (app, _repo, events, card_id) = test_app().await;
    let mut rx = events.subscribe();

    let payload = json!({
        "hook_event_name": "Stop",
        "session_id": "session-stop",
        "transcript_path": "/tmp/neige-stop.jsonl",
        "transcript_size_bytes": 128,
    });

    post_and_assert(&app, &mut rx, card_id.as_str(), payload, "hook.codex.stop").await;
}

#[tokio::test]
async fn codex_ingest_stop_failure_hook() {
    let (app, _repo, events, card_id) = test_app().await;
    let mut rx = events.subscribe();

    let payload = json!({
        "hook_event_name": "StopFailure",
        "error": "rate_limit",
        "error_details": "429 Too Many Requests",
    });

    post_and_assert(
        &app,
        &mut rx,
        card_id.as_str(),
        payload,
        "hook.codex.stop_failure",
    )
    .await;
}

#[tokio::test]
async fn codex_ingest_session_end_hook() {
    let (app, _repo, events, card_id) = test_app().await;
    let mut rx = events.subscribe();

    let payload = json!({
        "hook_event_name": "SessionEnd",
        "reason": "prompt_input_exit",
    });

    post_and_assert(
        &app,
        &mut rx,
        card_id.as_str(),
        payload,
        "hook.codex.session_end",
    )
    .await;
}

async fn test_app() -> (axum::Router, Arc<SqlxRepo>, EventBus, String) {
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
            calm_server::state::WriteContext::new(
                cache.clone(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache),
        Some(calm_server::wave_cove_cache::WaveCoveCache::new()),
    );
    // Scope β: the actor middleware must be present — the `ingest_hook`
    // handler now extracts `Actor` from request extensions to honor the
    // `X-Calm-Actor` header the bridge sends. Without the middleware the
    // extractor returns 500 ("middleware not applied").
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    (app, repo, events, card.id.to_string())
}

async fn post_and_assert(
    app: &axum::Router,
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
    card_id: &str,
    payload: Value,
    expected_kind: &str,
) {
    let uri = format!("/internal/codex/hook?card_id={card_id}");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let env = rx.recv().await.expect("event emitted");
    assert!(env.id > 0, "expected real events.id, got {}", env.id);
    match env.event {
        Event::CodexHook {
            card_id: event_card_id,
            kind,
            hook_idempotency_key,
            payload: event_payload,
        } => {
            assert_eq!(event_card_id.as_str(), card_id);
            assert_eq!(kind, expected_kind);
            assert!(!hook_idempotency_key.is_empty());
            assert_eq!(event_payload, payload);
        }
        other => panic!("expected CodexHook, got {other:?}"),
    }
}

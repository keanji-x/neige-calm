//! Verifies the Claude hook ingest path: POSTing fake Claude Code hook
//! payloads to the internal endpoint persists `claude.hook` events with
//! `ActorId::AiClaude(card_id)` and the snake_case `hook.claude.<event>`
//! discriminator.

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use calm_server::actor::actor_middleware;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{Event, EventBus};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn ingest_emits_and_persists_claude_hook_events() {
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
            kind: "claude".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();

    sqlx::query("UPDATE cards SET role = 'worker' WHERE id = ?1")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    assert_eq!(cache.get(&card.id), Some(CardRole::Worker));

    let wave_cove_cache = WaveCoveCache::new();
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

    let runtime_id =
        bind_claude_runtime_session(repo.as_ref(), card.id.as_str(), "claude-session-active").await;
    let active_payload = json!({
        "hook_event_name": "Stop",
        "session_id": "claude-session-active",
    });
    post_and_assert(
        &app,
        repo.as_ref(),
        &mut rx,
        card.id.as_str(),
        active_payload,
        "hook.claude.stop",
        json!({ "kind": "AiClaudeSession", "id": runtime_id }),
    )
    .await;

    let unresolved_payload = json!({
        "hook_event_name": "Stop",
        "session_id": "missing-claude-session",
    });
    post_and_assert(
        &app,
        repo.as_ref(),
        &mut rx,
        card.id.as_str(),
        unresolved_payload,
        "hook.claude.stop",
        json!({ "kind": "AiClaude", "id": card.id.as_str() }),
    )
    .await;

    let pre_tool_payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test -p calm-server" },
    });
    post_and_assert(
        &app,
        repo.as_ref(),
        &mut rx,
        card.id.as_str(),
        pre_tool_payload,
        "hook.claude.pre_tool_use",
        json!({ "kind": "AiClaude", "id": card.id.as_str() }),
    )
    .await;

    let stop_payload = json!({
        "hook_event_name": "Stop",
    });
    post_and_assert(
        &app,
        repo.as_ref(),
        &mut rx,
        card.id.as_str(),
        stop_payload,
        "hook.claude.stop",
        json!({ "kind": "AiClaude", "id": card.id.as_str() }),
    )
    .await;

    let stop_failure_payload = json!({
        "hook_event_name": "StopFailure",
        "error": "rate_limit",
        "error_details": "429 Too Many Requests",
    });
    post_and_assert(
        &app,
        repo.as_ref(),
        &mut rx,
        card.id.as_str(),
        stop_failure_payload,
        "hook.claude.stop_failure",
        json!({ "kind": "AiClaude", "id": card.id.as_str() }),
    )
    .await;

    let session_end_payload = json!({
        "hook_event_name": "SessionEnd",
        "reason": "prompt_input_exit",
    });
    post_and_assert(
        &app,
        repo.as_ref(),
        &mut rx,
        card.id.as_str(),
        session_end_payload,
        "hook.claude.session_end",
        json!({ "kind": "AiClaude", "id": card.id.as_str() }),
    )
    .await;
}

async fn bind_claude_runtime_session(repo: &SqlxRepo, card_id: &str, session_id: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
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
    runtime.id
}

async fn post_and_assert(
    app: &axum::Router,
    repo: &SqlxRepo,
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
    card_id: &str,
    payload: Value,
    expected_kind: &str,
    expected_actor: Value,
) {
    let uri = format!("/internal/claude/hook?card_id={card_id}");
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
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({ "continue": true })
    );

    let env = rx.recv().await.expect("event emitted");
    assert!(env.id > 0, "expected real events.id, got {}", env.id);
    match &env.event {
        Event::ClaudeHook {
            card_id: event_card_id,
            kind,
            hook_idempotency_key,
            payload: event_payload,
        } => {
            assert_eq!(event_card_id.as_str(), card_id);
            assert_eq!(kind, expected_kind);
            assert!(!hook_idempotency_key.is_empty());
            assert_eq!(event_payload, &payload);
        }
        other => panic!("expected ClaudeHook, got {other:?}"),
    }

    let row: (String, String, String, String, String, String, String) = sqlx::query_as(
        "SELECT kind, actor, payload, scope_kind, scope_card, scope_wave, scope_cove \
         FROM events WHERE id = ?1",
    )
    .bind(env.id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "claude.hook");
    assert_eq!(
        serde_json::from_str::<Value>(&row.1).unwrap(),
        expected_actor
    );
    let stored_payload = serde_json::from_str::<Value>(&row.2).unwrap();
    assert_eq!(stored_payload["card_id"], card_id);
    assert_eq!(stored_payload["kind"], expected_kind);
    assert_eq!(stored_payload["payload"], payload);
    assert_eq!(row.3, "card");
    assert_eq!(row.4, card_id);
    assert!(!row.5.is_empty(), "scope_wave should be persisted");
    assert!(!row.6.is_empty(), "scope_cove should be persisted");
}

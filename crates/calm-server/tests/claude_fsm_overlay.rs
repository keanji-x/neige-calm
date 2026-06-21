//! Regression anchor: Claude hook ingest -> role gate -> card FSM -> card
//! status overlay, end-to-end at the kernel level (no real Claude Code CLI
//! involved).
//!
//! Claude worker `Stop` matches codex foreground-agent `Stop`: the worker
//! is waiting for the next user prompt, so the card projects as
//! `AwaitingInput`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use calm_server::actor::actor_middleware;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::{Value, json};
use tower::ServiceExt;

const OVERLAY_DEADLINE: Duration = Duration::from_secs(2);
const OVERLAY_POLL: Duration = Duration::from_millis(50);

async fn setup() -> (axum::Router, Arc<dyn Repo>, String) {
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
            workflow_id: None,
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
            payload: json!({}),
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
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-claude-fsm-overlay"),
            Vec::new(),
            events.clone(),
            calm_server::state::WriteContext::new(cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    calm_server::card_fsm::spawn(
        repo_dyn.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wave_cove_cache),
    );
    tokio::task::yield_now().await;

    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    (app, repo_dyn, card.id.to_string())
}

async fn post_claude_hook(app: &axum::Router, card_id: &str, payload: Value) {
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
    assert_eq!(
        resp.status(),
        200,
        "POST /internal/claude/hook expected 200; got {}",
        resp.status()
    );
}

async fn await_card_state(repo: &Arc<dyn Repo>, card_id: &str, expected_state: &str) {
    let poll = async {
        loop {
            let overlays = repo.overlays_for("card", card_id).await.unwrap();
            if let Some(o) = overlays.iter().find(|o| o.kind == "status")
                && o.payload.get("state").and_then(Value::as_str) == Some(expected_state)
            {
                return;
            }
            tokio::time::sleep(OVERLAY_POLL).await;
        }
    };

    if tokio::time::timeout(OVERLAY_DEADLINE, poll).await.is_err() {
        let overlays = repo.overlays_for("card", card_id).await.unwrap();
        panic!(
            "timed out waiting for card status overlay `state: {expected_state}` on card \
             {card_id}; current card overlays: {overlays:?}",
        );
    }
}

#[tokio::test]
async fn claude_stop_sets_worker_card_awaiting_input() {
    let (app, repo, card_id) = setup().await;

    post_claude_hook(
        &app,
        &card_id,
        json!({
            "hook_event_name": "Stop",
        }),
    )
    .await;

    await_card_state(&repo, &card_id, "AwaitingInput").await;
}

#[tokio::test]
async fn claude_activity_and_permission_hooks_set_distinct_card_states() {
    let (app, repo, card_id) = setup().await;

    post_claude_hook(
        &app,
        &card_id,
        json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_response": { "exit_code": 0 },
        }),
    )
    .await;
    await_card_state(&repo, &card_id, "Working").await;

    post_claude_hook(
        &app,
        &card_id,
        json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test -p calm-server" },
        }),
    )
    .await;
    await_card_state(&repo, &card_id, "AwaitingInput").await;
}

#[tokio::test]
async fn claude_subagent_stop_sets_worker_card_working() {
    let (app, repo, card_id) = setup().await;
    post_claude_hook(&app, &card_id, json!({ "hook_event_name": "SubagentStop" })).await;
    await_card_state(&repo, &card_id, "Working").await;
}

#[tokio::test]
async fn claude_task_completed_sets_worker_card_working() {
    let (app, repo, card_id) = setup().await;
    post_claude_hook(
        &app,
        &card_id,
        json!({ "hook_event_name": "TaskCompleted" }),
    )
    .await;
    await_card_state(&repo, &card_id, "Working").await;
}

#[tokio::test]
async fn claude_elicitation_sets_worker_card_awaiting_input() {
    let (app, repo, card_id) = setup().await;
    post_claude_hook(&app, &card_id, json!({ "hook_event_name": "Elicitation" })).await;
    await_card_state(&repo, &card_id, "AwaitingInput").await;
}

#[tokio::test]
async fn claude_permission_denied_sets_worker_card_awaiting_input() {
    let (app, repo, card_id) = setup().await;
    post_claude_hook(
        &app,
        &card_id,
        json!({ "hook_event_name": "PermissionDenied" }),
    )
    .await;
    await_card_state(&repo, &card_id, "AwaitingInput").await;
}

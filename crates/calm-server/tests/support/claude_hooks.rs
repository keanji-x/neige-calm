use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_exec::flow::WorkerFlowSource;
use calm_server::actor::actor_middleware;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::WorkerSessionProjection;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::worker_flow::claude_transcript::{
    ClaudeTranscriptFlowSource, ClaudeTranscriptFlowSourceOptions,
};
use calm_truth::worker_flow_sink::WorkerFlowSink;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use super::worker_flow::{self as wf, SeededRuntime};

/// Ingests a Claude hook through the production `/internal/claude/hook` route.
pub async fn post_claude_hook(repo: &Arc<SqlxRepo>, card_id: &str, payload: Value) {
    post_claude_hook_on(repo, EventBus::new(), card_id, payload).await;
}

/// [`post_claude_hook`] publishing on `events`, for a subscriber that must see the hook event.
pub async fn post_claude_hook_on(
    repo: &Arc<SqlxRepo>,
    events: EventBus,
    card_id: &str,
    payload: Value,
) {
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(hook_app_state(repo, events));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/internal/claude/hook?card_id={card_id}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// The ingest role gate reads the repo's seeded role and area caches.
fn hook_app_state(repo: &Arc<SqlxRepo>, events: EventBus) -> AppState {
    let write = WriteContext::new(
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
    );
    AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-claude-hooks"),
            Vec::new(),
            events,
            write,
        )),
        Arc::new(CodexClient::new_stub()),
        Some(repo.card_role_cache().clone()),
        Some(repo.track_area_cache().clone()),
    )
}

pub fn session_start(session_id: &str, cwd: &str, transcript_path: &Path) -> Value {
    json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "cwd": cwd,
        "transcript_path": transcript_path,
        "source": "startup"
    })
}

/// Spawns the capture source with production transcript resolution.
pub fn spawn_claude_source(
    repo: Arc<SqlxRepo>,
    runtime: WorkerSessionProjection,
    seed: &SeededRuntime,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), calm_types::error::CoreError>>,
) {
    let token = CancellationToken::new();
    let source = ClaudeTranscriptFlowSource::new_with_options(
        repo.clone(),
        runtime,
        wf::claude_card_cwd(seed),
        token.clone(),
        ClaudeTranscriptFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
        },
    );
    let session = wf::worker_session(seed);
    let sink = WorkerFlowSink::new(repo);
    let handle = tokio::spawn(async move { source.capture(&session, &sink).await });
    (token, handle)
}

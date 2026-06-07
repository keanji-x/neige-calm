#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCove};
use calm_server::operation::shared_daemon_spawn_adapter::SharedDaemonSpawnAdapter;
use calm_server::operation::terminal_adapter::TerminalAdapter;
use calm_server::operation::{OperationRuntime, SpawnCtx, SpawnHandle, SqlxOperationRepo};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use futures::future::BoxFuture;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    cove_id: String,
    spawn_count: Arc<AtomicUsize>,
    fail_spawn: Arc<AtomicBool>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "shared-wave".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
    let codex = Arc::new(CodexClient::new_stub());
    let mut state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        codex.clone(),
        None,
        None,
    );
    let pending = Arc::new(PendingThreadStartRegistry::new(
        repo.clone(),
        events.clone(),
    ));
    let shared =
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), Some(pending.clone()));
    state = state.with_shared_codex_appserver(shared);
    state = state.with_pending_codex_threads(pending);

    let spawn_count = Arc::new(AtomicUsize::new(0));
    let fail_spawn = Arc::new(AtomicBool::new(false));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let terminal_adapter = Arc::new(TerminalAdapter::new(
        route_repo.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
    ));
    let count_for_hook = spawn_count.clone();
    let fail_for_hook = fail_spawn.clone();
    let repo_for_hook = route_repo.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            let fail = fail_for_hook.clone();
            let repo = repo_for_hook.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    return Err(calm_server::error::CalmError::Internal(
                        "forced shared-daemon-spawn fixture spawn failure".into(),
                    ));
                }
                repo.terminal_set_pid(&terminal_id, Some(91_000)).await?;
                Ok(SpawnHandle {
                    renderer_id: terminal_id.clone(),
                    terminal_id,
                })
            })
        },
    );
    let shared_adapter = Arc::new(SharedDaemonSpawnAdapter::new_with_spawn_hook(
        route_repo.clone(),
        codex,
        state.shared_codex_appserver.clone(),
        state.pending_codex_threads.clone(),
        state.pending_codex_threads_spawn_serial.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        state.dispatcher.clone(),
        state.spec_push.clone(),
        state.aspects.clone(),
        hook,
    ));
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![terminal_adapter, shared_adapter],
        events.clone(),
        SpawnCtx::new(
            route_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events,
        ),
    ));
    state = state.with_operation_runtime(runtime);

    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        repo,
        cove_id: cove.id.to_string(),
        spawn_count,
        fail_spawn,
        _tmp: tmp,
    }
}

async fn post_wave(boot: &Boot, title: &str) -> (StatusCode, Value) {
    let body = json!({
        "cove_id": "ignored-by-path-wrapper",
        "title": title,
        "sort": null,
        "cwd": "/workspace",
        "attach_folder": true,
        "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] }
    });
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/coves/{}/waves", boot.cove_id))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

async fn spec_card_id(repo: &SqlxRepo, wave_id: &str) -> String {
    for card in repo.cards_by_wave(wave_id).await.unwrap() {
        if repo.card_role_get(card.id.as_str()).await.unwrap() == Some(CardRole::Spec) {
            return card.id.to_string();
        }
    }
    panic!("missing spec card for wave {wave_id}");
}

async fn runtime_status(repo: &SqlxRepo, card_id: &str) -> String {
    sqlx::query_scalar(
        "SELECT status FROM runtimes WHERE card_id = ?1 AND status != 'superseded' ORDER BY updated_at_ms DESC LIMIT 1",
    )
    .bind(card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

async fn only_operation(repo: &SqlxRepo) -> (String, String) {
    sqlx::query_as("SELECT kind, phase FROM operations ORDER BY created_at_ms DESC LIMIT 1")
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn post_wave_non_empty_title_routes_through_mint_and_await() {
    let boot = boot().await;
    let (status, wave) = post_wave(&boot, "build the thing").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let wave_id = wave["id"].as_str().unwrap();
    let spec_card_id = spec_card_id(&boot.repo, wave_id).await;

    assert_eq!(
        only_operation(&boot.repo).await,
        ("shared-daemon-spawn".into(), "succeeded".into())
    );
    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(&spec_card_id)
        .await
        .unwrap()
        .expect("thread row");
    assert_eq!(mapping.thread_id, "fake-thread-0001");
    assert!(boot.state.spec_push.contains(&wave_id.to_string().into()));
    assert_eq!(runtime_status(&boot.repo, &spec_card_id).await, "running");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_wave_empty_title_routes_through_register_pending() {
    let boot = boot().await;
    let (status, wave) = post_wave(&boot, "   ").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let wave_id = wave["id"].as_str().unwrap();
    let spec_card_id = spec_card_id(&boot.repo, wave_id).await;

    assert_eq!(
        only_operation(&boot.repo).await,
        ("shared-daemon-spawn".into(), "succeeded".into())
    );
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(&spec_card_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
    assert!(boot.state.spec_push.contains(&wave_id.to_string().into()));
    assert_eq!(
        runtime_status(&boot.repo, &spec_card_id).await,
        "turn_pending"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_wave_shared_daemon_spawn_failure_inert_wave_with_201() {
    let boot = boot().await;
    boot.fail_spawn.store(true, Ordering::SeqCst);
    let (status, wave) = post_wave(&boot, "   ").await;
    assert_eq!(status, StatusCode::CREATED, "body={wave:?}");
    let wave_id = wave["id"].as_str().unwrap();
    let spec_card_id = spec_card_id(&boot.repo, wave_id).await;
    let card = boot
        .repo
        .card_get(&spec_card_id)
        .await
        .unwrap()
        .expect("spec card stays alive");

    assert_eq!(
        only_operation(&boot.repo).await,
        ("shared-daemon-spawn".into(), "failed".into())
    );
    assert!(card.payload.get("codex_source").is_none());
    assert!(card.payload.get("appserver_sock").is_none());
    assert!(card.payload.get("push_watermark").is_none());
    assert!(!boot.state.spec_push.contains(&wave_id.to_string().into()));
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 0);
    assert_eq!(runtime_status(&boot.repo, &spec_card_id).await, "failed");
}

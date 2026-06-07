#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{NewCove, NewWave};
use calm_server::operation::codex_adapter::CodexAdapter;
use calm_server::operation::terminal_adapter::TerminalAdapter;
use calm_server::operation::{OperationRuntime, SpawnCtx, SpawnHandle, SqlxOperationRepo};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::runtime_lookup::project_runtime_into_card_payload;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::RendererConfig;
use futures::future::BoxFuture;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use tempfile::TempDir;
use tower::ServiceExt;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type TestSpawnHook = Arc<
    dyn Fn(
            String,
            String,
            String,
            Value,
        ) -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>>
        + Send
        + Sync,
>;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    wave_id: String,
    events: EventBus,
    spawn_count: Arc<AtomicUsize>,
    _tmp: TempDir,
}

async fn boot_success() -> Boot {
    boot_with_spawn_hook_factory(|_, _| success_spawn_hook()).await
}

async fn boot_with_spawn_hook_factory<F>(factory: F) -> Boot
where
    F: FnOnce(
        Arc<SqlxRepo>,
        Arc<calm_server::terminal_renderer::TerminalRendererRegistry>,
    ) -> (Arc<AtomicUsize>, TestSpawnHook),
{
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "codex-endpoint".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "codex-endpoint".into(),
            sort: None,
            cwd: "/workspace".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
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

    let (spawn_count, hook) = factory(repo.clone(), state.terminal_renderer.clone());
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let terminal_adapter = Arc::new(TerminalAdapter::new_with_spawn_hook(
        route_repo.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        silent_spawn_hook(),
    ));
    let codex_adapter = Arc::new(CodexAdapter::new_with_spawn_hook(
        route_repo.clone(),
        codex,
        state.shared_codex_appserver.clone(),
        state.pending_codex_threads.clone(),
        state.pending_codex_threads_spawn_serial.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        hook.clone(),
    ));
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![terminal_adapter, codex_adapter],
        events.clone(),
        SpawnCtx::new(
            route_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events.clone(),
        ),
    ));
    state = state.with_operation_runtime(runtime);

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        repo,
        wave_id: wave.id.to_string(),
        events,
        spawn_count,
        _tmp: tmp,
    }
}

fn success_spawn_hook() -> (Arc<AtomicUsize>, TestSpawnHook) {
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_hook = count.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(SpawnHandle::Terminal {
                    terminal_id: terminal_id.clone(),
                    renderer_id: terminal_id,
                })
            })
        },
    );
    (count, hook)
}

fn silent_spawn_hook() -> TestSpawnHook {
    Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            Box::pin(async move {
                Ok(SpawnHandle::Terminal {
                    terminal_id: terminal_id.clone(),
                    renderer_id: terminal_id,
                })
            })
        },
    )
}

fn failing_spawn_hook(
    repo: Arc<SqlxRepo>,
    renderer: Arc<calm_server::terminal_renderer::TerminalRendererRegistry>,
) -> (Arc<AtomicUsize>, TestSpawnHook) {
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_hook = count.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              program: String,
              cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let repo = repo.clone();
            let renderer = renderer.clone();
            let count = count_for_hook.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                repo.terminal_set_pid(&terminal_id, Some(99_999)).await?;
                renderer.insert_test_entry(RendererConfig {
                    terminal_id: terminal_id.clone(),
                    cols: 80,
                    rows: 24,
                    buffer_bytes: 1 << 20,
                    terminal_fg: (216, 219, 226),
                    terminal_bg: (15, 20, 24),
                    program,
                    args: Vec::new(),
                    envs: Vec::new(),
                    cwd,
                    supervisor_sock: PathBuf::from("/tmp/missing-calm-supervisor.sock"),
                });
                Err(calm_server::error::CalmError::Internal(
                    "forced codex spawn failure".into(),
                ))
            })
        },
    );
    (count, hook)
}

async fn post(
    app: axum::Router,
    wave_id: &str,
    body: Value,
    idempotency_key: Option<&str>,
    actor: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/waves/{wave_id}/codex-cards"))
        .header("content-type", "application/json");
    if let Some(key) = idempotency_key {
        req = req.header("Idempotency-Key", key);
    }
    if let Some(actor) = actor {
        req = req.header("X-Calm-Actor", actor);
    }
    let resp = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    response_json(resp).await
}

async fn post_terminal(
    app: axum::Router,
    wave_id: &str,
    body: Value,
    idempotency_key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/waves/{wave_id}/terminal-cards"))
        .header("content-type", "application/json");
    if let Some(key) = idempotency_key {
        req = req.header("Idempotency-Key", key);
    }
    let resp = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    response_json(resp).await
}

async fn response_json(resp: axum::response::Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn operation_key_is_new_id_shape(operation_key: &str) -> bool {
    operation_key.len() == 32 && operation_key.bytes().all(|b| b.is_ascii_hexdigit())
}

fn body(prompt: Option<&str>) -> Value {
    let mut body = json!({
        "cwd": "/workspace",
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });
    if let Some(prompt) = prompt {
        body["prompt"] = json!(prompt);
    }
    body
}

async fn latest_codex_operation_phase(repo: &SqlxRepo) -> (String, Value) {
    let row = sqlx::query(
        "SELECT phase, COALESCE(phase_detail_json, '{}') AS detail FROM operations WHERE kind = 'codex-create' ORDER BY created_at_ms DESC LIMIT 1",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    let phase: String = row.try_get("phase").unwrap();
    let detail_text: String = row.try_get("detail").unwrap();
    (phase, serde_json::from_str(&detail_text).unwrap())
}

#[tokio::test]
async fn post_codex_card_empty_prompt_succeeds_via_register_pending() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (status, card) = post(boot.app.clone(), &boot.wave_id, body(None), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_eq!(
        card["payload"]["codex_thread_status"],
        "pending_thread_start"
    );
    let (phase, _) = latest_codex_operation_phase(&boot.repo).await;
    assert_eq!(phase, "succeeded");
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_codex_card_with_prompt_succeeds_via_mint_and_await() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(Some("explain this")),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_eq!(card["payload"]["codex_thread_id"], "fake-thread-0001");
    let card_id = card["id"].as_str().unwrap();
    let row = sqlx::query("SELECT status, thread_id FROM runtimes WHERE card_id = ?1")
        .bind(card_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let status: String = row.try_get("status").unwrap();
    let thread_id: String = row.try_get("thread_id").unwrap();
    assert_eq!(status, "running");
    assert_eq!(thread_id, "fake-thread-0001");
    let (phase, _) = latest_codex_operation_phase(&boot.repo).await;
    assert_eq!(phase, "succeeded");
}

#[tokio::test]
async fn post_codex_card_idempotency_same_key_same_normalized_payload_reuses_op() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let mut first_body = body(None);
    first_body["cwd"] = json!("");
    let second_body = json!({
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });

    let (first_status, first_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        first_body,
        Some("codex-same-normalized"),
        None,
    )
    .await;
    let (second_status, second_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        second_body,
        Some("codex-same-normalized"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_card:?}");
    assert_eq!(second_status, StatusCode::CREATED, "body={second_card:?}");
    assert_eq!(first_card["id"], second_card["id"]);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_codex_card_idempotency_key_reused_by_terminal_operation_uses_fresh_operation_key() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let terminal_key = "codex-create:abc";
    let codex_key = "abc";

    let terminal_body = json!({
        "program": "/bin/sh",
        "cwd": "",
        "env": {},
        "sort": 1.0,
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });
    let (terminal_status, terminal_card) = post_terminal(
        boot.app.clone(),
        &boot.wave_id,
        terminal_body,
        Some(terminal_key),
    )
    .await;
    assert_eq!(
        terminal_status,
        StatusCode::CREATED,
        "body={terminal_card:?}"
    );

    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(None),
        Some(codex_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_ne!(terminal_card["id"], card["id"]);

    let rows = sqlx::query(
        "SELECT kind, operation_key, idempotency_key FROM operations WHERE idempotency_key IN (?1, ?2) ORDER BY kind",
    )
    .bind(codex_key)
    .bind(terminal_key)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap();
    let observed: Vec<(String, String, String)> = rows
        .into_iter()
        .map(|row| {
            (
                row.try_get("kind").unwrap(),
                row.try_get("operation_key").unwrap(),
                row.try_get("idempotency_key").unwrap(),
            )
        })
        .collect();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].0, "codex-create");
    assert_eq!(observed[0].2, codex_key);
    assert_eq!(observed[1].0, "terminal-create");
    assert_eq!(observed[1].2, terminal_key);
    assert!(operation_key_is_new_id_shape(&observed[0].1));
    assert!(operation_key_is_new_id_shape(&observed[1].1));
    assert_ne!(observed[0].1, observed[1].1);
    assert_ne!(observed[0].1, format!("codex-create:{codex_key}"));
    assert_ne!(observed[1].1, terminal_key);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_codex_card_idempotency_trims_cwd_and_prompt_for_hash_equivalence() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let first_body = json!({
        "cwd": "  /workspace  ",
        "prompt": "  explain this  ",
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });
    let second_body = json!({
        "cwd": "/workspace",
        "prompt": "explain this",
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });

    let (first_status, first_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        first_body,
        Some("codex-trimmed-normalized"),
        None,
    )
    .await;
    let (second_status, second_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        second_body,
        Some("codex-trimmed-normalized"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_card:?}");
    assert_eq!(second_status, StatusCode::CREATED, "body={second_card:?}");
    assert_eq!(first_card["id"], second_card["id"]);
    assert_eq!(first_card["payload"]["cwd"], "/workspace");
    assert_eq!(first_card["payload"]["prompt"], "explain this");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_codex_card_idempotency_same_key_different_payload_returns_409() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (first_status, first_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(None),
        Some("codex-different-payload"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_card:?}");
    let (second_status, second_body) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(Some("now prompted")),
        Some("codex-different-payload"),
        None,
    )
    .await;
    assert_eq!(second_status, StatusCode::CONFLICT, "body={second_body:?}");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_codex_card_empty_spawn_failure_reaps_pty_and_keeps_failed_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_with_spawn_hook_factory(failing_spawn_hook).await;
    let mut rx = boot.events.subscribe();

    let (status, response) = post(boot.app.clone(), &boot.wave_id, body(None), None, None).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={response:?}"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);

    let mut added: Vec<BroadcastEnvelope> = Vec::new();
    let mut deleted: Vec<BroadcastEnvelope> = Vec::new();
    let mut updated: Vec<BroadcastEnvelope> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        match tokio::time::timeout(deadline - now, rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::CardAdded(_) => added.push(env),
                Event::CardDeleted { .. } => deleted.push(env),
                Event::CardUpdated(_) => updated.push(env),
                _ => {}
            },
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert_eq!(added.len(), 1, "expected one optimistic CardAdded");
    assert!(
        !updated.is_empty(),
        "expected compensation to emit CardUpdated"
    );
    assert!(deleted.is_empty(), "codex failure UI must keep the card");
    assert!(updated.iter().any(|env| env.actor == ActorId::Kernel));
    let added_card = match &added[0].event {
        Event::CardAdded(card) => card,
        other => panic!("expected CardAdded, got {other:?}"),
    };
    let terminal_id = added_card.payload["terminal_id"].as_str().unwrap();
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 0);
    assert!(boot.state.terminal_renderer.get(terminal_id).is_none());
    assert!(boot.repo.terminal_get(terminal_id).await.unwrap().is_some());
    let mut failed_card = boot
        .repo
        .card_get(added_card.id.as_str())
        .await
        .unwrap()
        .expect("failed codex card remains visible");
    project_runtime_into_card_payload(boot.repo.as_ref(), &mut failed_card)
        .await
        .unwrap();
    assert_eq!(
        failed_card.payload["codex_thread_status"],
        "failed_to_spawn"
    );
    let (phase, detail) = latest_codex_operation_phase(&boot.repo).await;
    assert_eq!(phase, "failed");
    assert_eq!(detail["last_error_class"], "internal");
}

#[tokio::test]
async fn post_codex_card_prompt_spawn_failure_interrupts_turn_and_keeps_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_with_spawn_hook_factory(failing_spawn_hook).await;

    let (status, response) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(Some("interrupt me")),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={response:?}"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .active_turn_for_test("fake-thread-0001"),
        None,
        "spawn-failure compensation must interrupt the prompted active turn"
    );
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert!(
        cards[0].payload.get("codex_thread_id").is_none(),
        "prompted compensation must clear the payload thread id"
    );
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(cards[0].id.as_str())
            .await
            .unwrap()
            .is_none(),
        "prompted compensation must delete the card/thread mapping"
    );
    let row = sqlx::query("SELECT status FROM runtimes WHERE card_id = ?1")
        .bind(cards[0].id.as_str())
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let status: String = row.try_get("status").unwrap();
    assert_eq!(
        status, "failed",
        "prompted compensation must mark runtime failed"
    );
}

#[tokio::test]
async fn post_codex_card_validate_forbidden_returns_403_phase_failed() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (status, response) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(None),
        None,
        Some("ai:codex"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={response:?}");
    let (phase, detail) = latest_codex_operation_phase(&boot.repo).await;
    assert_eq!(phase, "failed");
    assert_eq!(detail["last_error_class"], "forbidden");
    assert!(
        boot.repo
            .cards_by_wave(&boot.wave_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn post_codex_card_invalid_idempotency_key_header_returns_400() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/waves/{}/codex-cards", boot.wave_id))
        .header("content-type", "application/json")
        .body(Body::from(body(None).to_string()))
        .unwrap();
    req.headers_mut().insert(
        "Idempotency-Key",
        HeaderValue::from_bytes(b"\xff").expect("non-ASCII header value"),
    );

    let resp = boot.app.clone().oneshot(req).await.unwrap();
    let (status, response) = response_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={response:?}");
}

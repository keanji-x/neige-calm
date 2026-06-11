//! Integration tests for `POST /api/waves/:wave_id/terminal-cards` —
//! the atomic terminal-card endpoint introduced in #13 PR2.
//!
//! Boots a real Axum router (in-memory `SqlxRepo`) + the actual
//! terminal renderer for the happy paths, and points
//! `DaemonClient::proc_supervisor_sock` at a non-existent socket for
//! the "spawn failure but row persisted" case.
//!
//! Test taxonomy:
//!   * `post_terminal_card_atomic_returns_card_with_linked_payload` — 201,
//!     response is a card with `kind == "terminal"` and
//!     `payload.terminal_id` matching the linked terminal row.
//!   * `post_terminal_card_atomic_emits_single_card_added_event` — exactly
//!     one `card.added` on the bus carrying the final payload; zero
//!     `card.updated`.
//!   * `post_terminal_card_atomic_returns_500_on_daemon_spawn_failure_and_rolls_back`
//!     — 500 to the client, and the operation compensation removes the rows.
//!   * `post_terminal_card_atomic_404_on_unknown_wave` — 404 + no leaked
//!     rows.
//!   * `post_terminal_card_same_idempotency_key_returns_same_card` — same
//!     key and payload returns 201 with the same card body.
//!   * `post_terminal_card_atomic_defaults_program_to_shell` — empty body
//!     stamps `$SHELL` (or `/bin/sh`) onto the terminal row.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
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
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, SpawnCtx, SpawnHandle, SqlxOperationRepo,
};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::RendererConfig;
use futures::future::BoxFuture;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use tempfile::TempDir;
use tower::ServiceExt;
struct Boot {
    app: axum::Router,
    state: AppState,
    wave_id: String,
    events: EventBus,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

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

fn strip_runtime(mut card: Value) -> Value {
    if let Some(obj) = card.as_object_mut() {
        obj.remove("runtime");
    }
    card
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "endpoint-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "endpoint-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let mut state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
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
    let state = install_success_spawn_runtime(state, repo.clone(), events.clone());

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        state,
        wave_id: wave.id.to_string(),
        events,
        repo,
        _tmp: tmp,
    }
}

fn install_success_spawn_runtime(
    state: AppState,
    repo: Arc<dyn Repo>,
    events: EventBus,
) -> AppState {
    install_spawn_runtime_with_hook(state, repo, events, silent_spawn_hook())
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

fn install_spawn_runtime_with_hook(
    state: AppState,
    repo: Arc<dyn Repo>,
    events: EventBus,
    hook: TestSpawnHook,
) -> AppState {
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(
        repo.sqlite_pool()
            .expect("terminal endpoint tests require sqlite repo"),
    ));
    let terminal_adapter = Arc::new(TerminalAdapter::new_with_spawn_hook(
        route_repo.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        hook,
    ));
    let codex_adapter = Arc::new(CodexAdapter::new_with_spawn_hook(
        route_repo.clone(),
        state.codex.clone(),
        state.shared_codex_appserver.clone(),
        state.pending_codex_threads.clone(),
        state.pending_codex_threads_spawn_serial.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        silent_spawn_hook(),
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![terminal_adapter, codex_adapter],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events,
            completion,
        ),
    ));
    state.with_operation_runtime(runtime)
}

async fn boot_happy() -> Boot {
    boot().await
}

async fn boot_with_bad_supervisor(bad_sock: PathBuf) -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "endpoint-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "endpoint-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: Some(bad_sock),
    });
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        state,
        wave_id: wave.id.to_string(),
        events,
        repo,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    post_with_idempotency(app, uri, body, None).await
}

async fn post_with_idempotency(
    app: axum::Router,
    uri: String,
    body: Value,
    idempotency_key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = idempotency_key {
        req = req.header("Idempotency-Key", key);
    }
    let resp = app
        .oneshot(
            req.body(Body::from(body.to_string()))
                .expect("build terminal-card POST request"),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn post_codex_card(
    app: axum::Router,
    wave_id: &str,
    body: Value,
    idempotency_key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/waves/{wave_id}/codex-cards"))
        .header("content-type", "application/json");
    if let Some(key) = idempotency_key {
        req = req.header("Idempotency-Key", key);
    }
    let resp = app
        .oneshot(
            req.body(Body::from(body.to_string()))
                .expect("build codex-card POST request"),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn operation_key_is_new_id_shape(operation_key: &str) -> bool {
    operation_key.len() == 32 && operation_key.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn post_with_actor(
    app: axum::Router,
    uri: String,
    body: Value,
    actor: &str,
) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("X-Calm-Actor", actor)
                .body(Body::from(body.to_string()))
                .expect("build terminal-card POST request"),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn delete(app: axum::Router, uri: String) -> StatusCode {
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .expect("build DELETE request"),
        )
        .await
        .unwrap();
    resp.status()
}

#[tokio::test]
async fn post_terminal_card_same_idempotency_key_returns_same_card() {
    let boot = boot_happy().await;
    let body = json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let uri = format!("/api/waves/{}/terminal-cards", boot.wave_id);

    let (first_status, first_card) = post_with_idempotency(
        boot.app.clone(),
        uri.clone(),
        body.clone(),
        Some("terminal-route-retry-key"),
    )
    .await;
    let (second_status, second_card) = post_with_idempotency(
        boot.app.clone(),
        uri,
        body,
        Some("terminal-route-retry-key"),
    )
    .await;

    assert_eq!(first_status, StatusCode::CREATED, "body={first_card:?}");
    assert_eq!(second_status, StatusCode::CREATED, "body={second_card:?}");
    assert_eq!(first_card, second_card);
}

#[tokio::test]
async fn post_terminal_card_idempotency_key_reused_by_codex_operation_uses_fresh_operation_key() {
    let boot = boot_happy().await;
    let codex_key = "abc";
    let terminal_key = "codex-create:abc";

    let codex_body = json!({
        "cwd": "",
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });
    let (codex_status, codex_card) =
        post_codex_card(boot.app.clone(), &boot.wave_id, codex_body, Some(codex_key)).await;
    assert_eq!(codex_status, StatusCode::CREATED, "body={codex_card:?}");

    let terminal_body = json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let terminal_uri = format!("/api/waves/{}/terminal-cards", boot.wave_id);
    let (terminal_status, terminal_card) = post_with_idempotency(
        boot.app.clone(),
        terminal_uri,
        terminal_body,
        Some(terminal_key),
    )
    .await;
    assert_eq!(
        terminal_status,
        StatusCode::CREATED,
        "body={terminal_card:?}"
    );
    assert_ne!(codex_card["id"], terminal_card["id"]);

    let pool = boot
        .repo
        .sqlite_pool()
        .expect("terminal endpoint tests require sqlite repo");
    let rows = sqlx::query(
        "SELECT kind, operation_key, idempotency_key FROM operations WHERE idempotency_key IN (?1, ?2) ORDER BY kind",
    )
    .bind(codex_key)
    .bind(terminal_key)
    .fetch_all(&pool)
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
    assert_ne!(observed[0].1, terminal_key);
    assert_ne!(observed[1].1, terminal_key);
}

#[tokio::test]
async fn post_terminal_card_rejects_malformed_idempotency_key() {
    let boot = boot_happy().await;
    let body = json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let uri = format!("/api/waves/{}/terminal-cards", boot.wave_id);
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("build terminal-card POST request");
    req.headers_mut().insert(
        "Idempotency-Key",
        HeaderValue::from_bytes(b"\xff").expect("build non-ASCII header value"),
    );

    let resp = boot.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={json:?}");
}

#[tokio::test]
async fn post_terminal_card_idempotency_retry_skips_validation_after_wave_delete() {
    let boot = boot_happy().await;
    let body = json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let uri = format!("/api/waves/{}/terminal-cards", boot.wave_id);

    let (first_status, first_card) = post_with_idempotency(
        boot.app.clone(),
        uri.clone(),
        body.clone(),
        Some("terminal-route-retry-after-wave-delete"),
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_card:?}");

    let delete_status = delete(boot.app.clone(), format!("/api/waves/{}", boot.wave_id)).await;
    assert_eq!(delete_status, StatusCode::NO_CONTENT);

    let (retry_status, retry_card) = post_with_idempotency(
        boot.app.clone(),
        uri,
        body,
        Some("terminal-route-retry-after-wave-delete"),
    )
    .await;
    assert_eq!(
        retry_status,
        StatusCode::CREATED,
        "retry must return the stored operation instead of revalidating the deleted wave: {retry_card:?}"
    );
    assert_eq!(
        strip_runtime(retry_card.clone()),
        strip_runtime(first_card.clone())
    );
}

#[tokio::test]
async fn post_terminal_card_prepare_forbidden_returns_403_and_marks_failed() {
    let boot = boot_happy().await;
    let body = json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let uri = format!("/api/waves/{}/terminal-cards", boot.wave_id);

    let (status, response) = post_with_actor(boot.app.clone(), uri, body, "ai:codex").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "prepare-time role errors must surface as 403: {response:?}"
    );

    let pool = boot
        .repo
        .sqlite_pool()
        .expect("terminal endpoint tests require sqlite repo");
    let row = sqlx::query(
        "SELECT phase, phase_detail_json FROM operations ORDER BY created_at_ms DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let phase: String = row.try_get("phase").unwrap();
    assert_eq!(phase, "failed");
    let detail_text: String = row.try_get("phase_detail_json").unwrap();
    let detail: Value = serde_json::from_str(&detail_text).unwrap();
    assert_eq!(detail["last_error_class"], "forbidden");

    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert!(
        cards.is_empty(),
        "prepare-time Forbidden must roll back the opened transaction"
    );
}

async fn get(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn post_terminal_card_atomic_returns_card_with_linked_payload() {
    let boot = boot_happy().await;

    let (status, card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_eq!(card["kind"], "terminal", "card kind: {card:?}");
    let terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("payload.terminal_id is a string")
        .to_string();
    assert!(
        !terminal_id.is_empty(),
        "payload.terminal_id non-empty: {card:?}"
    );
    // payload schemaVersion is stamped by the kernel-side helper.
    assert!(
        card["payload"]["schemaVersion"].is_number(),
        "payload.schemaVersion present: {card:?}"
    );

    // The linked terminal row is also visible via the GET helper that
    // `useTodayTerminal` uses. Same id round-trip.
    let card_id = card["id"].as_str().unwrap();
    let (gstatus, term) = get(boot.app.clone(), format!("/api/cards/{card_id}/terminal")).await;
    assert_eq!(gstatus, StatusCode::OK, "GET terminal: {term:?}");
    assert_eq!(term["id"], terminal_id, "terminal id mismatch: {term:?}");
}

#[tokio::test]
async fn post_terminal_card_atomic_emits_single_card_added_event() {
    let boot = boot_happy().await;
    let mut rx = boot.events.subscribe();

    let (status, _card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Drain the bus over a short window. We expect EXACTLY ONE card.added
    // (carrying the fully-stamped payload) and ZERO card.updated frames.
    // The old 3-step recipe used to emit one card.added (payload=null)
    // followed by one card.updated (payload={terminal_id}); the atomic
    // endpoint collapses both into a single broadcast.
    let mut added: Vec<BroadcastEnvelope> = Vec::new();
    let mut updated_count = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::CardAdded(_) => added.push(env),
                Event::CardUpdated(_) => updated_count += 1,
                _ => {}
            },
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        added.len(),
        1,
        "exactly one card.added; got {} added + {} updated",
        added.len(),
        updated_count
    );
    assert_eq!(
        updated_count, 0,
        "no card.updated allowed; got {updated_count}"
    );
    let env = added.into_iter().next().unwrap();
    match env.event {
        Event::CardAdded(card) => {
            assert_eq!(card.kind, "terminal");
            assert!(
                card.payload
                    .get("terminal_id")
                    .and_then(|v| v.as_str())
                    .is_some(),
                "card.added event payload must carry terminal_id: {:?}",
                card.payload
            );
        }
        other => panic!("expected CardAdded, got {other:?}"),
    }
}

#[tokio::test]
async fn post_terminal_card_atomic_returns_500_on_daemon_spawn_failure_and_rolls_back() {
    // Point the renderer at a supervisor socket that definitely doesn't exist.
    // The handler must:
    //   (a) propagate the 500 to the caller, AND
    //   (b) roll back the card+terminal rows through operation compensation.
    let bad_sock = std::env::temp_dir().join("definitely-not-a-real-proc-supervisor.sock");
    let _ = std::fs::remove_file(&bad_sock);
    let boot = boot_with_bad_supervisor(bad_sock).await;
    let mut rx = boot.events.subscribe();

    let (status, body) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh", "cwd": "", "env": {}, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "expected 500 on daemon spawn failure: {body:?}"
    );

    let mut added: Vec<BroadcastEnvelope> = Vec::new();
    let mut deleted: Vec<BroadcastEnvelope> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::CardAdded(_) => added.push(env),
                Event::CardDeleted { .. } => deleted.push(env),
                _ => {}
            },
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        added.len(),
        1,
        "spawn-failure rollback must still expose the optimistic card.added exactly once"
    );
    assert_eq!(
        deleted.len(),
        1,
        "spawn-failure rollback must emit exactly one matching card.deleted"
    );
    let added_card = match &added[0].event {
        Event::CardAdded(card) => card,
        other => panic!("expected CardAdded, got {other:?}"),
    };
    match &deleted[0].event {
        Event::CardDeleted { id, wave_id } => {
            assert_eq!(id, &added_card.id);
            assert_eq!(wave_id.as_str(), boot.wave_id.as_str());
        }
        other => panic!("expected CardDeleted, got {other:?}"),
    }

    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert_eq!(
        cards.len(),
        0,
        "operation compensation must roll back the failed card; got {}",
        cards.len()
    );
}

#[tokio::test]
async fn post_terminal_card_spawn_failure_reaps_renderer_before_rollback() {
    let base = boot().await;
    let renderer = base.state.terminal_renderer.clone();
    let repo_for_hook = base.repo.clone();
    let supervisor_sock = base
        .state
        .daemon
        .data_dir
        .join("missing-proc-supervisor.sock");
    let hook = Arc::new(
        move |terminal_id: String,
              program: String,
              cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let renderer = renderer.clone();
            let repo = repo_for_hook.clone();
            let supervisor_sock = supervisor_sock.clone();
            Box::pin(async move {
                repo.terminal_set_pid(&terminal_id, Some(999_999_999))
                    .await?;
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
                    supervisor_sock,
                });
                Err(calm_server::error::CalmError::Internal(
                    "injected spawn failure after renderer entry".into(),
                ))
            })
        },
    );
    let Boot {
        state,
        wave_id,
        events,
        repo,
        _tmp,
        ..
    } = base;
    let state = install_spawn_runtime_with_hook(state, repo.clone(), events.clone(), hook);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    let boot = Boot {
        app,
        state,
        wave_id,
        events,
        repo,
        _tmp,
    };
    let mut rx = boot.events.subscribe();

    let (status, body) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh", "cwd": "/tmp", "env": {}, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "expected injected spawn failure: {body:?}"
    );

    let mut added: Vec<BroadcastEnvelope> = Vec::new();
    let mut deleted: Vec<BroadcastEnvelope> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::CardAdded(_) => added.push(env),
                Event::CardDeleted { .. } => deleted.push(env),
                _ => {}
            },
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert_eq!(added.len(), 1, "expected one optimistic card.added");
    assert_eq!(deleted.len(), 1, "expected one rollback card.deleted");
    assert_eq!(
        deleted[0].actor,
        ActorId::Kernel,
        "compensation rollback must be audited as kernel"
    );

    let added_card = match &added[0].event {
        Event::CardAdded(card) => card,
        other => panic!("expected CardAdded, got {other:?}"),
    };
    let terminal_id = added_card
        .payload
        .get("terminal_id")
        .and_then(Value::as_str)
        .expect("card payload has terminal_id")
        .to_string();
    match &deleted[0].event {
        Event::CardDeleted { id, wave_id } => {
            assert_eq!(id, &added_card.id);
            assert_eq!(wave_id.as_str(), boot.wave_id.as_str());
        }
        other => panic!("expected CardDeleted, got {other:?}"),
    }
    assert!(
        boot.state.terminal_renderer.get(&terminal_id).is_none(),
        "rollback must reap the live renderer entry before deleting the terminal row"
    );
    assert!(
        boot.repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_none(),
        "terminal row must be removed by rollback"
    );
    assert!(
        boot.repo
            .card_get(added_card.id.as_str())
            .await
            .unwrap()
            .is_none(),
        "card row must be removed by rollback"
    );
}

#[tokio::test]
async fn post_terminal_card_atomic_404_on_unknown_wave() {
    // No daemon spawn happens on the 404 path, so the stub binary is fine.
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves/wave_does_not_exist/terminal-cards".to_string(),
        json!({ "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body:?}");

    // No card and no terminal row leaked under the original wave either.
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert!(cards.is_empty(), "no rows must leak on 404: {cards:?}");
}

#[tokio::test]
async fn post_terminal_card_atomic_defaults_program_to_shell() {
    let boot = boot_happy().await;
    let (status, card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        // Only required field (#177): theme. Every other field falls
        // back to its default (program → $SHELL, cwd → $HOME, env → {}).
        json!({ "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    let terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("payload.terminal_id is a string");
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row exists after create");
    // `$SHELL` → falls back to `/bin/sh`. We accept either form so the test
    // is robust across host envs (CI typically has no $SHELL exported).
    let expected = std::env::var("SHELL").unwrap_or_default();
    if expected.is_empty() {
        assert_eq!(
            term.program, "/bin/sh",
            "default program: {:?}",
            term.program
        );
    } else {
        assert_eq!(
            term.program, expected,
            "default program should match $SHELL: {:?}",
            term.program
        );
    }
}

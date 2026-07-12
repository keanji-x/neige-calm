#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewCove, NewTerminal, NewWave, new_id, now_ms};
use calm_server::operation::claude_adapter::ClaudeAdapter;
use calm_server::operation::claude_restart_adapter::ClaudeRestartAdapter;
use calm_server::operation::codex_adapter::CodexAdapter;
use calm_server::operation::terminal_adapter::TerminalAdapter;
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, SpawnCtx, SpawnHandle, SqlxOperationRepo,
};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
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

#[derive(Clone, Debug)]
struct SpawnCall {
    terminal_id: String,
    program: String,
    cwd: String,
    env: Value,
}

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
            name: "claude-endpoint".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "claude-endpoint".into(),
            sort: None,
            cwd: "/workspace".into(),
            workflow_id: None,
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
    let mut codex = CodexClient::new_stub();
    codex.claude_bin = "/bin/true".into();
    codex.ingest_url = "http://127.0.0.1:4040".into();
    let codex = Arc::new(codex);
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
        codex.clone(),
        state.shared_codex_appserver.clone(),
        state.pending_codex_threads.clone(),
        state.pending_codex_threads_spawn_serial.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        silent_spawn_hook(),
    ));
    let claude_adapter = Arc::new(ClaudeAdapter::new_with_spawn_hook(
        route_repo.clone(),
        codex.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        hook.clone(),
    ));
    let claude_restart_adapter = Arc::new(ClaudeRestartAdapter::new_with_spawn_hook(
        route_repo.clone(),
        codex,
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        hook,
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![
            terminal_adapter,
            codex_adapter,
            claude_adapter,
            claude_restart_adapter,
        ],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events.clone(),
            completion,
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
                    renderer_id: terminal_id.clone(),
                    terminal_id,
                })
            })
        },
    );
    (count, hook)
}

fn recording_spawn_hook(
    calls: Arc<tokio::sync::Mutex<Vec<SpawnCall>>>,
) -> (Arc<AtomicUsize>, TestSpawnHook) {
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_hook = count.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              program: String,
              cwd: String,
              env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            let calls = calls.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                calls.lock().await.push(SpawnCall {
                    terminal_id: terminal_id.clone(),
                    program,
                    cwd,
                    env,
                });
                Ok(SpawnHandle::Terminal {
                    renderer_id: terminal_id.clone(),
                    terminal_id,
                })
            })
        },
    );
    (count, hook)
}

fn renderer_entry_spawn_hook(
    renderer: Arc<calm_server::terminal_renderer::TerminalRendererRegistry>,
    saw_existing_entry: Arc<tokio::sync::Mutex<Vec<bool>>>,
) -> (Arc<AtomicUsize>, TestSpawnHook) {
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_hook = count.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              program: String,
              cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let renderer = renderer.clone();
            let saw_existing_entry = saw_existing_entry.clone();
            let count = count_for_hook.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                saw_existing_entry
                    .lock()
                    .await
                    .push(renderer.get(&terminal_id).is_some());
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
                Ok(SpawnHandle::Terminal {
                    renderer_id: terminal_id.clone(),
                    terminal_id,
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
                    renderer_id: terminal_id.clone(),
                    terminal_id,
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
                    "forced claude spawn failure".into(),
                ))
            })
        },
    );
    (count, hook)
}

fn restart_failing_spawn_hook() -> (Arc<AtomicUsize>, TestSpawnHook) {
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
                let call_index = count.fetch_add(1, Ordering::SeqCst);
                if call_index == 0 {
                    Ok(SpawnHandle::Terminal {
                        renderer_id: terminal_id.clone(),
                        terminal_id,
                    })
                } else {
                    Err(calm_server::error::CalmError::Internal(
                        "forced claude restart spawn failure".into(),
                    ))
                }
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
        .uri(format!("/api/waves/{wave_id}/claude-cards"))
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

async fn post_restart(app: axum::Router, card_id: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card_id}/claude/restart"))
                .body(Body::empty())
                .unwrap(),
        )
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

async fn latest_claude_operation_phase(repo: &SqlxRepo) -> (String, Value) {
    let row = sqlx::query(
        "SELECT phase, COALESCE(phase_detail_json, '{}') AS detail FROM operations WHERE kind = 'claude-create' ORDER BY created_at_ms DESC LIMIT 1",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    let phase: String = row.try_get("phase").unwrap();
    let detail_text: String = row.try_get("detail").unwrap();
    (phase, serde_json::from_str(&detail_text).unwrap())
}

async fn runtime_status(repo: &SqlxRepo, card_id: &str) -> String {
    let row = sqlx::query("SELECT state AS status FROM worker_sessions WHERE card_id = ?1")
        .bind(card_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    row.try_get("status").unwrap()
}

fn operation_key_is_new_id_shape(operation_key: &str) -> bool {
    operation_key.len() == 32 && operation_key.bytes().all(|b| b.is_ascii_hexdigit())
}

#[tokio::test]
async fn post_claude_card_no_prompt_succeeds_through_saga() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (status, card) = post(boot.app.clone(), &boot.wave_id, body(None), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_eq!(card["kind"], "claude");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    let (phase, _) = latest_claude_operation_phase(&boot.repo).await;
    assert_eq!(phase, "succeeded");
    let card_id = card["id"].as_str().unwrap();
    assert_eq!(runtime_status(&boot.repo, card_id).await, "running");
    let settings_path = card["payload"]["settings_path"].as_str().unwrap();
    assert!(Path::new(settings_path).exists());
    let settings_text = std::fs::read_to_string(settings_path).unwrap();
    assert!(settings_text.contains("--provider claude"));
    assert!(settings_text.contains("/internal/claude/hook"));
    assert!(settings_text.contains(card_id));
    assert!(!settings_text.contains("mcp_servers"));
    assert!(!settings_text.contains("mcpServers"));
    let mcp_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id = ?1")
            .bind(card_id)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        mcp_count.0, 0,
        "Claude worker cards must not mint MCP tokens"
    );
    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let term = boot.repo.terminal_get(terminal_id).await.unwrap().unwrap();
    assert_eq!(term.cwd, "/workspace");
    assert!(!term.program.contains(" -- '"));
}

#[tokio::test]
async fn post_claude_card_with_prompt_succeeds_through_saga() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(Some("--help")),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    let card_id = card["id"].as_str().unwrap();
    assert_eq!(runtime_status(&boot.repo, card_id).await, "running");
    let (phase, _) = latest_claude_operation_phase(&boot.repo).await;
    assert_eq!(phase, "succeeded");
    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let term = boot.repo.terminal_get(terminal_id).await.unwrap().unwrap();
    assert!(
        term.program.contains(" -- '--help'"),
        "prompt must be passed after argv separator: {}",
        term.program
    );
    assert_eq!(
        term.env["NEIGE_HOOK_PROVIDER"],
        Value::String("claude".into())
    );
    assert!(Path::new(card["payload"]["settings_path"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn post_claude_restart_after_exit_reuses_terminal_and_resumes_session() {
    let _guard = ENV_LOCK.lock().await;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let calls_for_factory = calls.clone();
    let boot =
        boot_with_spawn_hook_factory(move |_, _| recording_spawn_hook(calls_for_factory)).await;

    let (create_status, created) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(Some("first prompt")),
        None,
        None,
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap();
    let terminal_id = created["payload"]["terminal_id"].as_str().unwrap();
    let session_id = created["payload"]["claude_session_id"].as_str().unwrap();
    boot.repo
        .session_projection_complete_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();

    let (restart_status, restarted) = post_restart(boot.app.clone(), card_id).await;
    assert_eq!(restart_status, StatusCode::OK, "body={restarted:?}");
    assert_eq!(restarted["id"], created["id"]);
    assert_eq!(restarted["payload"]["terminal_id"], terminal_id);
    assert_eq!(restarted["payload"]["claude_session_id"], session_id);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 2);

    let calls = calls.lock().await.clone();
    let restart_call = calls.last().expect("restart spawn call recorded");
    assert_eq!(restart_call.terminal_id, terminal_id);
    assert_eq!(restart_call.cwd, "/workspace");
    assert_eq!(
        restart_call.env["NEIGE_CARD_ID"],
        Value::String(card_id.to_string())
    );
    assert!(
        restart_call
            .program
            .contains(&format!("--resume '{}'", session_id)),
        "restart program must resume existing session: {}",
        restart_call.program
    );
    assert!(
        restart_call.program.contains("--settings '"),
        "restart program must include settings path: {}",
        restart_call.program
    );
    assert!(!restart_call.program.contains("--session-id"));
    assert!(!restart_call.program.contains("--fork-session"));
    assert!(!restart_call.program.contains("first prompt"));

    let rows = sqlx::query(
        "SELECT state AS status, terminal_run_id, agent_session_id AS session_id FROM worker_sessions WHERE card_id = ?1 ORDER BY created_at_ms ASC, id ASC",
    )
    .bind(card_id)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    let first_status: String = rows[0].try_get("status").unwrap();
    let second_status: String = rows[1].try_get("status").unwrap();
    let first_terminal: String = rows[0].try_get("terminal_run_id").unwrap();
    let second_terminal: String = rows[1].try_get("terminal_run_id").unwrap();
    let first_session: String = rows[0].try_get("session_id").unwrap();
    let second_session: String = rows[1].try_get("session_id").unwrap();
    assert_eq!(first_status, "exited");
    assert_eq!(second_status, "running");
    assert_eq!(first_terminal, terminal_id);
    assert_eq!(second_terminal, terminal_id);
    assert_eq!(first_session, session_id);
    assert_eq!(second_session, session_id);
}

#[tokio::test]
async fn post_claude_restart_recreates_missing_terminal_row_and_resumes_session() {
    let _guard = ENV_LOCK.lock().await;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let calls_for_factory = calls.clone();
    let boot =
        boot_with_spawn_hook_factory(move |_, _| recording_spawn_hook(calls_for_factory)).await;

    let (create_status, created) =
        post(boot.app.clone(), &boot.wave_id, body(None), None, None).await;
    assert_eq!(create_status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap();
    let old_terminal_id = created["payload"]["terminal_id"].as_str().unwrap();
    let session_id = created["payload"]["claude_session_id"].as_str().unwrap();
    boot.repo
        .session_projection_complete_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    boot.repo.terminal_delete(old_terminal_id).await.unwrap();
    assert!(
        boot.repo
            .terminal_get_by_card(card_id)
            .await
            .unwrap()
            .is_none(),
        "test setup must match the post-upgrade missing-terminal state"
    );

    let (restart_status, restarted) = post_restart(boot.app.clone(), card_id).await;
    assert_eq!(restart_status, StatusCode::OK, "body={restarted:?}");
    assert_eq!(restarted["id"], created["id"]);
    assert_eq!(restarted["payload"]["claude_session_id"], session_id);
    let new_terminal_id = restarted["payload"]["terminal_id"].as_str().unwrap();
    assert_ne!(new_terminal_id, old_terminal_id);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 2);

    let recreated = boot
        .repo
        .terminal_get_by_card(card_id)
        .await
        .unwrap()
        .expect("restart must recreate missing terminal row");
    assert_eq!(recreated.id, new_terminal_id);
    assert_eq!(recreated.cwd, "/workspace");

    let calls = calls.lock().await.clone();
    let restart_call = calls.last().expect("restart spawn call recorded");
    assert_eq!(restart_call.terminal_id, new_terminal_id);
    assert_eq!(restart_call.cwd, "/workspace");
    assert!(
        restart_call
            .program
            .contains(&format!("--resume '{}'", session_id)),
        "restart program must resume existing session: {}",
        restart_call.program
    );

    let rows = sqlx::query(
        "SELECT state AS status, terminal_run_id, agent_session_id AS session_id FROM worker_sessions WHERE card_id = ?1 ORDER BY created_at_ms ASC, id ASC",
    )
    .bind(card_id)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    let first_status: String = rows[0].try_get("status").unwrap();
    let second_status: String = rows[1].try_get("status").unwrap();
    let first_terminal: Option<String> = rows[0].try_get("terminal_run_id").unwrap();
    let second_terminal: String = rows[1].try_get("terminal_run_id").unwrap();
    let second_session: String = rows[1].try_get("session_id").unwrap();
    assert_eq!(first_status, "exited");
    assert_eq!(second_status, "running");
    assert_eq!(first_terminal, None);
    assert_eq!(second_terminal, new_terminal_id);
    assert_eq!(second_session, session_id);
}

#[tokio::test]
async fn post_claude_restart_drops_stale_renderer_entry_before_respawn() {
    let _guard = ENV_LOCK.lock().await;
    let saw_existing_entry = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let saw_existing_entry_for_factory = saw_existing_entry.clone();
    let boot = boot_with_spawn_hook_factory(move |_, renderer| {
        renderer_entry_spawn_hook(renderer, saw_existing_entry_for_factory)
    })
    .await;

    let (create_status, created) =
        post(boot.app.clone(), &boot.wave_id, body(None), None, None).await;
    assert_eq!(create_status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap();
    let terminal_id = created["payload"]["terminal_id"].as_str().unwrap();
    let stale_entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("initial renderer entry seeded by spawn hook");
    boot.repo
        .session_projection_complete_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();

    let (restart_status, restarted) = post_restart(boot.app.clone(), card_id).await;
    assert_eq!(restart_status, StatusCode::OK, "body={restarted:?}");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 2);
    let observations = saw_existing_entry.lock().await.clone();
    assert_eq!(
        observations,
        vec![false, false],
        "restart spawn must not see the stale registry entry"
    );
    let current_entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("restart spawn seeded a fresh renderer entry");
    assert!(
        !Arc::ptr_eq(&stale_entry, &current_entry),
        "restart must replace the stale renderer entry"
    );
}

#[tokio::test]
async fn post_claude_restart_spawn_failure_restores_terminal_exit_and_marks_runtime_failed() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_with_spawn_hook_factory(|_, _| restart_failing_spawn_hook()).await;

    let (create_status, created) =
        post(boot.app.clone(), &boot.wave_id, body(None), None, None).await;
    assert_eq!(create_status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap();
    let terminal_id = created["payload"]["terminal_id"].as_str().unwrap();
    boot.repo
        .session_projection_complete_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    boot.repo
        .terminal_set_exit(terminal_id, Some(0), false)
        .await
        .unwrap();

    let (restart_status, response) = post_restart(boot.app.clone(), card_id).await;
    assert_eq!(
        restart_status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={response:?}"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 2);

    let term = boot.repo.terminal_get(terminal_id).await.unwrap().unwrap();
    assert_eq!(term.exit_code, Some(0));
    assert!(!term.signal_killed);

    let rows = sqlx::query(
        "SELECT state AS status, terminal_run_id FROM worker_sessions WHERE card_id = ?1 ORDER BY created_at_ms ASC, id ASC",
    )
    .bind(card_id)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    let first_status: String = rows[0].try_get("status").unwrap();
    let second_status: String = rows[1].try_get("status").unwrap();
    let first_terminal: String = rows[0].try_get("terminal_run_id").unwrap();
    let second_terminal: String = rows[1].try_get("terminal_run_id").unwrap();
    assert_eq!(first_status, "exited");
    assert_eq!(second_status, "failed");
    assert_eq!(first_terminal, terminal_id);
    assert_eq!(second_terminal, terminal_id);
}

#[tokio::test]
async fn post_claude_restart_returns_409_when_runtime_is_active() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (create_status, created) =
        post(boot.app.clone(), &boot.wave_id, body(None), None, None).await;
    assert_eq!(create_status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap();

    let (status, response) = post_restart(boot.app.clone(), card_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={response:?}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("kill or wait for child exit before restart"),
        "body={response:?}"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_claude_restart_returns_403_for_non_claude_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone().into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();

    let (status, response) = post_restart(boot.app.clone(), card.id.as_str()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={response:?}");
}

#[tokio::test]
async fn post_claude_restart_returns_403_without_resumable_session() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone().into(),
            title: None,
            kind: "claude".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "settings_path": "/tmp/missing-session-settings.json"
            }),
        })
        .await
        .unwrap();
    let term = boot
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "'/bin/true' --settings '/tmp/missing-session-settings.json'".into(),
            cwd: "/workspace".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: new_id(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status: WorkerSessionState::Exited,
            terminal_run_id: Some(term.id),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, response) = post_restart(boot.app.clone(), card.id.as_str()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={response:?}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("no resumable session id"),
        "body={response:?}"
    );
}

#[tokio::test]
async fn post_claude_restart_returns_404_for_unknown_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (status, response) = post_restart(boot.app.clone(), "missing-card").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={response:?}");
}

#[tokio::test]
async fn post_claude_card_idempotency_same_key_same_normalized_payload_reuses_op() {
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
        Some("claude-same-normalized"),
        None,
    )
    .await;
    let (second_status, second_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        second_body,
        Some("claude-same-normalized"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_card:?}");
    assert_eq!(second_status, StatusCode::CREATED, "body={second_card:?}");
    assert_eq!(first_card["id"], second_card["id"]);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_claude_card_idempotency_same_key_different_payload_returns_409() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;

    let (first_status, first_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(None),
        Some("claude-different-payload"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_card:?}");
    let (second_status, second_body) = post(
        boot.app.clone(),
        &boot.wave_id,
        body(Some("now prompted")),
        Some("claude-different-payload"),
        None,
    )
    .await;
    assert_eq!(second_status, StatusCode::CONFLICT, "body={second_body:?}");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn post_claude_card_idempotency_trims_cwd_and_prompt_for_hash_equivalence() {
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
        Some("claude-trimmed-normalized"),
        None,
    )
    .await;
    let (second_status, second_card) = post(
        boot.app.clone(),
        &boot.wave_id,
        second_body,
        Some("claude-trimmed-normalized"),
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
async fn post_claude_card_spawn_failure_reaps_pty_deletes_settings_dir_marks_runtime_failed_keeps_card()
 {
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
                _ => {}
            },
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert_eq!(added.len(), 1, "expected one CardAdded");
    assert!(deleted.is_empty(), "claude failure UI must keep the card");
    assert!(added.iter().all(|env| env.actor != ActorId::Kernel));

    let added_card = match &added[0].event {
        Event::CardAdded(card) => card,
        other => panic!("expected CardAdded, got {other:?}"),
    };
    let card_id = added_card.id.as_str();
    let terminal_id = added_card.payload["terminal_id"].as_str().unwrap();
    let settings_path = added_card.payload["settings_path"].as_str().unwrap();
    let settings_dir = Path::new(settings_path).parent().unwrap();
    assert!(boot.state.terminal_renderer.get(terminal_id).is_none());
    assert!(
        !settings_dir.exists(),
        "settings dir must be deleted by compensation: {}",
        settings_dir.display()
    );
    assert_eq!(runtime_status(&boot.repo, card_id).await, "failed");
    assert!(
        boot.repo.card_get(card_id).await.unwrap().is_some(),
        "failed claude card remains visible"
    );
    let (phase, detail) = latest_claude_operation_phase(&boot.repo).await;
    assert_eq!(phase, "failed");
    assert_eq!(detail["last_error_class"], "internal");
}

#[tokio::test]
async fn post_claude_card_validate_forbidden_returns_403_phase_failed() {
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
    let (phase, detail) = latest_claude_operation_phase(&boot.repo).await;
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
async fn post_claude_card_invalid_idempotency_key_header_returns_400() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/waves/{}/claude-cards", boot.wave_id))
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

#[tokio::test]
async fn post_claude_card_idempotency_key_reused_by_other_kind_uses_fresh_operation_key() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let key = "shared-user-key";
    let terminal_op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               phase, created_at_ms, updated_at_ms, completed_at_ms
           )
           VALUES (?1, ?2, 'terminal-create', ?3, 'terminal-hash',
                   'wave', ?4, ?5, '{}', 'succeeded', ?6, ?6, ?6)"#,
    )
    .bind(&terminal_op_id)
    .bind(key)
    .bind(key)
    .bind(&boot.wave_id)
    .bind(serde_json::to_string(&json!({ "type": "wave", "id": boot.wave_id })).unwrap())
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let (status, card) = post(boot.app.clone(), &boot.wave_id, body(None), Some(key), None).await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");

    let rows = sqlx::query(
        "SELECT kind, operation_key, idempotency_key FROM operations WHERE idempotency_key = ?1 ORDER BY kind",
    )
    .bind(key)
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
    assert_eq!(observed[0].0, "claude-create");
    assert_eq!(observed[0].2, key);
    assert_eq!(observed[1].0, "terminal-create");
    assert_eq!(observed[1].1, key);
    assert_eq!(observed[1].2, key);
    assert!(operation_key_is_new_id_shape(&observed[0].1));
    assert_ne!(observed[0].1, key);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

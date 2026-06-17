#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewTerminal, NewWave};
use calm_server::pending_codex_threads::{PendingEntry, PendingThreadStartRegistry};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::runtime_lookup::project_runtime_into_cards_payload;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use clap::Parser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

/// Serializes intra-binary tests that toggle `FAKE_CODEX_CAPTURE_REQUESTS`
/// (or any other process env read by the fake codex shim). Peer test
/// binaries keep their own `ENV_LOCK` because each test binary is a separate
/// process.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    wave_id: String,
    codex_homes_dir: PathBuf,
    _tmp: TempDir,
}

fn fake_codex_bin() -> &'static str {
    env!("CARGO_BIN_EXE_osc-probe-child")
}

fn cfg(root: &TempDir) -> Config {
    Config::parse_from([
        "calm-server",
        "--data-dir",
        root.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ])
}

async fn boot() -> Boot {
    boot_with_shared_daemon(true).await
}

async fn boot_with_shared_daemon(start_appserver: bool) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "prompt-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "prompt-shared".into(),
            sort: None,
            cwd: "/workspace".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: std::env::var_os("CALM_TEST_PROC_SUPERVISOR_SOCK").map(PathBuf::from),
    });
    let events = EventBus::new();
    let codex = Arc::new(CodexClient::new_stub());
    let codex_homes_dir = codex.codex_homes_dir.clone();
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
        codex,
        None,
        None,
    );

    let cfg = cfg(&tmp);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed_from(None).unwrap();
    let pending = Arc::new(PendingThreadStartRegistry::new(
        repo.clone(),
        events.clone(),
    ));
    let shared = SharedCodexAppServer::new_with_pending(
        &cfg,
        Arc::new(home),
        repo.clone(),
        Some(pending.clone()),
    );
    if start_appserver {
        shared.start_or_takeover().await.unwrap();
    }
    state = state.with_shared_codex_appserver(shared);
    state = state.with_pending_codex_threads(pending);

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
        codex_homes_dir,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, wave_id: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{wave_id}/codex-cards"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn wait_for_requests(path: &Path, min_count: usize) -> Vec<Value> {
    for _ in 0..50 {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let rows: Vec<Value> = raw
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            if rows.len() >= min_count {
                return rows;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {min_count} captured fake app-server requests");
}

fn request<'a>(rows: &'a [Value], method: &str) -> &'a Value {
    rows.iter()
        .find(|row| row.get("method").and_then(Value::as_str) == Some(method))
        .unwrap_or_else(|| panic!("missing {method} in captured requests: {rows:?}"))
}

fn has_interrupt(rows: &[Value], thread_id: &str, turn_id: &str) -> bool {
    rows.iter().any(|row| {
        row.get("method").and_then(Value::as_str) == Some("turn/interrupt")
            && row.pointer("/params/threadId").and_then(Value::as_str) == Some(thread_id)
            && row.pointer("/params/turnId").and_then(Value::as_str) == Some(turn_id)
    })
}

fn theme() -> Value {
    json!({"fg": [216,219,226], "bg": [15,20,24]})
}

async fn runtime_status_for_card(repo: &SqlxRepo, card_id: &str) -> String {
    sqlx::query_scalar(
        "SELECT state FROM worker_sessions WHERE card_id = ?1 ORDER BY updated_at_ms DESC LIMIT 1",
    )
    .bind(card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn create_prompt_card_calls_shared_daemon_thread_start() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot().await;

    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "explain this", "theme": theme() }),
    )
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");

    let rows = wait_for_requests(&capture_file, 3).await;
    let thread = request(&rows, "thread/start");
    assert_eq!(
        thread["params"]["cwd"], "/workspace",
        "thread/start cwd: {thread}"
    );
    assert_eq!(thread["params"]["approvalPolicy"], "never");
    assert_eq!(thread["params"]["sandbox"], "workspace-write");
    assert!(thread["params"].get("developerInstructions").is_none());

    let turn = request(&rows, "turn/start");
    assert_eq!(turn["params"]["threadId"], "fake-thread-0001");
    assert_eq!(
        turn["params"]["input"],
        json!([{ "type": "text", "text": "explain this" }])
    );
}

#[tokio::test]
async fn create_prompt_card_writes_runtime_and_projects_thread_id() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot().await;
    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "persist me", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");

    let card_id = card["id"].as_str().unwrap();
    assert_eq!(card["payload"]["codex_thread_id"], "fake-thread-0001");
    // Use projectable (broadened to include terminal-status rows) so the
    // assertion is robust to CI-only timing where the codex TUI fixture
    // exits quickly → attach_reader marks runtime Exited before this read.
    let runtime = boot
        .repo
        .runtime_get_projectable_for_card(&card_id.to_string())
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.kind, RuntimeKind::CodexCard);
    assert_eq!(runtime.thread_id.as_deref(), Some("fake-thread-0001"));
}

#[tokio::test]
async fn create_prompt_card_spawns_remote_resume_tui() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot().await;
    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "attach me", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("renderer entry");
    let shell_line = &entry.config().args[1];
    assert!(
        shell_line.contains("codex resume 'fake-thread-0001' --remote 'unix://"),
        "unexpected command line: {shell_line}"
    );
}

#[tokio::test]
async fn create_prompt_card_skips_per_card_codex_home_seeding() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot().await;
    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "no seed", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    let card_id = card["id"].as_str().unwrap();
    assert!(
        !boot.codex_homes_dir.join(card_id).exists(),
        "shared prompt path must not create a per-card CODEX_HOME"
    );
}

#[tokio::test]
async fn empty_path_errors_when_shared_daemon_not_running() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_with_shared_daemon(false).await;
    let (status, body) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body:?}");
    assert!(
        boot.repo
            .cards_by_wave(&boot.wave_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn create_empty_card_with_empty_cards_flag_enabled_uses_shared_daemon_pending_register() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot().await;
    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert!(card["payload"].get("codex_thread_id").is_none());
    assert_eq!(
        card["payload"]["codex_thread_status"],
        "pending_thread_start"
    );
    let card_id = card["id"].as_str().unwrap();
    assert!(
        !boot.codex_homes_dir.join(card_id).exists(),
        "shared empty-card path must not create a per-card CODEX_HOME"
    );
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("renderer entry");
    let shell_line = &entry.config().args[1];
    assert!(
        shell_line.contains("codex --remote 'unix://"),
        "unexpected command line: {shell_line}"
    );
    assert!(
        !shell_line.contains("resume"),
        "empty shared TUI must fresh-start, not resume: {shell_line}"
    );
}

#[tokio::test]
async fn empty_user_card_respawns_daemon_when_proxy_changed() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot().await;
    let old_pid = boot
        .state
        .shared_codex_appserver
        .status_snapshot()
        .runtime
        .expect("shared daemon runtime")
        .pid;
    boot.repo
        .settings_upsert(
            "http_proxy",
            "http://proxy-after-empty-user-card.local:3128",
        )
        .await
        .unwrap();
    boot.state.shared_codex_appserver.mark_needs_respawn();

    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");

    let snapshot = boot.state.shared_codex_appserver.status_snapshot();
    assert_eq!(snapshot.restart_count, 1);
    assert_ne!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "empty user-card path must respawn before the TUI uses the shared remote"
    );
    assert!(
        !boot
            .state
            .shared_codex_appserver
            .needs_respawn_on_next_thread_start_for_test()
    );
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
}

#[tokio::test]
async fn empty_user_card_respawn_failure_does_not_leave_card_stuck_pending() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_INITIALIZE", "1");
    }
    boot.state.shared_codex_appserver.mark_needs_respawn();

    let (status, body) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "theme": theme() }),
    )
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_INITIALIZE");
    }

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body:?}");
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 0);
    let mut failed_cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    project_runtime_into_cards_payload(boot.repo.as_ref(), &mut failed_cards)
        .await
        .unwrap();
    assert_eq!(failed_cards.len(), 1);
    assert_eq!(
        failed_cards[0].payload["codex_thread_status"], "failed_to_spawn",
        "runtime compensation must leave the failed card visible"
    );
}

#[tokio::test]
async fn prompt_card_thread_start_respawn_failure_marks_runtime_failed() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_INITIALIZE", "1");
    }
    boot.state.shared_codex_appserver.mark_needs_respawn();

    let (status, body) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "respawn then fail", "theme": theme() }),
    )
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_INITIALIZE");
    }

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body:?}");
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        runtime_status_for_card(&boot.repo, cards[0].id.as_str()).await,
        "failed"
    );
}

#[tokio::test]
async fn prompt_card_turn_start_failure_marks_runtime_failed() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_TURN_START", "1");
    }
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "turn should fail", "theme": theme() }),
    )
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_TURN_START");
    }

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body:?}");
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        runtime_status_for_card(&boot.repo, cards[0].id.as_str()).await,
        "failed"
    );
    assert!(cards[0].payload.get("codex_thread_id").is_none());
}

#[tokio::test]
async fn prompt_card_lifecycle_wait_failure_interrupts_and_rolls_back() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
        std::env::set_var("FAKE_CODEX_SKIP_TURN_STARTED", "1");
    }
    let boot = boot().await;

    let app = boot.app.clone();
    let wave_id = boot.wave_id.clone();
    let post_task = tokio::spawn(async move {
        post(
            app,
            &wave_id,
            json!({ "cwd": "/workspace", "prompt": "lifecycle should fail", "theme": theme() }),
        )
        .await
    });

    let rows = wait_for_requests(&capture_file, 3).await;
    assert!(
        rows.iter()
            .any(|row| row.get("method").and_then(Value::as_str) == Some("turn/start")),
        "lifecycle rollback test must reach turn/start before advancing time: {rows:?}"
    );

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();

    let (status, body) = post_task.await.unwrap();
    let rows = wait_for_requests(&capture_file, 4).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
        std::env::remove_var("FAKE_CODEX_SKIP_TURN_STARTED");
    }

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body:?}");
    assert!(
        has_interrupt(&rows, "fake-thread-0001", "fake-turn-0001"),
        "prompt lifecycle rollback must interrupt the in-flight shared turn: {rows:?}"
    );
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        runtime_status_for_card(&boot.repo, cards[0].id.as_str()).await,
        "failed"
    );
    assert!(cards[0].payload.get("codex_thread_id").is_none());
}

#[tokio::test]
async fn empty_card_spawn_failure_removes_pending_entry() {
    let _guard = ENV_LOCK.lock().await;
    let missing_sock = std::env::temp_dir().join(format!(
        "neige-calm-missing-supervisor-{}.sock",
        uuid::Uuid::new_v4()
    ));
    unsafe {
        std::env::set_var("CALM_TEST_PROC_SUPERVISOR_SOCK", &missing_sock);
    }
    let boot = boot().await;
    unsafe {
        std::env::remove_var("CALM_TEST_PROC_SUPERVISOR_SOCK");
    }

    let (status, failed) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={failed:?}");
    let pending = &boot.state.pending_codex_threads;
    assert_eq!(pending.pending_count().await, 0);
    let mut failed_cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    project_runtime_into_cards_payload(boot.repo.as_ref(), &mut failed_cards)
        .await
        .unwrap();
    assert_eq!(failed_cards.len(), 1);
    assert_eq!(
        failed_cards[0].payload["codex_thread_status"], "failed_to_spawn",
        "runtime compensation must leave the failed card visible"
    );

    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone().into(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let terminal = boot
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "codex".into(),
            cwd: "/".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card_id = card.id.to_string();
    let runtime_id = calm_server::model::new_id();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card_id.clone(),
            kind: RuntimeKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::TurnPending,
            terminal_run_id: Some(terminal.id.to_string()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    pending
        .register(PendingEntry::new(
            card_id.clone(),
            None,
            terminal.id.to_string(),
            runtime_id,
        ))
        .await
        .unwrap();
    assert!(
        boot.state
            .shared_codex_appserver
            .handle_thread_started_notification_for_test("T-new")
            .await
            .unwrap()
    );
    let runtime = boot
        .repo
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(runtime.thread_id.as_deref(), Some("T-new"));
}

#[tokio::test]
async fn create_prompt_card_errors_when_shared_daemon_not_running() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_with_shared_daemon(false).await;
    assert!(!boot.state.shared_codex_appserver.is_running());

    let (status, body) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "legacy degraded", "theme": theme() }),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body:?}");
    assert!(
        boot.repo
            .cards_by_wave(&boot.wave_id)
            .await
            .unwrap()
            .is_empty()
    );
}

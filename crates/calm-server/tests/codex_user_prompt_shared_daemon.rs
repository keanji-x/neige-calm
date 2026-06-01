#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use clap::Parser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

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

fn cfg(root: &TempDir, enabled: bool) -> Config {
    let mut cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        root.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ]);
    cfg.shared_codex_appserver_enabled = enabled;
    cfg
}

async fn boot(shared_enabled: bool) -> Boot {
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
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let codex = Arc::new(CodexClient::new_stub());
    let codex_homes_dir = codex.codex_homes_dir.clone();
    let mut state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        codex,
        None,
        None,
    );

    if shared_enabled {
        let cfg = cfg(&tmp, true);
        let home = calm_server::shared_codex_home::SharedCodexHome::new(
            cfg.data_dir_resolved().join("codex-home"),
            cfg.data_dir_resolved().join("codex-homes"),
        );
        home.seed().unwrap();
        let shared = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
        shared.start_or_takeover().await.unwrap();
        state = state.with_shared_codex_appserver(shared);
    }

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

fn theme() -> Value {
    json!({"fg": [216,219,226], "bg": [15,20,24]})
}

#[tokio::test]
async fn create_prompt_card_calls_shared_daemon_thread_start() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot(true).await;

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
async fn create_prompt_card_persists_thread_mapping_to_table_and_payload() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "persist me", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");

    let card_id = card["id"].as_str().unwrap();
    assert_eq!(card["payload"]["codex_thread_id"], "fake-thread-0001");
    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(card_id)
        .await
        .unwrap()
        .expect("mapping row");
    assert_eq!(mapping.thread_id, "fake-thread-0001");
    assert_eq!(mapping.wave_id.as_deref(), Some(boot.wave_id.as_str()));
}

#[tokio::test]
async fn create_prompt_card_spawns_remote_resume_tui() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
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
    let boot = boot(true).await;
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
async fn create_empty_codex_card_still_uses_legacy_path() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert!(card["payload"].get("codex_thread_id").is_none());
    let card_id = card["id"].as_str().unwrap();
    assert!(
        boot.codex_homes_dir.join(card_id).exists(),
        "empty prompt legacy path still seeds per-card CODEX_HOME"
    );
    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("renderer entry");
    assert_eq!(entry.config().args[1], "codex");
}

#[tokio::test]
async fn create_prompt_card_with_shared_daemon_disabled_falls_back_to_legacy() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(false).await;
    let (status, card) = post(
        boot.app.clone(),
        &boot.wave_id,
        json!({ "cwd": "/workspace", "prompt": "legacy prompt", "theme": theme() }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert!(card["payload"].get("codex_thread_id").is_none());
    let card_id = card["id"].as_str().unwrap();
    assert!(
        boot.codex_homes_dir
            .join(card_id)
            .join("config.toml")
            .exists()
    );
    assert!(
        boot.repo
            .card_codex_thread_get_by_card(card_id)
            .await
            .unwrap()
            .is_none()
    );
    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let entry = boot
        .state
        .terminal_renderer
        .get(terminal_id)
        .expect("renderer entry");
    assert_eq!(entry.config().args[1], "codex 'legacy prompt'");
}

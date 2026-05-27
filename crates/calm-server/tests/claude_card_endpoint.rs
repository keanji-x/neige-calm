//! Integration tests for `POST /api/waves/:wave_id/claude-cards`.
//! The route mirrors codex card creation but spawns a Claude worker with
//! generated hook settings and no MCP token/config.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

fn locate_recorder_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_argv-recorder-daemon"))
}

struct Boot {
    app: axum::Router,
    wave_id: String,
    repo: Arc<SqlxRepo>,
    role_cache: CardRoleCache,
    _tmp: TempDir,
}

async fn boot_happy() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "claude-endpoint-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "claude-endpoint-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: locate_recorder_bin(),
    });
    let mut codex = CodexClient::new_stub();
    codex.claude_bin = "/bin/true".into();
    codex.ingest_url = "http://127.0.0.1:4040".into();
    let role_cache = CardRoleCache::new();
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
            role_cache.clone(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(codex),
        Some(role_cache.clone()),
        None,
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        wave_id: wave.id.to_string(),
        repo,
        role_cache,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
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

#[tokio::test]
async fn post_claude_card_creates_worker_terminal_and_hook_settings_without_mcp() {
    let boot = boot_happy().await;

    let (status, card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/claude-cards", boot.wave_id),
        json!({
            "cwd": "/workspace",
            "prompt": "--help",
            "sort": 1.0,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_eq!(card["kind"], "claude");
    let card_id = card["id"].as_str().unwrap();
    assert_eq!(boot.role_cache.get(&card_id.into()), Some(CardRole::Worker));

    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let claude_session_id = card["payload"]["claude_session_id"].as_str().unwrap();
    assert_eq!(claude_session_id.len(), 36);
    assert!(
        uuid::Uuid::parse_str(claude_session_id).is_ok(),
        "claude_session_id must be a hyphenated UUID: {claude_session_id}"
    );
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row persists");
    assert_eq!(term.card_id.as_str(), card_id);
    assert!(term.program.contains("'/bin/true' --settings"));
    assert!(
        term.program
            .contains(&format!(" --session-id '{claude_session_id}'")),
        "first launch must assign Claude's durable session id: {}",
        term.program
    );
    assert!(
        term.program.contains(" -- '--help'"),
        "prompt must be protected by argv separator: {}",
        term.program
    );
    assert!(
        term.program
            .find(&format!("--session-id '{claude_session_id}'"))
            < term.program.find(" -- '--help'"),
        "--session-id must be before the prompt separator: {}",
        term.program
    );
    assert_eq!(term.cwd, "/workspace");
    assert!(
        term.env.get("ANTHROPIC_API_KEY").is_none(),
        "Claude subscription-auth path must not inject ANTHROPIC_API_KEY: {:?}",
        term.env
    );
    assert_eq!(term.env["NEIGE_CARD_ID"], card_id);
    assert_eq!(term.env["NEIGE_HOOK_PROVIDER"], "claude");

    let settings_path = card["payload"]["settings_path"].as_str().unwrap();
    let settings_text = std::fs::read_to_string(settings_path).unwrap();
    assert!(settings_text.contains("--provider claude"));
    assert!(settings_text.contains("/internal/claude/hook"));
    assert!(settings_text.contains(card_id));
    assert!(!settings_text.contains("mcp_servers"));
    assert!(!settings_text.contains("mcpServers"));

    let settings_json: Value = serde_json::from_str(&settings_text).unwrap();
    assert_eq!(
        settings_json["hooks"]["PreToolUse"][0]["matcher"],
        Value::String("*".into())
    );
    assert_eq!(
        settings_json["hooks"]["PermissionRequest"][0]["matcher"],
        Value::String("*".into())
    );
    assert!(settings_json["hooks"]["Stop"][0].get("matcher").is_none());
    assert!(
        settings_json["hooks"]["SessionEnd"][0]
            .get("matcher")
            .is_none()
    );
    // #364: the generated settings must register every hook the FSM projects,
    // including the ones that previously drifted out.
    for ev in [
        "SubagentStart",
        "SubagentStop",
        "TaskCreated",
        "TaskCompleted",
        "Elicitation",
    ] {
        assert!(
            settings_json["hooks"][ev][0].get("matcher").is_none(),
            "{ev} must be registered without a matcher"
        );
    }
    assert_eq!(
        settings_json["hooks"]["PermissionDenied"][0]["matcher"],
        Value::String("*".into()),
        "PermissionDenied is tool-name-scoped and mirrors PermissionRequest's matcher"
    );

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
}

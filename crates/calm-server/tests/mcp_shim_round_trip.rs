//! True E2E MCP round trip: real shim binary + real kernel MCP server.
//!
//! This exercises the production byte path that codex uses:
//! codex-style stdio frames -> `neige-mcp-stdio-shim` -> kernel UDS MCP
//! transport -> tool handler -> response -> shim stdout.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_codex_create_tx, runtime_bind_attribution_tx,
    runtime_get_active_for_card_tx, runtime_start_tx,
};
use calm_server::event::EventBus;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewWave, now_ms};
use calm_server::runtime_repo::{
    AgentProvider, RunStatus, RuntimeInit, RuntimeKind, ThreadAttribution,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::BufReader;
use tokio::process::Command;

fn shim_bin() -> std::path::PathBuf {
    // CARGO_BIN_EXE_<name> is only set for bins in the SAME package; this
    // test lives in calm-server but the shim bin lives in
    // crates/neige-mcp-stdio-shim. Resolve it via the workspace target dir:
    // cargo puts the bin next to the integration-test binary
    // (target/{debug,release}/neige-mcp-stdio-shim).
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // .../deps/
    p.pop(); // .../debug/ or .../release/
    p.push("neige-mcp-stdio-shim");
    assert!(
        p.exists(),
        "neige-mcp-stdio-shim not found at {p:?}; ensure `cargo test -p calm-server` triggers a workspace build of the shim crate"
    );
    p
}

struct Boot {
    server: Arc<McpServer>,
    thread_id: String,
    raw_token: String,
    socket_path: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for MCP socket");
    let socket_path = tmp.path().join("kernel.sock");

    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "mcp-shim-round-trip".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-shim-round-trip".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    let card_id = calm_server::model::new_id();

    let mut tx = sqlx_repo.pool().begin().await.unwrap();
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &calm_server::model::new_id(),
        wave.id.clone(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Spec,
        false,
        &card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint spec card");
    tx.commit().await.unwrap();
    let raw_token = mcp_token.expect("Spec card must mint a token");
    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(&sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    let events = EventBus::new();
    let registry = build_default_registry();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo,
        events,
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
        None,
    )
    .await
    .expect("spawn McpServer");

    Boot {
        server,
        thread_id,
        raw_token,
        socket_path,
        _tmp: tmp,
    }
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    if let Some(runtime) = runtime_get_active_for_card_tx(&mut tx, card_id)
        .await
        .unwrap()
    {
        runtime_bind_attribution_tx(
            &mut tx,
            &runtime.id,
            ThreadAttribution {
                runtime_id: runtime.id.clone(),
                provider: AgentProvider::Codex,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
            },
        )
        .await
        .unwrap();
    } else {
        runtime_start_tx(
            &mut tx,
            RuntimeInit {
                id: calm_server::model::new_id(),
                card_id: card_id.to_string(),
                kind: RuntimeKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: RunStatus::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                lease_owner: None,
                lease_until_ms: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn shim_round_trip_initialize_and_tools_call_completes() {
    let boot = boot().await;

    let mut child = Command::new(shim_bin())
        .env("NEIGE_MCP_SOCKET", &boot.socket_path)
        .env("NEIGE_MCP_TOKEN", &boot.raw_token)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "e2e-test-client", "version": "0.1" }
        }
    });
    write_frame(&mut stdin, &init).await;
    let resp1 = read_frame_timeout(&mut stdout, Duration::from_secs(5))
        .await
        .expect("initialize response within 5s");
    assert_eq!(resp1["id"], json!(1), "initialize id round-trips");
    assert!(
        resp1.get("error").is_none(),
        "initialize succeeded: {resp1:#?}"
    );

    let call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "calm.update_wave_state",
            "arguments": { "title": "renamed-by-e2e" },
            "_meta": { "threadId": boot.thread_id }
        }
    });
    write_frame(&mut stdin, &call).await;
    let resp2 = read_frame_timeout(&mut stdout, Duration::from_secs(5))
        .await
        .expect("tools/call response within 5s -- REGRESSION if this times out");
    assert_eq!(resp2["id"], json!(2), "tools/call id round-trips");
    assert!(
        resp2.get("error").is_none(),
        "tools/call succeeded: {resp2:#?}"
    );
    assert_eq!(resp2["result"]["isError"], json!(false));
    let structured = &resp2["result"]["structuredContent"];
    assert_eq!(structured["wave"]["title"], json!("renamed-by-e2e"));

    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = &boot.server;
}

async fn write_frame(w: &mut tokio::process::ChildStdin, frame: &Value) {
    use tokio::io::AsyncWriteExt;

    let mut bytes = serde_json::to_vec(frame).unwrap();
    bytes.push(b'\n');
    w.write_all(&bytes).await.expect("write frame");
    w.flush().await.expect("flush");
}

async fn read_frame_timeout(
    r: &mut BufReader<tokio::process::ChildStdout>,
    budget: Duration,
) -> Option<Value> {
    use tokio::io::AsyncBufReadExt;

    let mut line = String::new();
    tokio::time::timeout(budget, r.read_line(&mut line))
        .await
        .ok()?
        .ok()?;
    if line.is_empty() {
        return None;
    }
    Some(serde_json::from_str(line.trim_end()).expect("response is valid JSON"))
}

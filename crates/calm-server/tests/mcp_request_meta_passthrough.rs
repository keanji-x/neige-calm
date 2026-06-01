//! PR3a (#410) — MCP request `_meta` passthrough infrastructure.
//!
//! These tests drive the real kernel-as-MCP-server transport over UDS.
//! Handlers receive per-request `_meta`, but production handlers still
//! ignore it and continue using the handshake-bound `CardIdentity`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_codex_create_tx};
use calm_server::event::EventBus;
use calm_server::mcp_server::registry::{ToolDescriptor, ToolHandler, ToolHandlerFuture};
use calm_server::mcp_server::tools::wave_state::TOOL_GET_WAVE_STATE;
use calm_server::mcp_server::{McpServer, ToolRegistry, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewWave};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

const TEST_BUDGET: Duration = Duration::from_secs(5);

struct Boot {
    server: Arc<McpServer>,
    socket_path: PathBuf,
    raw_token: String,
    wave_id: String,
    _tmp: TempDir,
}

async fn boot_with_registry(registry: Arc<ToolRegistry>) -> Boot {
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
            name: "mcp-request-meta-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-request-meta-test".into(),
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
        card_id,
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

    let events = EventBus::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo,
        events,
        card_role_cache,
        wave_cove_cache,
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
    )
    .await
    .expect("spawn McpServer");

    Boot {
        server,
        socket_path,
        raw_token: mcp_token.expect("Spec card must mint a token"),
        wave_id: wave.id.as_str().to_string(),
        _tmp: tmp,
    }
}

fn meta_capture_registry() -> (Arc<ToolRegistry>, mpsc::UnboundedReceiver<Option<Value>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut registry = ToolRegistry::new();
    let handler: ToolHandler = Arc::new(
        move |_ctx, _identity, request_meta, _args| -> ToolHandlerFuture {
            let tx = tx.clone();
            Box::pin(async move {
                tx.send(request_meta)
                    .expect("meta capture receiver should still be alive");
                Ok(json!({ "status": "ok" }))
            })
        },
    );
    registry.register(
        ToolDescriptor {
            name: "test.echo_meta".into(),
            description: "Capture request metadata for PR3a tests.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        handler,
    );
    (Arc::new(registry), rx)
}

async fn connect(
    path: &std::path::Path,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(path).await.expect("connect UDS");
    let (rd, wr) = stream.into_split();
    (BufReader::new(rd), wr)
}

async fn send_frame(wr: &mut tokio::net::unix::OwnedWriteHalf, frame: Value) {
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    wr.write_all(&bytes).await.expect("write frame");
    wr.flush().await.expect("flush frame");
}

async fn recv_frame(rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    timeout(TEST_BUDGET, rd.read_line(&mut line))
        .await
        .expect("read response within budget")
        .expect("read_line ok");
    assert!(!line.is_empty(), "got empty/EOF response line");
    serde_json::from_str(line.trim_end()).expect("response is valid JSON")
}

fn initialize_frame(id: i64, token: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "mcp-test-client", "version": "0.1" },
            "_meta": {
                "dev.neige/auth": { "token": token }
            }
        }
    })
}

fn tools_call_frame(id: i64, name: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    })
}

async fn initialized_client(
    boot: &Boot,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    send_frame(&mut wr, initialize_frame(1, &boot.raw_token)).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "initialize errored: {resp:#?}");
    (rd, wr)
}

async fn recv_meta(rx: &mut mpsc::UnboundedReceiver<Option<Value>>) -> Option<Value> {
    timeout(TEST_BUDGET, rx.recv())
        .await
        .expect("handler should send metadata within budget")
        .expect("metadata channel should remain open")
}

#[tokio::test]
async fn meta_field_flows_from_request_to_handler() {
    let (registry, mut rx) = meta_capture_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    let mut params_meta = tools_call_frame(2, "test.echo_meta", json!({ "x": 1 }));
    params_meta["params"]["_meta"] = json!({ "threadId": "abc" });
    send_frame(&mut wr, params_meta).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    assert_eq!(recv_meta(&mut rx).await, Some(json!({ "threadId": "abc" })));

    let mut top_level_meta = tools_call_frame(3, "test.echo_meta", json!({ "x": 2 }));
    top_level_meta["_meta"] = json!({ "threadId": "top-level" });
    send_frame(&mut wr, top_level_meta).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    assert_eq!(
        recv_meta(&mut rx).await,
        Some(json!({ "threadId": "top-level" }))
    );

    let _ = &boot.server;
}

#[tokio::test]
async fn request_without_meta_yields_none() {
    let (registry, mut rx) = meta_capture_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.echo_meta", json!({ "x": 1 })),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    assert_eq!(recv_meta(&mut rx).await, None);

    let _ = &boot.server;
}

#[tokio::test]
async fn meta_with_non_object_yields_none() {
    let (registry, mut rx) = meta_capture_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    let mut frame = tools_call_frame(2, "test.echo_meta", json!({ "x": 1 }));
    frame["params"]["_meta"] = json!("not-an-object");
    send_frame(&mut wr, frame).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    assert_eq!(recv_meta(&mut rx).await, None);

    let _ = &boot.server;
}

#[tokio::test]
async fn existing_handlers_unchanged_when_meta_present() {
    let boot = boot_with_registry(build_default_registry()).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    let without_meta = tools_call_frame(2, TOOL_GET_WAVE_STATE, json!({}));
    send_frame(&mut wr, without_meta).await;
    let without_resp = recv_frame(&mut rd).await;
    assert!(
        without_resp.get("error").is_none(),
        "get_wave_state without meta errored: {without_resp:#?}"
    );

    let mut with_meta = tools_call_frame(3, TOOL_GET_WAVE_STATE, json!({}));
    with_meta["params"]["_meta"] = json!({
        "threadId": "ignored-by-pr3a",
        "card_id": "not-the-bound-card"
    });
    send_frame(&mut wr, with_meta).await;
    let with_resp = recv_frame(&mut rd).await;
    assert!(
        with_resp.get("error").is_none(),
        "get_wave_state with meta errored: {with_resp:#?}"
    );

    let without_result = &without_resp["result"]["structuredContent"];
    let with_result = &with_resp["result"]["structuredContent"];
    assert_eq!(without_result["wave"]["id"], json!(boot.wave_id));
    assert_eq!(with_result["wave"]["id"], json!(boot.wave_id));
    assert_eq!(without_result, with_result);

    let _ = &boot.server;
}

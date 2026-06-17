//! PR3a (#410) — MCP request `_meta` passthrough infrastructure.
//!
//! These tests drive the real kernel-as-MCP-server transport over UDS.
//! PR3b consumes per-request `_meta.threadId` to resolve identity before
//! invoking handlers.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_codex_create_tx, runtime_get_active_for_card_tx,
    session_bind_attribution_tx, session_start_runtime_tx,
};
use calm_server::event::EventBus;
use calm_server::mcp_server::auth;
use calm_server::mcp_server::registry::{
    ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture,
};
use calm_server::mcp_server::tools::wave_state::TOOL_WAVE_STATE;
use calm_server::mcp_server::{McpServer, ToolRegistry, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewWave, now_ms};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::runtime_repo::{
    AgentProvider, RuntimeInit, RuntimeKind, ThreadAttribution, WorkerSessionState,
};
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
    card_id: String,
    thread_id: String,
    _tmp: TempDir,
}

async fn boot_with_registry(registry: Arc<ToolRegistry>) -> Boot {
    boot_with_registry_options(registry, AuthMode::CardBound).await
}

async fn boot_with_registry_as_daemontrust(registry: Arc<ToolRegistry>) -> Boot {
    boot_with_registry_options(registry, AuthMode::DaemonTrust).await
}

#[derive(Clone, Copy)]
enum AuthMode {
    CardBound,
    DaemonTrust,
}

async fn boot_with_registry_options(registry: Arc<ToolRegistry>, auth_mode: AuthMode) -> Boot {
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
        card_id.clone(),
        &calm_server::model::new_id(),
        None,
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
    let card_token = mcp_token.expect("Spec card must mint a token");
    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(&sqlx_repo, card_id.as_str(), thread_id.as_str()).await;
    let daemon_token = "request-meta-daemon-token";
    let (raw_token, daemon_token_hash) = match auth_mode {
        AuthMode::CardBound => (card_token, None),
        AuthMode::DaemonTrust => (
            daemon_token.to_string(),
            Some(auth::hash_token(daemon_token)),
        ),
    };

    let events = EventBus::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo,
        events,
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
        daemon_token_hash,
        std::env::temp_dir().join("neige-test-gate-logs"),
    )
    .await
    .expect("spawn McpServer");

    Boot {
        server,
        socket_path,
        raw_token,
        wave_id: wave.id.as_str().to_string(),
        card_id,
        thread_id,
        _tmp: tmp,
    }
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    if let Some(runtime) = runtime_get_active_for_card_tx(&mut tx, card_id)
        .await
        .unwrap()
    {
        session_bind_attribution_tx(
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
        session_start_runtime_tx(
            &mut tx,
            RuntimeInit {
                id: calm_server::model::new_id(),
                card_id: card_id.to_string(),
                kind: RuntimeKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                lease_owner: None,
                lease_until_ms: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

fn identity_capture_registry() -> (Arc<ToolRegistry>, mpsc::UnboundedReceiver<ToolCallIdentity>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut registry = ToolRegistry::new();
    let handler: ToolHandler = Arc::new(move |_ctx, identity, _args| -> ToolHandlerFuture {
        let tx = tx.clone();
        Box::pin(async move {
            tx.send(identity)
                .expect("identity capture receiver should still be alive");
            Ok(json!({ "status": "ok" }))
        })
    });
    registry.register(
        ToolDescriptor {
            name: "test.echo_identity".into(),
            description: "Capture resolved identity for PR3b tests.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            annotations: None,
            visible_to_roles: &[CardRole::Spec],
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

async fn recv_identity(rx: &mut mpsc::UnboundedReceiver<ToolCallIdentity>) -> ToolCallIdentity {
    timeout(TEST_BUDGET, rx.recv())
        .await
        .expect("handler should send identity within budget")
        .expect("identity channel should remain open")
}

#[tokio::test]
async fn thread_id_flows_from_meta_to_identity() {
    let (registry, mut rx) = identity_capture_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    let mut params_meta = tools_call_frame(2, "test.echo_identity", json!({ "x": 1 }));
    params_meta["params"]["_meta"] = json!({ "threadId": boot.thread_id });
    send_frame(&mut wr, params_meta).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    let identity = recv_identity(&mut rx).await;
    assert_eq!(identity.card_id, boot.card_id);
    assert_eq!(identity.role, CardRole::Spec);
    assert_eq!(identity.wave_id.as_deref(), Some(boot.wave_id.as_str()));

    let mut top_level_meta = tools_call_frame(3, "test.echo_identity", json!({ "x": 2 }));
    top_level_meta["_meta"] = json!({ "threadId": boot.thread_id });
    send_frame(&mut wr, top_level_meta).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    assert_eq!(recv_identity(&mut rx).await.thread_id, boot.thread_id);

    let _ = &boot.server;
}

#[tokio::test]
async fn daemontrust_request_without_meta_rejects() {
    let (registry, mut rx) = identity_capture_registry();
    let boot = boot_with_registry_as_daemontrust(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.echo_identity", json!({ "x": 1 })),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["error"]["code"], json!(RpcError::INVALID_PARAMS));
    assert!(
        rx.try_recv().is_err(),
        "handler must not run without threadId"
    );

    let _ = &boot.server;
}

#[tokio::test]
async fn meta_with_non_object_rejects_before_handler() {
    let (registry, mut rx) = identity_capture_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    let mut frame = tools_call_frame(2, "test.echo_identity", json!({ "x": 1 }));
    frame["params"]["_meta"] = json!("not-an-object");
    send_frame(&mut wr, frame).await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["error"]["code"], json!(RpcError::INVALID_PARAMS));
    assert!(
        rx.try_recv().is_err(),
        "handler must not run without threadId"
    );

    let _ = &boot.server;
}

#[tokio::test]
async fn existing_handlers_unchanged_when_meta_present() {
    let boot = boot_with_registry(build_default_registry()).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    let without_meta = tools_call_frame(2, TOOL_WAVE_STATE, json!({}));
    send_frame(&mut wr, without_meta).await;
    let without_resp = recv_frame(&mut rd).await;
    assert!(
        without_resp.get("error").is_none(),
        "get_wave_state without meta should use CardBound identity: {without_resp:#?}"
    );
    assert_eq!(
        without_resp["result"]["structuredContent"]["wave"]["id"],
        json!(boot.wave_id)
    );

    let mut with_meta = tools_call_frame(3, TOOL_WAVE_STATE, json!({}));
    with_meta["params"]["_meta"] = json!({
        "threadId": boot.thread_id,
        "card_id": "not-the-bound-card"
    });
    send_frame(&mut wr, with_meta).await;
    let with_resp = recv_frame(&mut rd).await;
    assert!(
        with_resp.get("error").is_none(),
        "get_wave_state with meta errored: {with_resp:#?}"
    );

    let with_result = &with_resp["result"]["structuredContent"];
    assert_eq!(with_result["wave"]["id"], json!(boot.wave_id));

    let _ = &boot.server;
}

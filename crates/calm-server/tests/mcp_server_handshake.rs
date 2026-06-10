//! PR7a.1 (#136 followup) — integration tests for the
//! `mcp_server::handshake` path + per-connection identity binding.
//!
//! Boots a real `McpServer` against an in-memory `SqlxRepo` + a UDS
//! tempdir, mints a Spec card with a per-card MCP token, and drives a mock
//! client over the socket. Covers:
//!
//!   * `initialize` with a valid token → success + capabilities echoed.
//!   * `initialize` with a bogus token → `-32401` error + connection close.
//!   * `tools/call` before `initialize` → `-32002` error.
//!   * Multiple `tools/call`s on one connection → per-call
//!     `_meta.threadId` identity mapping routes to distinguishable threads.
//!
//! Test budget: 5 seconds per case (UDS bind/connect is sub-ms; the
//! budget exists only to bound runaway hangs).

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_codex_create_tx, runtime_bind_attribution_tx,
    runtime_get_active_for_card_tx, runtime_start_tx,
};
use calm_server::event::EventBus;
use calm_server::mcp_server::registry::{ToolCallIdentity, ToolHandler, ToolHandlerFuture};
use calm_server::mcp_server::{McpServer, ToolRegistry, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewWave, now_ms};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::runtime_repo::{
    AgentProvider, RunStatus, RuntimeInit, RuntimeKind, ThreadAttribution,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

const TEST_BUDGET: Duration = Duration::from_secs(5);

type IdentityCaptureRx = mpsc::UnboundedReceiver<ToolCallIdentity>;

struct Boot {
    server: Arc<McpServer>,
    sqlx_repo: Arc<SqlxRepo>,
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_id: String,
    wave_id: String,
    thread_id: String,
    /// Raw per-card MCP token (kept in memory only — never persisted).
    raw_token: String,
    socket_path: PathBuf,
    _tmp: TempDir,
}

/// Boot an `McpServer` against an in-memory SqlxRepo with a Spec card
/// plus an MCP token already minted. The card's wave + cove are seeded so
/// the emit tools (PR7a) can resolve the scope chain.
async fn boot() -> Boot {
    boot_with_registry(build_default_registry()).await
}

async fn boot_with_registry(registry: Arc<ToolRegistry>) -> Boot {
    let tmp = TempDir::new().expect("tempdir for MCP socket");
    let socket_path = tmp.path().join("kernel.sock");

    // Hold the concrete `SqlxRepo` separately so we can reach `pool()`
    // for the direct-tx card mint below; the `Arc<dyn Repo>` upcast
    // goes to the server.
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "mcp-handshake-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-handshake-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    let card_id = calm_server::model::new_id();

    // Mint Spec card + token inside a tx. We bypass the route layer
    // and write directly via `card_with_codex_create_tx` — the
    // route layer would also work, but this keeps the test focused
    // on the handshake / tools surface.
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
        // #229 PR A — spec cards are kernel-owned in production. The
        // mcp-handshake test focuses on the MCP surface, not on the
        // delete guard; minting `false` here also mirrors the prod
        // wave-create path (`routes/waves.rs`).
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
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"), // not used in handshake tests
        registry,
        None,
    )
    .await
    .expect("spawn McpServer");

    Boot {
        server,
        sqlx_repo,
        repo,
        events,
        card_id,
        wave_id: wave.id.to_string(),
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

/// Connect to the kernel-side socket. Returns a buffered reader paired
/// with the write half so the test can interleave read_line / write_all.
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

/// Send one JSON-RPC frame (object + trailing newline).
async fn send_frame(wr: &mut tokio::net::unix::OwnedWriteHalf, frame: Value) {
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    wr.write_all(&bytes).await.expect("write frame");
    wr.flush().await.expect("flush frame");
}

/// Read one JSON-RPC response frame.
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

fn initialize_frame_without_token(id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "mcp-test-client", "version": "0.1" },
            "_meta": {}
        }
    })
}

fn tools_call_frame(id: i64, name: &str, thread_id: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": args,
            "_meta": { "threadId": thread_id }
        }
    })
}

fn wave_json_from_cat_response(resp: &Value) -> Value {
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(structured["content_type"], json!("application/json"));
    let content = structured["content"].as_str().expect("wave content string");
    serde_json::from_str(content).expect("wave content is JSON")
}

fn registry_with_wave_cat_identity_capture() -> (Arc<ToolRegistry>, IdentityCaptureRx) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let wave_cat = registry
        .lookup("calm.wave.cat")
        .expect("calm.wave.cat registered");
    let descriptor = registry
        .descriptors()
        .into_iter()
        .find(|d| d.name == "calm.wave.cat")
        .expect("calm.wave.cat descriptor registered");
    let handler: ToolHandler = Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let tx = tx.clone();
        let wave_cat = wave_cat.clone();
        Box::pin(async move {
            tx.send(identity.clone()).unwrap();
            wave_cat(ctx, identity, args).await
        })
    });
    registry.register(descriptor, handler);
    (Arc::new(registry), rx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_with_valid_token_succeeds() {
    let b = boot().await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    send_frame(&mut wr, initialize_frame(1, &b.raw_token)).await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["id"], json!(1));
    assert!(resp.get("error").is_none(), "got error: {resp:#?}");
    let result = &resp["result"];
    assert_eq!(result["protocolVersion"], json!("2024-11-05"));
    assert!(
        result["capabilities"]["tools"].is_object(),
        "capabilities.tools should be an object; got: {result:#?}"
    );
    assert_eq!(result["serverInfo"]["name"], json!("neige-calm-kernel"));
    // Keep `b` and `_server` alive until the end.
    let _ = &b.server;
}

#[tokio::test]
async fn initialize_without_token_returns_invalid_params_and_closes() {
    let b = boot().await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    send_frame(&mut wr, initialize_frame_without_token(6)).await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["id"], json!(6));
    let err = resp.get("error").expect("must carry error object");
    assert_eq!(err["code"], json!(RpcError::INVALID_PARAMS));
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("per-card MCP token required"),
        "unexpected missing-token error: {err:#?}"
    );

    let mut line = String::new();
    let n = timeout(TEST_BUDGET, rd.read_line(&mut line))
        .await
        .expect("EOF within budget")
        .expect("read_line ok");
    assert_eq!(n, 0, "server must close after failed initialize");
    let _ = &b.server;
}

#[tokio::test]
async fn initialize_with_bad_token_returns_minus_32401_and_closes() {
    let b = boot().await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    let bogus = "0".repeat(64);
    send_frame(&mut wr, initialize_frame(7, &bogus)).await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["id"], json!(7));
    let err = resp.get("error").expect("must carry error object");
    assert_eq!(
        err["code"],
        json!(-32401),
        "TOKEN_NOT_RECOGNIZED_CODE = -32401; got {err:#?}"
    );

    // Server closes the connection on failed initialize — the next
    // read should hit EOF.
    let mut line = String::new();
    let n = timeout(TEST_BUDGET, rd.read_line(&mut line))
        .await
        .expect("EOF within budget")
        .expect("read_line ok");
    assert_eq!(n, 0, "server must close after failed initialize");
    let _ = &b.server;
}

#[tokio::test]
async fn tools_call_before_initialize_is_rejected() {
    let b = boot().await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    // Send a `tools/call` without `initialize` first — the transport
    // should refuse with -32002 ("not initialized").
    send_frame(
        &mut wr,
        tools_call_frame(
            3,
            "calm.task.complete",
            "pre-init-thread",
            json!({"idempotency_key": "x"}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["id"], json!(3));
    let err = resp.get("error").expect("must carry error object");
    assert_eq!(
        err["code"],
        json!(-32002),
        "pre-initialize traffic must be -32002; got {err:#?}"
    );
    let _ = &b.server;
}

// Per-call `_meta.threadId` routing is verified on one initialized card-bound
// connection by wrapping `calm.wave.cat` and capturing the identity delivered to
// the handler. Both calls resolve to the same card/wave, so the load-bearing
// assertion is that the observed `identity.thread_id` matches each request's
// `_meta.threadId`; falling back to the initialize/bound identity is caught.
#[tokio::test]
async fn two_tools_calls_route_per_call_meta_thread_id() {
    let (registry, mut identity_rx) = registry_with_wave_cat_identity_capture();
    let b = boot_with_registry(registry).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    send_frame(&mut wr, initialize_frame(1, &b.raw_token)).await;
    let init_resp = recv_frame(&mut rd).await;
    assert!(
        init_resp.get("error").is_none(),
        "init failed: {init_resp:#?}"
    );

    send_frame(
        &mut wr,
        tools_call_frame(
            10,
            "calm.wave.cat",
            &b.thread_id,
            json!({"path": "wave.json"}),
        ),
    )
    .await;
    let r1 = recv_frame(&mut rd).await;
    assert!(
        r1.get("error").is_none(),
        "first tools/call errored: {r1:#?}"
    );
    let wave1 = wave_json_from_cat_response(&r1);
    assert_eq!(
        wave1["id"],
        json!(b.wave_id.as_str()),
        "first wave.cat resolved wrong wave for bound card {}",
        b.card_id
    );
    let identity1 = timeout(TEST_BUDGET, identity_rx.recv())
        .await
        .expect("first captured identity within budget")
        .expect("first captured identity present");
    assert_eq!(identity1.card_id, b.card_id);
    assert_eq!(identity1.thread_id, b.thread_id);

    let thread_id_a2 = format!("thread-{}-second", b.card_id);
    seed_runtime_thread(&b.sqlx_repo, b.card_id.as_str(), thread_id_a2.as_str()).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            11,
            "calm.wave.cat",
            &thread_id_a2,
            json!({"path": "wave.json"}),
        ),
    )
    .await;
    let r2 = recv_frame(&mut rd).await;
    assert!(
        r2.get("error").is_none(),
        "second tools/call errored: {r2:#?}"
    );
    let wave2 = wave_json_from_cat_response(&r2);
    assert_eq!(
        wave2["id"],
        json!(b.wave_id.as_str()),
        "second wave.cat resolved wrong wave for bound card {}",
        b.card_id
    );
    let identity2 = timeout(TEST_BUDGET, identity_rx.recv())
        .await
        .expect("second captured identity within budget")
        .expect("second captured identity present");
    assert_eq!(identity2.card_id, b.card_id);
    assert_eq!(identity2.thread_id, thread_id_a2);

    match timeout(Duration::from_millis(50), rx.recv()).await {
        Err(_) => {}
        Ok(Ok(env)) => panic!("read-only wave.cat must not emit events; got {env:?}"),
        Ok(Err(_)) => panic!("bus closed unexpectedly"),
    }
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn spec_role_cannot_call_task_complete_or_fail() {
    let b = boot().await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    send_frame(&mut wr, initialize_frame(1, &b.raw_token)).await;
    let init_resp = recv_frame(&mut rd).await;
    assert!(
        init_resp.get("error").is_none(),
        "init failed: {init_resp:#?}"
    );

    send_frame(
        &mut wr,
        tools_call_frame(
            12,
            "calm.task.complete",
            &b.thread_id,
            json!({"idempotency_key": "tc-spec-refused", "result": "ok"}),
        ),
    )
    .await;
    let completed = recv_frame(&mut rd).await;
    let err = completed
        .get("error")
        .expect("spec task.complete must be rejected");
    assert_eq!(err["code"], json!(RpcError::INVALID_PARAMS), "{err:#?}");

    send_frame(
        &mut wr,
        tools_call_frame(
            13,
            "calm.task.fail",
            &b.thread_id,
            json!({"idempotency_key": "tf-spec-refused", "reason": "nope"}),
        ),
    )
    .await;
    let failed = recv_frame(&mut rd).await;
    let err = failed
        .get("error")
        .expect("spec task.fail must be rejected");
    assert_eq!(err["code"], json!(RpcError::INVALID_PARAMS), "{err:#?}");
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn wave_file_tools_support_two_calls_on_one_connection() {
    let b = boot().await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    send_frame(&mut wr, initialize_frame(1, &b.raw_token)).await;
    let init_resp = recv_frame(&mut rd).await;
    assert!(
        init_resp.get("error").is_none(),
        "init failed: {init_resp:#?}"
    );

    send_frame(
        &mut wr,
        tools_call_frame(20, "calm.wave.ls", &b.thread_id, json!({"path": "/"})),
    )
    .await;
    let r1 = recv_frame(&mut rd).await;
    assert!(
        r1.get("error").is_none(),
        "first wave file tools/call errored: {r1:#?}"
    );
    let entries = r1["result"]["structuredContent"]
        .as_array()
        .expect("ls structuredContent is array");
    assert!(
        entries.iter().any(|entry| entry["name"] == "index.md"),
        "root listing should contain index.md: {entries:#?}"
    );

    send_frame(
        &mut wr,
        tools_call_frame(
            21,
            "calm.wave.cat",
            &b.thread_id,
            json!({"path": "index.md"}),
        ),
    )
    .await;
    let r2 = recv_frame(&mut rd).await;
    assert!(
        r2.get("error").is_none(),
        "second wave file tools/call errored: {r2:#?}"
    );
    let structured = &r2["result"]["structuredContent"];
    assert_eq!(structured["content_type"], json!("text/markdown"));
    assert!(
        structured["content"]
            .as_str()
            .unwrap_or_default()
            .contains("Report: [report.md](report.md)"),
        "index.md should expose the current wave summary: {structured:#?}"
    );
    let _ = &b.server;
}

/// Regression: a co-tenant `calm-server` against the same XDG-shared
/// data dir must NOT steal the live socket on boot.
///
/// Pre-fix behavior: `McpServer::spawn` unconditionally
/// `remove_file()`d any existing socket file before `bind()`. When a
/// second server instance booted against the same data dir (e.g. two
/// docker stacks pointing at `$HOME/.local/share/neige-calm`), it would
/// race against the first instance's listener: the unlink severed the
/// path → listener mapping in the filesystem without closing the live
/// listener fd; the rebind then created a brand-new socket file the
/// second process bound to. The second process typically died next
/// (HTTP port already in use), leaving behind a defunct socket file at
/// the path. The first instance's listener was still alive but
/// orphaned — clients reaching it via the path got `ECONNREFUSED`.
///
/// Fix: probe the existing path with `UnixStream::connect` before
/// unlink. A live answer means another listener owns the socket; we
/// refuse to boot loudly rather than break the live tenant. This test
/// drives a stand-in listener (a `UnixListener::bind` directly, no
/// `McpServer` needed) and verifies the second `McpServer::spawn`
/// errors and leaves the original listener intact.
#[tokio::test]
async fn spawn_refuses_to_steal_live_co_tenant_socket() {
    let tmp = TempDir::new().expect("tempdir for MCP socket");
    let socket_path = tmp.path().join("kernel.sock");

    // Stand-in "live first tenant": just a raw UnixListener bound at
    // the same path. We don't need the full McpServer stack to exercise
    // the steal-detection — the probe is purely about whether a peer
    // answers `connect()`.
    let first = tokio::net::UnixListener::bind(&socket_path).expect("first listener");

    // Boot a real McpServer at the same path. Without the fix this
    // would happily unlink the path, rebind, and return Ok. With the
    // fix it must error.
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo;
    let card_role_cache = CardRoleCache::new();
    let events = EventBus::new();
    let registry = build_default_registry();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();

    let result = McpServer::spawn(
        repo,
        events,
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
        None,
    )
    .await;

    let err = match result {
        Ok(_) => panic!("second spawn must refuse to steal live socket"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already listening"),
        "expected steal-refusal error message; got: {msg}"
    );

    // The first listener must still be functional. A fresh connect +
    // accept round-trip proves the path was not stolen.
    let connect_handle = tokio::spawn({
        let p = socket_path.clone();
        async move { UnixStream::connect(&p).await }
    });
    let (accepted, _addr) = timeout(TEST_BUDGET, first.accept())
        .await
        .expect("first listener accept within budget")
        .expect("accept ok");
    let _ = connect_handle.await.expect("connect task joins").expect(
        "connect to first listener must succeed; instead the path was stolen by the second spawn",
    );
    drop(accepted);
}

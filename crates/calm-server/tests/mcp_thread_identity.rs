//! PR3b (#410) — MCP tools/call identity comes from per-call
//! `_meta.threadId`, not from initialize-time card binding.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_codex_create_tx};
use calm_server::event::EventBus;
use calm_server::mcp_server::registry::{
    ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, require_role,
};
use calm_server::mcp_server::{McpServer, ToolRegistry, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewWave};
use calm_server::plugin_host::mcp::RpcError;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, registry as tracing_registry};

const TEST_BUDGET: Duration = Duration::from_secs(5);

struct Boot {
    server: Arc<McpServer>,
    repo: Arc<dyn Repo>,
    socket_path: PathBuf,
    raw_token: String,
    wave_id: String,
    spec_card_id: String,
    plain_card_id: String,
    _tmp: TempDir,
}

async fn boot_with_registry(registry: Arc<ToolRegistry>) -> Boot {
    let tmp = TempDir::new().expect("tempdir for MCP socket");
    let socket_path = tmp.path().join("kernel.sock");
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "mcp-thread-identity-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-thread-identity-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    let spec_card_id = calm_server::model::new_id();
    let mut tx = sqlx_repo.pool().begin().await.unwrap();
    let (_spec_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        spec_card_id.clone(),
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
    .unwrap();
    tx.commit().await.unwrap();

    let plain = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    card_role_cache.insert(plain.id.clone(), CardRole::Plain, wave.id.clone());

    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo.clone(),
        EventBus::new(),
        card_role_cache,
        wave_cove_cache,
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
    )
    .await
    .unwrap();

    Boot {
        server,
        repo,
        socket_path,
        raw_token: mcp_token.unwrap(),
        wave_id: wave.id.as_str().to_string(),
        spec_card_id: spec_card_id.as_str().to_string(),
        plain_card_id: plain.id.as_str().to_string(),
        _tmp: tmp,
    }
}

fn capture_identity_registry() -> (Arc<ToolRegistry>, mpsc::UnboundedReceiver<ToolCallIdentity>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut registry = ToolRegistry::new();
    let handler: ToolHandler = Arc::new(move |_ctx, identity, _args| -> ToolHandlerFuture {
        let tx = tx.clone();
        Box::pin(async move {
            tx.send(identity).unwrap();
            Ok(json!({ "ok": true }))
        })
    });
    registry.register(test_descriptor("test.capture_identity"), handler);
    (Arc::new(registry), rx)
}

fn role_gate_registry() -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    let handler: ToolHandler = Arc::new(move |_ctx, identity, _args| -> ToolHandlerFuture {
        Box::pin(async move {
            require_role(&identity, CardRole::Spec)?;
            Ok(json!({ "role": "spec" }))
        })
    });
    registry.register(test_descriptor("test.spec_only"), handler);
    Arc::new(registry)
}

fn test_descriptor(name: &str) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        description: "test tool".into(),
        input_schema: json!({ "type": "object", "properties": {} }),
    }
}

async fn seed_thread(boot: &Boot, card_id: &str, thread_id: &str, role: CardRole) {
    boot.repo
        .card_codex_thread_upsert(card_id, thread_id, role, Some(boot.wave_id.as_str()))
        .await
        .unwrap();
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
    wr.write_all(&bytes).await.unwrap();
    wr.flush().await.unwrap();
}

async fn recv_frame(rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    timeout(TEST_BUDGET, rd.read_line(&mut line))
        .await
        .expect("read response within budget")
        .expect("read_line ok");
    assert!(!line.is_empty(), "got empty/EOF response line");
    serde_json::from_str(line.trim_end()).unwrap()
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
            "_meta": { "dev.neige/auth": { "token": token } }
        }
    })
}

fn tools_call_frame(id: i64, name: &str, thread_id: Option<&str>, args: Value) -> Value {
    let mut frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    if let Some(thread_id) = thread_id {
        frame["params"]["_meta"] = json!({ "threadId": thread_id });
    }
    frame
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

#[tokio::test]
async fn tools_call_with_known_thread_id_uses_mapped_card_identity() {
    let (registry, mut rx) = capture_identity_registry();
    let boot = boot_with_registry(registry).await;
    seed_thread(&boot, &boot.spec_card_id, "T1", CardRole::Spec).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", Some("T1"), json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    let identity = rx.recv().await.unwrap();
    assert_eq!(identity.card_id, boot.spec_card_id);
    assert_eq!(identity.role, CardRole::Spec);
    assert_eq!(identity.wave_id.as_deref(), Some(boot.wave_id.as_str()));
    assert_eq!(identity.thread_id, "T1");
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_call_without_thread_id_rejects_with_invalid_params() {
    let (registry, _rx) = capture_identity_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", None, json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["error"]["code"], json!(RpcError::INVALID_PARAMS));
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("_meta.threadId")
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_call_with_unknown_thread_id_rejects_with_not_found() {
    let (registry, _rx) = capture_identity_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;
    let observed = Arc::new(AtomicBool::new(false));
    let subscriber = tracing_registry().with(IdentityMissLayer {
        observed: observed.clone(),
    });
    let _ = tracing::subscriber::set_global_default(subscriber);

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", Some("missing"), json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["error"]["code"], json!(RpcError::METHOD_NOT_FOUND));
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown thread_id: missing")
    );
    assert!(
        observed.load(Ordering::SeqCst),
        "mcp_identity_miss tracing event should fire"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_call_thread_id_drives_role_gate() {
    let boot = boot_with_registry(role_gate_registry()).await;
    seed_thread(&boot, &boot.plain_card_id, "plain-thread", CardRole::Plain).await;
    seed_thread(&boot, &boot.spec_card_id, "spec-thread", CardRole::Spec).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.spec_only", Some("plain-thread"), json!({})),
    )
    .await;
    let plain_resp = recv_frame(&mut rd).await;
    assert_eq!(plain_resp["error"]["code"], json!(RpcError::INVALID_PARAMS));

    send_frame(
        &mut wr,
        tools_call_frame(3, "test.spec_only", Some("spec-thread"), json!({})),
    )
    .await;
    let spec_resp = recv_frame(&mut rd).await;
    assert!(
        spec_resp.get("error").is_none(),
        "spec thread should pass role gate: {spec_resp:#?}"
    );
    assert_eq!(
        spec_resp["result"]["structuredContent"]["role"],
        json!("spec")
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn initialize_does_not_bind_card_identity() {
    let (registry, mut rx) = capture_identity_registry();
    let boot = boot_with_registry(registry).await;
    seed_thread(&boot, &boot.plain_card_id, "plain-thread", CardRole::Plain).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", Some("plain-thread"), json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    let identity = rx.recv().await.unwrap();
    assert_eq!(identity.card_id, boot.plain_card_id);
    assert_eq!(identity.role, CardRole::Plain);
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_list_works_without_thread_id() {
    let boot = boot_with_registry(build_default_registry()).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/list errored: {resp:#?}");
    assert!(resp["result"]["tools"].as_array().unwrap().len() >= 4);
    let _ = &boot.server;
}

#[tokio::test]
async fn legacy_per_card_mcp_token_still_accepted_during_handshake() {
    let boot = boot_with_registry(build_default_registry()).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;

    send_frame(&mut wr, initialize_frame(1, &boot.raw_token)).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "initialize errored: {resp:#?}");
    assert_eq!(
        resp["result"]["serverInfo"]["name"],
        json!("neige-calm-kernel")
    );
    let _ = &boot.server;
}

struct IdentityMissLayer {
    observed: Arc<AtomicBool>,
}

impl<S> Layer<S> for IdentityMissLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() == "shared_codex_daemon::mcp_identity_miss" {
            self.observed.store(true, Ordering::SeqCst);
        }
    }
}

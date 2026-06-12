//! PR3b (#410) — MCP tools/call identity comes from per-call
//! `_meta.threadId`, not from initialize-time card binding.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, card_mcp_token_set_tx, card_with_codex_create_tx,
    runtime_bind_attribution_tx, runtime_get_active_for_card_tx, runtime_start_tx,
};
use calm_server::event::EventBus;
use calm_server::mcp_server::auth;
use calm_server::mcp_server::registry::{
    ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, require_role,
};
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
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, registry as tracing_registry};

const TEST_BUDGET: Duration = Duration::from_secs(5);

struct Boot {
    server: Arc<McpServer>,
    sqlx_repo: Arc<SqlxRepo>,
    card_role_cache: CardRoleCache,
    socket_path: PathBuf,
    raw_token: String,
    wave_id: String,
    spec_card_id: String,
    worker_card_id: String,
    _tmp: TempDir,
}

async fn boot_with_registry(registry: Arc<ToolRegistry>) -> Boot {
    boot_with_registry_and_daemon_hash(registry, None).await
}

async fn boot_with_registry_and_daemon_hash(
    registry: Arc<ToolRegistry>,
    daemon_token_hash: Option<String>,
) -> Boot {
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
    .unwrap();
    tx.commit().await.unwrap();

    let worker = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    card_role_cache.insert(worker.id.clone(), CardRole::Worker, wave.id.clone());

    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo.clone(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
        daemon_token_hash,
    )
    .await
    .unwrap();

    Boot {
        server,
        sqlx_repo,
        card_role_cache,
        socket_path,
        raw_token: mcp_token.unwrap(),
        wave_id: wave.id.as_str().to_string(),
        spec_card_id: spec_card_id.as_str().to_string(),
        worker_card_id: worker.id.as_str().to_string(),
        _tmp: tmp,
    }
}

struct TokenCard {
    card_id: String,
    mcp_token: String,
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
        annotations: None,
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn seed_thread(boot: &Boot, card_id: &str, thread_id: &str, _role: CardRole) {
    let mut tx = boot.sqlx_repo.pool().begin().await.unwrap();
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
async fn card_mcp_token_set_tx_replaces_hash() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = repo
        .cove_create(NewCove {
            name: "mcp-token-wrapper".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "mcp-token-wrapper".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card_id = calm_server::model::new_id();
    let role_cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        card_id.clone(),
        calm_server::model::NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        },
        CardRole::Spec,
        true,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let token_a = auth::CardMcpToken::generate();
    let hash_a = auth::hash_token(token_a.as_str());
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card_id, &hash_a)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        repo.card_mcp_token_lookup_by_hash(&hash_a).await.unwrap(),
        Some((card_id.clone(), hash_a.clone()))
    );

    let token_b = auth::CardMcpToken::generate();
    let hash_b = auth::hash_token(token_b.as_str());
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card_id, &hash_b)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        repo.card_mcp_token_lookup_by_hash(&hash_a)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.card_mcp_token_lookup_by_hash(&hash_b).await.unwrap(),
        Some((card_id, hash_b))
    );
}

async fn seed_card_with_mcp_token(boot: &Boot, card_id: &str, role: CardRole) -> TokenCard {
    let token = auth::CardMcpToken::generate();
    let mut tx = boot.sqlx_repo.pool().begin().await.unwrap();
    let card = calm_server::model::NewCard {
        wave_id: boot.wave_id.as_str().into(),
        kind: "codex".into(),
        sort: None,
        payload: Value::Null,
    };
    card_create_with_id_tx(
        &mut tx,
        card_id.to_string(),
        card,
        role,
        true,
        &boot.card_role_cache,
    )
    .await
    .unwrap();
    card_mcp_token_set_tx(&mut tx, card_id, &auth::hash_token(token.as_str()))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    TokenCard {
        card_id: card_id.to_string(),
        mcp_token: token.into_inner(),
    }
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

async fn initialized_client_with_token(
    boot: &Boot,
    token: &str,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    send_frame(&mut wr, initialize_frame(1, token)).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "initialize errored: {resp:#?}");
    (rd, wr)
}

async fn call_with_token(
    boot: &Boot,
    token: &str,
    name: &str,
    thread_id: Option<&str>,
    args: Value,
) -> Value {
    let (mut rd, mut wr) = initialized_client_with_token(boot, token).await;
    send_frame(&mut wr, tools_call_frame(2, name, thread_id, args)).await;
    recv_frame(&mut rd).await
}

async fn raw_call_with_token(boot: &Boot, token: &str, frame: Value) -> Value {
    let (mut rd, mut wr) = initialized_client_with_token(boot, token).await;
    send_frame(&mut wr, frame).await;
    recv_frame(&mut rd).await
}

#[tokio::test]
async fn cardbound_with_matching_thread_id_uses_resolved_thread() {
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
async fn cardbound_with_cross_card_thread_id_rejects() {
    let (registry, mut rx) = capture_identity_registry();
    let boot = boot_with_registry(registry).await;
    seed_thread(
        &boot,
        &boot.worker_card_id,
        "worker-thread",
        CardRole::Worker,
    )
    .await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", Some("worker-thread"), json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["error"]["code"], json!(RpcError::INVALID_PARAMS));
    let err_msg = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        err_msg.contains("bound card") && err_msg.contains(&boot.spec_card_id),
        "cross-card rejection should explain the bound card mismatch: {resp:#?}"
    );
    assert!(
        !err_msg.contains(&boot.worker_card_id),
        "cross-card rejection must not leak the resolved foreign card: {resp:#?}"
    );
    assert!(
        rx.try_recv().is_err(),
        "handler must not run for a cross-card CardBound threadId"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn daemontrust_with_known_thread_id_uses_mapped_card_identity() {
    let daemon_token = "daemon-token-for-thread-map";
    let (registry, mut rx) = capture_identity_registry();
    let boot =
        boot_with_registry_and_daemon_hash(registry, Some(auth::hash_token(daemon_token))).await;
    seed_thread(&boot, &boot.spec_card_id, "T-daemon", CardRole::Spec).await;
    let (mut rd, mut wr) = initialized_client_with_token(&boot, daemon_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", Some("T-daemon"), json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    let identity = rx.recv().await.unwrap();
    assert_eq!(identity.card_id, boot.spec_card_id);
    assert_eq!(identity.role, CardRole::Spec);
    assert_eq!(identity.thread_id, "T-daemon");
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_call_meta_threadid_in_params_meta_resolves_when_top_level_meta_has_no_thread_id() {
    let (registry, mut rx) = capture_identity_registry();
    let boot = boot_with_registry(registry).await;
    seed_thread(&boot, &boot.spec_card_id, "T-params", CardRole::Spec).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "_meta": { "client_request_id": "abc" },
            "params": {
                "name": "test.capture_identity",
                "arguments": {},
                "_meta": { "threadId": "T-params" }
            }
        }),
    )
    .await;

    let resp = recv_frame(&mut rd).await;
    assert!(
        resp.get("error").is_none(),
        "must resolve via params._meta when top-level lacks threadId: {resp:#?}"
    );
    let identity = rx.recv().await.unwrap();
    assert_eq!(identity.thread_id, "T-params");
    assert_eq!(identity.card_id, boot.spec_card_id);
    let _ = &boot.server;
}

#[tokio::test]
async fn cardbound_without_thread_id_uses_bound_card() {
    let (registry, mut rx) = capture_identity_registry();
    let boot = boot_with_registry(registry).await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", None, json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(
        resp.get("error").is_none(),
        "CardBound no-thread call must use bound card: {resp:#?}"
    );
    let identity = rx.recv().await.unwrap();
    assert_eq!(identity.card_id, boot.spec_card_id);
    assert_eq!(identity.role, CardRole::Spec);
    assert_eq!(identity.wave_id.as_deref(), Some(boot.wave_id.as_str()));
    assert_eq!(identity.thread_id, "card-bound");
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_call_malformed_meta_rejects_even_when_cardbound_token_present() {
    let boot = boot_with_registry(build_default_registry()).await;

    let resp = raw_call_with_token(
        &boot,
        &boot.raw_token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "_meta": "not-an-object",
            "params": {
                "name": "calm.wave.state",
                "arguments": {}
            }
        }),
    )
    .await;

    let err = resp
        .get("error")
        .expect("must reject malformed request-level _meta");
    assert_eq!(
        err["code"],
        json!(RpcError::INVALID_PARAMS),
        "must be INVALID_PARAMS before identity resolution"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_call_malformed_params_meta_also_rejects() {
    let boot = boot_with_registry(build_default_registry()).await;

    let resp = raw_call_with_token(
        &boot,
        &boot.raw_token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "calm.wave.state",
                "arguments": {},
                "_meta": ["array-not-object"]
            }
        }),
    )
    .await;

    let err = resp
        .get("error")
        .expect("must reject malformed params._meta");
    assert_eq!(err["code"], json!(RpcError::INVALID_PARAMS));
    let _ = &boot.server;
}

#[tokio::test]
async fn cardbound_with_unresolvable_thread_id_rejects() {
    let (registry, mut rx) = capture_identity_registry();
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
        rx.try_recv().is_err(),
        "handler must not run for an unresolvable CardBound threadId"
    );
    assert!(
        observed.load(Ordering::SeqCst),
        "mcp_identity_miss tracing event should fire"
    );
    let _ = &boot.server;
}

#[tokio::test]
// Uses DaemonTrust to exercise the role gate; under CardBound, cross-card rejection short-circuits first.
async fn tools_call_thread_id_drives_role_gate() {
    let daemon_token = "daemon-token-for-role-gate";
    let boot = boot_with_registry_and_daemon_hash(
        role_gate_registry(),
        Some(auth::hash_token(daemon_token)),
    )
    .await;
    seed_thread(
        &boot,
        &boot.worker_card_id,
        "worker-thread",
        CardRole::Worker,
    )
    .await;
    seed_thread(&boot, &boot.spec_card_id, "spec-thread", CardRole::Spec).await;
    let (mut rd, mut wr) = initialized_client_with_token(&boot, daemon_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.spec_only", Some("worker-thread"), json!({})),
    )
    .await;
    let worker_resp = recv_frame(&mut rd).await;
    assert_eq!(
        worker_resp["error"]["code"],
        json!(RpcError::INVALID_PARAMS)
    );

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
async fn daemontrust_without_thread_id_rejects() {
    let daemon_token = "daemon-token-without-thread";
    let (registry, mut rx) = capture_identity_registry();
    let boot =
        boot_with_registry_and_daemon_hash(registry, Some(auth::hash_token(daemon_token))).await;
    let (mut rd, mut wr) = initialized_client_with_token(&boot, daemon_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", None, json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["error"]["code"], json!(RpcError::INVALID_PARAMS));
    assert!(
        rx.try_recv().is_err(),
        "handler must not run for DaemonTrust without threadId"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn daemontrust_with_unresolvable_thread_id_rejects() {
    let daemon_token = "daemon-token-missing-thread";
    let (registry, mut rx) = capture_identity_registry();
    let boot =
        boot_with_registry_and_daemon_hash(registry, Some(auth::hash_token(daemon_token))).await;
    let (mut rd, mut wr) = initialized_client_with_token(&boot, daemon_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", Some("missing"), json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(resp["error"]["code"], json!(RpcError::METHOD_NOT_FOUND));
    assert!(
        rx.try_recv().is_err(),
        "handler must not run for DaemonTrust with an unresolvable threadId"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn default_wave_state_tool_without_thread_id_uses_cardbound_spec_identity() {
    let boot = boot_with_registry(build_default_registry()).await;
    let resp = call_with_token(&boot, &boot.raw_token, "calm.wave.state", None, json!({})).await;
    assert!(
        resp.get("error").is_none(),
        "shell-neige style CardBound no-thread call must succeed: {resp:#?}"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn pre_initialize_tools_call_rejects() {
    let boot = boot_with_registry(build_default_registry()).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "calm.wave.state", None, json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(
        resp.get("error").is_some(),
        "anonymous call must reject: {resp:#?}"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn cardbound_role_gate_still_applies() {
    let boot = boot_with_registry(build_default_registry()).await;
    let report = seed_card_with_mcp_token(&boot, "c-report-cardbound", CardRole::ReportCard).await;
    let resp = call_with_token(&boot, &report.mcp_token, "calm.wave.state", None, json!({})).await;
    assert!(
        resp.get("error").is_some(),
        "ReportCard CardBound identity must still be rejected: {resp:#?}"
    );
    assert_eq!(resp["error"]["code"], json!(RpcError::INVALID_PARAMS));
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_call_report_card_role_rejected_by_documented_role_gates() {
    let boot = boot_with_registry(build_default_registry()).await;
    let report = seed_card_with_mcp_token(&boot, "c-report-thread", CardRole::ReportCard).await;
    seed_thread(
        &boot,
        &report.card_id,
        "report-thread",
        CardRole::ReportCard,
    )
    .await;
    let (mut rd, mut wr) = initialized_client(&boot).await;

    let cases = [
        (
            "calm.task.dispatch",
            json!({
                "kind": "codex",
                "idempotency_key": "report-dispatch",
                "goal": "should not run"
            }),
        ),
        (
            "calm.task.complete",
            json!({ "idempotency_key": "report-completed" }),
        ),
        (
            "calm.task.fail",
            json!({
                "idempotency_key": "report-failed",
                "reason": "should not run"
            }),
        ),
        ("calm.wave.state", json!({})),
        (
            "calm.task.verdict",
            json!({
                "idempotency_key": "report-meta",
                "status": "accepted"
            }),
        ),
    ];

    for (idx, (tool, args)) in cases.into_iter().enumerate() {
        send_frame(
            &mut wr,
            tools_call_frame(idx as i64 + 2, tool, Some("report-thread"), args),
        )
        .await;
        let resp = recv_frame(&mut rd).await;
        assert_eq!(
            resp["error"]["code"],
            json!(RpcError::INVALID_PARAMS),
            "tool {tool}: ReportCard role must be rejected, got {resp:#?}",
        );
    }

    let _ = &boot.server;
}

#[tokio::test]
async fn daemontrust_with_worker_thread_uses_resolved_card_identity() {
    let daemon_token = "daemon-token-worker-thread";
    let (registry, mut rx) = capture_identity_registry();
    let boot =
        boot_with_registry_and_daemon_hash(registry, Some(auth::hash_token(daemon_token))).await;
    seed_thread(
        &boot,
        &boot.worker_card_id,
        "worker-thread",
        CardRole::Worker,
    )
    .await;
    let (mut rd, mut wr) = initialized_client_with_token(&boot, daemon_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(2, "test.capture_identity", Some("worker-thread"), json!({})),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/call errored: {resp:#?}");
    let identity = rx.recv().await.unwrap();
    assert_eq!(identity.card_id, boot.worker_card_id);
    assert_eq!(identity.role, CardRole::Worker);
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_list_cardbound_without_thread_id_uses_bound_role() {
    let mut registry = ToolRegistry::new();
    let handler: ToolHandler = Arc::new(move |_ctx, _identity, _args| -> ToolHandlerFuture {
        Box::pin(async move { Ok(json!({ "ok": true })) })
    });
    registry.register(test_descriptor("test.spec_no_annotations"), handler.clone());
    registry.register(
        ToolDescriptor {
            name: "test.worker_only".into(),
            description: "worker test tool".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            annotations: None,
            visible_to_roles: &[CardRole::Worker],
        },
        handler,
    );
    let boot = boot_with_registry(Arc::new(registry)).await;
    seed_thread(
        &boot,
        &boot.worker_card_id,
        "worker-thread",
        CardRole::Worker,
    )
    .await;
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
    let result = &resp["result"];
    assert_eq!(result["tools"].as_array().unwrap().len(), 1);
    let entry = &result["tools"][0];
    assert_eq!(entry["name"], json!("test.spec_no_annotations"));
    assert!(
        entry.get("annotations").is_none(),
        "descriptor with annotations: None must omit the key from tools/list (got {entry:#?})"
    );

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list",
            "params": { "_meta": { "threadId": "worker-thread" } }
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/list errored: {resp:#?}");
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(
        tools.is_empty(),
        "CardBound tools/list must not expose foreign-card thread tools: {tools:#?}"
    );
    let _ = &boot.server;
}

#[tokio::test]
async fn tools_list_daemontrust_without_thread_id_returns_role_union() {
    let daemon_token = "daemon-token-tools-list";
    let mut registry = ToolRegistry::new();
    let handler: ToolHandler = Arc::new(move |_ctx, _identity, _args| -> ToolHandlerFuture {
        Box::pin(async move { Ok(json!({ "ok": true })) })
    });
    registry.register(test_descriptor("test.spec_tool"), handler);
    let boot = boot_with_registry_and_daemon_hash(
        Arc::new(registry),
        Some(auth::hash_token(daemon_token)),
    )
    .await;
    let (mut rd, mut wr) = initialized_client_with_token(&boot, daemon_token).await;

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
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], json!("test.spec_tool"));
    let _ = &boot.server;
}

#[tokio::test]
async fn per_card_mcp_token_still_accepted_during_handshake() {
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

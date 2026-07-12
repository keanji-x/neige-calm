use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_mcp_token_set_tx, card_with_codex_create_tx, session_bind_attribution_tx,
    session_mcp_token_set_tx, session_projection_active_for_card_tx, session_start_runtime_tx,
};
use calm_server::event::{BroadcastEnvelope, EventBus};
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewWave, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

pub const TEST_BUDGET: Duration = Duration::from_secs(5);

/// Carded boot helper — mints one card with the requested role and
/// returns everything callers need to drive an MCP session.
pub struct CardBoot {
    pub server: Arc<McpServer>,
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub card_id: String,
    /// Other card id tests may try to smuggle into tool args to prove the
    /// identity binding ignores it.
    pub other_card_id: String,
    pub raw_token: String,
    pub daemon_token: Option<String>,
    pub session_id: String,
    pub thread_id: String,
    pub socket_path: PathBuf,
    pub _tmp: TempDir,
}

pub async fn boot_with_role(role: CardRole) -> CardBoot {
    boot_with_role_and_daemon_token(role, None).await
}

pub async fn boot_shared_daemon_with_spec_thread() -> CardBoot {
    boot_with_role_and_daemon_token(CardRole::Spec, Some("mcp-test-daemon-token".to_string())).await
}

async fn boot_with_role_and_daemon_token(role: CardRole, daemon_token: Option<String>) -> CardBoot {
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
            name: "mcp-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "mcp-test".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    let card_id = calm_server::model::new_id();
    let other_card_id = calm_server::model::new_id();
    let runtime_id = calm_server::model::new_id();

    let mut tx = sqlx_repo.pool().begin().await.unwrap();
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &runtime_id,
        None,
        wave.id.clone(),
        None,
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        role,
        true,
        &card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint card");
    let raw_token = match mcp_token {
        Some(token) => token,
        None => {
            let token = calm_server::mcp_server::auth::CardMcpToken::generate();
            let token_hash = calm_server::mcp_server::auth::hash_token(token.as_str());
            card_mcp_token_set_tx(&mut tx, &card_id, &token_hash)
                .await
                .expect("mint test-only legacy MCP token");
            session_mcp_token_set_tx(&mut tx, &runtime_id, &token_hash)
                .await
                .expect("mint test-only session MCP token");
            token.into_inner()
        }
    };
    // Mint a second card so smuggled-card tests have a real alternative id.
    let (_card_b, _term_b, _tok_b) = card_with_codex_create_tx(
        &mut tx,
        other_card_id.clone(),
        &calm_server::model::new_id(),
        None,
        wave.id.clone(),
        None,
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        &card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint sidekick card");
    tx.commit().await.unwrap();
    let thread_id = format!("thread-{card_id}");
    let session_id = seed_runtime_thread(&sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    let events = EventBus::new();
    let registry = build_default_registry();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
        daemon_token
            .as_deref()
            .map(calm_server::mcp_server::auth::hash_token),
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::env::temp_dir().join("neige-test-gate-logs"),
    )
    .await
    .expect("spawn McpServer");

    CardBoot {
        server,
        repo,
        events,
        card_id,
        other_card_id,
        raw_token,
        daemon_token,
        session_id,
        thread_id,
        socket_path,
        _tmp: tmp,
    }
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime_id = if let Some(runtime) = session_projection_active_for_card_tx(&mut tx, card_id)
        .await
        .unwrap()
    {
        let runtime_id = runtime.id.clone();
        session_bind_attribution_tx(
            &mut tx,
            &runtime_id,
            ThreadAttribution {
                runtime_id: runtime_id.clone(),
                provider: AgentProvider::Codex,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
            },
        )
        .await
        .unwrap();
        runtime_id
    } else {
        let runtime = session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: calm_server::model::new_id(),
                card_id: card_id.to_string(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        runtime.id
    };
    tx.commit().await.unwrap();
    runtime_id
}

pub async fn connect(
    path: &std::path::Path,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(path).await.expect("connect UDS");
    let (rd, wr) = stream.into_split();
    (BufReader::new(rd), wr)
}

pub async fn send_frame(wr: &mut tokio::net::unix::OwnedWriteHalf, frame: Value) {
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    wr.write_all(&bytes).await.expect("write frame");
    wr.flush().await.expect("flush frame");
}

pub async fn recv_frame(rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    timeout(TEST_BUDGET, rd.read_line(&mut line))
        .await
        .expect("read response within budget")
        .expect("read_line ok");
    assert!(!line.is_empty(), "got empty/EOF response line");
    serde_json::from_str(line.trim_end()).expect("response is valid JSON")
}

pub async fn handshake(
    rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    token: &str,
) {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "mcp-test", "version": "0.1" },
            "_meta": { "dev.neige/auth": { "token": token } }
        }
    });
    send_frame(wr, frame).await;
    let resp = recv_frame(rd).await;
    assert!(resp.get("error").is_none(), "initialize failed: {resp:#?}");
}

pub async fn handshake_daemon(
    rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    token: &str,
) {
    handshake(rd, wr, token).await;
}

pub fn tools_list_frame(id: i64, thread_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/list",
        "params": {
            "_meta": { "threadId": thread_id }
        }
    })
}

/// Scripted MCP `tools/call` over a kernel UDS socket (hoisted from
/// forge_workflow_e2e.rs's fixture-local `call_tool`, generalized off that
/// file's `Fixture`). `token` may be a card-bound session token or the
/// shared-daemon token; identity resolves from `_meta.threadId` either way,
/// so callers that only hold the daemon token (codex forge E2E) can drive
/// scripted setup calls through the same wire as real agent sessions.
pub async fn call_tool_via_socket(
    socket_path: &std::path::Path,
    token: &str,
    thread_id: &str,
    id: i64,
    name: &str,
    args: Value,
) -> Value {
    let (mut rd, mut wr) = connect(socket_path).await;
    handshake(&mut rd, &mut wr, token).await;
    send_frame(&mut wr, tools_call_frame(id, name, thread_id, args)).await;
    recv_frame(&mut rd).await
}

/// Fire-and-forget MCP `tools/call` (#840 e2): connect, handshake, send the
/// call — and return WITHOUT ever awaiting the reply. Used to drive an
/// operation whose kernel is expected to crash while (or racing) the response
/// write; `call_tool_via_socket` would panic on EOF or its 5s `TEST_BUDGET`
/// when the crash wins that race. The socket halves are returned so the caller
/// can hold the connection open across the crash window instead of injecting
/// an early client-side EOF; callers that don't care can drop them.
pub async fn send_tool_call_without_reply(
    socket_path: &std::path::Path,
    token: &str,
    thread_id: &str,
    id: i64,
    name: &str,
    args: Value,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let (mut rd, mut wr) = connect(socket_path).await;
    handshake(&mut rd, &mut wr, token).await;
    send_frame(&mut wr, tools_call_frame(id, name, thread_id, args)).await;
    (rd, wr)
}

pub fn tools_call_frame(id: i64, name: &str, thread_id: &str, args: Value) -> Value {
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

/// Wait for one envelope of `kind_tag` to land on the bus, return it.
pub async fn wait_for_kind(
    rx: &mut tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
    kind_tag: &str,
) -> BroadcastEnvelope {
    let deadline = tokio::time::Instant::now() + TEST_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timeout waiting for {kind_tag} on bus");
        }
        let env = match timeout(remaining, rx.recv()).await {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => panic!("bus recv error: {e:?}"),
            Err(_) => panic!("timeout waiting for {kind_tag}"),
        };
        if env.event.kind_tag() == kind_tag {
            return env;
        }
    }
}

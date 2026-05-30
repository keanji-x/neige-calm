//! PR7a.1 (#136 followup) — integration tests for the three PR7a emit
//! tools (`calm.dispatch_request`, `calm.task_completed`,
//! `calm.task_failed`) over the real MCP server transport.
//!
//! Each test:
//!   * Boots an `McpServer` against an in-memory `SqlxRepo`.
//!   * Mints either a Spec or Worker card (with its per-card MCP token).
//!   * Connects, `initialize`s with the token, then calls one tool.
//!   * Verifies an event broadcast frame with the correct actor + scope.
//!
//! Also covers the identity-binding invariant: a `card_id` field
//! smuggled into the tool's `arguments` is IGNORED — the kernel always
//! routes through the handshake-bound `CardIdentity`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_codex_create_tx};
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewWave};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

const TEST_BUDGET: Duration = Duration::from_secs(5);

/// Carded boot helper — mints one card with the requested role and
/// returns everything callers need to drive an MCP session.
struct CardBoot {
    server: Arc<McpServer>,
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_id: String,
    /// Other card id we'll try to smuggle into tool args to prove the
    /// identity binding ignores it.
    other_card_id: String,
    raw_token: String,
    socket_path: PathBuf,
    _tmp: TempDir,
}

async fn boot_with_role(role: CardRole) -> CardBoot {
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
            name: "mcp-emit-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-emit-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    let card_id = calm_server::model::new_id();
    let other_card_id = calm_server::model::new_id();

    let mut tx = sqlx_repo.pool().begin().await.unwrap();
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        wave.id.clone(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        role,
        // #229 PR A — test fixtures use `true` (user-deletable). The
        // dedicated guard tests in `tests/cards_deletable.rs` exercise
        // the `false` path.
        true,
        &card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint card");
    // Mint a second card so the "smuggled card_id" test has a real
    // alternative id to try to spoof with.
    let (_card_b, _term_b, _tok_b) = card_with_codex_create_tx(
        &mut tx,
        other_card_id.clone(),
        wave.id.clone(),
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
    let raw_token = mcp_token.expect("Spec/Worker card must mint a token");

    let events = EventBus::new();
    let registry = build_default_registry();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo.clone(),
        events.clone(),
        card_role_cache,
        wave_cove_cache,
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        registry,
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
        socket_path,
        _tmp: tmp,
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

async fn handshake(
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
            "clientInfo": { "name": "mcp-emit-test", "version": "0.1" },
            "_meta": { "dev.neige/auth": { "token": token } }
        }
    });
    send_frame(wr, frame).await;
    let resp = recv_frame(rd).await;
    assert!(resp.get("error").is_none(), "initialize failed: {resp:#?}");
}

fn tools_call_frame(id: i64, name: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    })
}

/// Wait for one envelope of `kind_tag` to land on the bus, return it.
async fn wait_for_kind(
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

// ---------------------------------------------------------------------------
// Per-tool happy paths.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_request_codex_emits_codex_job_requested() {
    let b = boot_with_role(CardRole::Spec).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            10,
            "calm.dispatch_request",
            json!({
                "kind": "codex",
                "idempotency_key": "dr-codex-1",
                "goal": "build a thing"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "codex.job_requested").await;
    // Spec actor → AiSpec(card_id).
    match &env.actor {
        ActorId::AiSpec(cid) => assert_eq!(cid.as_str(), b.card_id.as_str()),
        other => panic!("expected AiSpec actor; got {other:?}"),
    }
    // Scope is Card on the bound card.
    match &env.scope {
        EventScope::Card { card, .. } => assert_eq!(card.as_str(), b.card_id.as_str()),
        other => panic!("expected Card scope; got {other:?}"),
    }
    // Event carries the goal we sent.
    match &env.event {
        Event::CodexJobRequested {
            goal,
            idempotency_key,
            ..
        } => {
            assert_eq!(goal, "build a thing");
            assert_eq!(idempotency_key, "dr-codex-1");
        }
        other => panic!("expected CodexJobRequested; got {other:?}"),
    }
    let _ = &b.server;
}

#[tokio::test]
async fn task_completed_emits_task_completed_with_worker_actor() {
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            20,
            "calm.task_completed",
            json!({"idempotency_key": "tc-1", "result": {"ok": true}}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    match &env.actor {
        ActorId::AiCodex(cid) => assert_eq!(cid.as_str(), b.card_id.as_str()),
        other => panic!("expected AiCodex actor; got {other:?}"),
    }
    match &env.scope {
        EventScope::Card { card, .. } => assert_eq!(card.as_str(), b.card_id.as_str()),
        other => panic!("expected Card scope; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn task_failed_emits_task_failed_with_worker_actor() {
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            30,
            "calm.task_failed",
            json!({"idempotency_key": "tf-1", "reason": "stub failure"}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.failed").await;
    match &env.actor {
        ActorId::AiCodex(cid) => assert_eq!(cid.as_str(), b.card_id.as_str()),
        other => panic!("expected AiCodex actor; got {other:?}"),
    }
    match &env.event {
        Event::TaskFailed { reason, .. } => assert_eq!(reason, "stub failure"),
        other => panic!("expected TaskFailed; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

// ---------------------------------------------------------------------------
// Identity binding: smuggled `card_id` arg is ignored.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn smuggled_card_id_in_args_is_ignored() {
    // The transport binds the identity at handshake — sending a
    // `card_id` field in `arguments` must not let the caller claim a
    // different card. Defense-in-depth assertion against a future
    // refactor that accidentally trusts tool args for identity.
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            40,
            "calm.task_completed",
            json!({
                "idempotency_key": "tc-smuggle",
                "card_id": b.other_card_id, // <-- smuggled
                "actor": "ai_spec",          // <-- smuggled
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    // The actor / scope must still bind to the connection's card
    // (b.card_id), NOT the smuggled other_card_id.
    match &env.actor {
        ActorId::AiCodex(cid) => assert_eq!(
            cid.as_str(),
            b.card_id.as_str(),
            "smuggled card_id must not override identity binding"
        ),
        other => panic!("expected AiCodex actor; got {other:?}"),
    }
    match &env.scope {
        EventScope::Card { card, .. } => assert_eq!(
            card.as_str(),
            b.card_id.as_str(),
            "smuggled card_id must not change the event scope"
        ),
        other => panic!("expected Card scope; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

// ---------------------------------------------------------------------------
// End-to-end: dispatch_request → dispatcher → spawn failure → rollback.
// ---------------------------------------------------------------------------
//
// History: this test originally asserted the buggy pre-#310 behavior —
// that the dispatcher's `card_with_codex_create_tx` committed a worker
// card row BEFORE the daemon spawn was attempted, so even with a bogus
// `/nonexistent-daemon-bin` the orphan card would land in `cards_by_wave`
// and be observable. PR #312 (which fixes #310) flips that ordering:
// the two-stage spawn defers `CardAdded` emission until after the daemon
// handle is in hand, AND `rollback_orphan_worker` actively DELETEs the
// pre-committed card row when the spawn errors out at the OS level.
//
// Under the new semantics, driving `calm.dispatch_request[codex]` against
// a stub daemon binary must:
//   (a) return success from the MCP tool (the spec just enqueued a
//       `codex.job_requested` event — the failure is downstream),
//   (b) emit a `TaskFailed` event with the dispatch's `idempotency_key`
//       on the bus once the dispatcher drains the request and the spawn
//       fails, and
//   (c) leave NO worker card with that idempotency_key in the cards
//       table — proof the rollback fired.
//
// This is the MCP-driven mirror of
// `dispatcher_rolls_back_card_on_codex_daemon_spawn_failure_issue_310`
// in `tests/dispatcher.rs`, which already pins the dispatcher-level
// contract. This one asserts the same contract end-to-end through the
// real MCP transport.

#[tokio::test]
async fn dispatch_request_drives_dispatcher_rollback_on_stub_daemon() {
    let b = boot_with_role(CardRole::Spec).await;

    // Stand up the dispatcher on the same bus + repo with a bogus
    // daemon binary so every spawn attempt fails at the OS level.
    let cache = CardRoleCache::new();
    b.repo.seed_card_role_cache(&cache).await.unwrap();
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    b.repo.seed_wave_cove_cache(&wcc).await.unwrap();
    // #272 (N3) — Dispatcher now stores `Weak<CodexClient>` so the
    // caller MUST hold the strong Arc for the dispatcher's lifetime.
    // The local `codex` binding's natural drop at end-of-test releases
    // the strong reference.
    let codex = Arc::new(calm_server::state::CodexClient::new_stub());
    let _dispatcher = calm_server::dispatcher::Dispatcher::spawn(
        b.repo.clone(),
        b.events.clone(),
        cache.clone(),
        wcc,
        codex.clone(),
        Arc::new(calm_server::state::DaemonClient {
            data_dir: PathBuf::from("/tmp/neige-mcp-e2e-noop"),
            session_daemon_bin: PathBuf::from("/nonexistent-daemon-bin"),
            proc_supervisor_sock: Some(PathBuf::from(
                "/tmp/neige-mcp-e2e-missing-proc-supervisor.sock",
            )),
        }),
        None,
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        2,
    );

    // Subscribe BEFORE dispatch so we don't miss the TaskFailed emit.
    let mut rx = b.events.subscribe();

    let idem = "e2e-rollback-1";
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;
    send_frame(
        &mut wr,
        tools_call_frame(
            50,
            "calm.dispatch_request",
            json!({
                "kind": "codex",
                "idempotency_key": idem,
                "goal": "e2e worker spawn"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    // (a) MCP tool itself succeeds — it merely enqueued the request.
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    // (b) Wait for the dispatcher to drain, fail the spawn, run
    // `rollback_orphan_worker`, and emit `TaskFailed` for our key.
    let deadline = tokio::time::Instant::now() + TEST_BUDGET;
    let mut saw_failed = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => {
                if let Event::TaskFailed {
                    idempotency_key, ..
                } = &env.event
                    && idempotency_key == idem
                {
                    saw_failed = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert!(
        saw_failed,
        "expected dispatcher to emit task.failed for {idem} after stub-daemon spawn failure"
    );

    // (c) No worker card with our idempotency_key remains — the
    // rollback deleted the pre-committed row.
    let spec = b.repo.card_get(b.card_id.as_str()).await.unwrap().unwrap();
    let wave_id_str = spec.wave_id.as_str().to_string();
    let cards = b.repo.cards_by_wave(&wave_id_str).await.unwrap();
    let leftover: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert!(
        leftover.is_empty(),
        "expected worker card to be rolled back after spawn failure; \
         found {} leftover card(s): {:?}",
        leftover.len(),
        leftover.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
    );
    let _ = &b.server;
}

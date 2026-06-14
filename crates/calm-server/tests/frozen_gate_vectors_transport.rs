//! Issue #679 PR7b-ii Unit 3: transport-only security cells plus
//! Principal delta vectors.
//!
//! The existing `frozen_gate_vectors.rs` corpus drives the public write
//! entry (`Repo::log_pure_event`). It cannot express MCP connection mode,
//! per-call `_meta.threadId` resolution, or the Principal recorder grant.
//! This file keeps those addenda separate and intentionally small.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_codex_create_tx, runtime_bind_attribution_tx,
    runtime_get_active_for_card_tx, runtime_start_tx, runtime_supersede_tx, session_insert_tx,
    session_mark_wave_root_tx,
};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::{ToolCallIdentity, ToolHandler, ToolHandlerFuture};
use calm_server::mcp_server::{McpServer, ToolRegistry, build_default_registry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, now_ms};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::role_gate::enforce_role;
use calm_server::runtime_repo::{
    AgentProvider, RunStatus, RuntimeInit, RuntimeKind, ThreadAttribution,
};
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_truth::decision_gate::PrincipalDecisionGate;
use calm_types::worker::{
    LivenessTag, Principal, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
    WorkerSessionId, WorkerSessionState,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

const TEST_BUDGET: Duration = Duration::from_secs(5);
const EXPECTED_PRINCIPAL_DELTA_VECTOR_COUNT: usize = 32;

type IdentityCaptureRx = mpsc::UnboundedReceiver<ToolCallIdentity>;

struct Boot {
    server: Arc<McpServer>,
    sqlx_repo: Arc<SqlxRepo>,
    repo: Arc<dyn Repo>,
    card_id: String,
    session_id: String,
    thread_id: String,
    raw_token: String,
    socket_path: PathBuf,
    _tmp: TempDir,
}

async fn boot_with_registry(
    registry: Arc<ToolRegistry>,
    daemon_token_hash: Option<String>,
) -> Boot {
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
            name: "transport-vector-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "transport-vector-test".into(),
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
    let session_id = seed_runtime_thread(&sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    let events = EventBus::new();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let server = McpServer::spawn(
        repo.clone(),
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
        sqlx_repo,
        repo,
        card_id,
        session_id,
        thread_id,
        raw_token,
        socket_path,
        _tmp: tmp,
    }
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime_id = if let Some(runtime) = runtime_get_active_for_card_tx(&mut tx, card_id)
        .await
        .unwrap()
    {
        let runtime_id = runtime.id.clone();
        runtime_bind_attribution_tx(
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
        let runtime = runtime_start_tx(
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
        runtime.id
    };
    tx.commit().await.unwrap();
    runtime_id
}

async fn supersede_runtime_session(repo: &SqlxRepo, card_id: &str, thread_id: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let existing = runtime_get_active_for_card_tx(&mut tx, card_id)
        .await
        .unwrap()
        .expect("active runtime before supersede");
    let runtime = runtime_supersede_tx(
        &mut tx,
        &existing.id,
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
    tx.commit().await.unwrap();
    runtime.id
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

async fn assert_eof(rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>) {
    let mut line = String::new();
    let n = timeout(TEST_BUDGET, rd.read_line(&mut line))
        .await
        .expect("EOF within budget")
        .expect("read_line ok");
    assert_eq!(n, 0, "server must close after failed initialize");
}

fn initialize_frame(id: i64, token: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "mcp-transport-vector", "version": "0.1" },
            "_meta": {
                "dev.neige/auth": { "token": token }
            }
        }
    })
}

fn tools_call_frame(id: i64, name: &str, thread_id: Option<&str>, args: Value) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("name".into(), Value::String(name.to_string()));
    params.insert("arguments".into(), args);
    if let Some(thread_id) = thread_id {
        params.insert("_meta".into(), json!({ "threadId": thread_id }));
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": Value::Object(params)
    })
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

async fn initialize_ok(
    rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    token: &str,
) {
    send_frame(wr, initialize_frame(1, token)).await;
    let resp = recv_frame(rd).await;
    assert_eq!(resp["id"], json!(1));
    assert!(resp.get("error").is_none(), "init failed: {resp:#?}");
}

async fn call_wave_cat(
    rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    id: i64,
    thread_id: Option<&str>,
) -> Value {
    send_frame(
        wr,
        tools_call_frame(id, "calm.wave.cat", thread_id, json!({"path": "wave.json"})),
    )
    .await;
    recv_frame(rd).await
}

#[tokio::test]
async fn card_bound_valid_token_allows_thread_absent_and_present() {
    let (registry, mut identity_rx) = registry_with_wave_cat_identity_capture();
    let b = boot_with_registry(registry, None).await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    initialize_ok(&mut rd, &mut wr, &b.raw_token).await;

    let no_thread = call_wave_cat(&mut rd, &mut wr, 10, None).await;
    assert!(
        no_thread.get("error").is_none(),
        "card-bound no-thread call failed: {no_thread:#?}"
    );
    let identity = timeout(TEST_BUDGET, identity_rx.recv())
        .await
        .expect("captured card-bound identity within budget")
        .expect("captured card-bound identity");
    assert_eq!(identity.card_id, b.card_id);
    assert_eq!(identity.session_id, b.session_id);
    assert_eq!(identity.thread_id, "card-bound");

    let with_thread = call_wave_cat(&mut rd, &mut wr, 11, Some(&b.thread_id)).await;
    assert!(
        with_thread.get("error").is_none(),
        "card-bound thread call failed: {with_thread:#?}"
    );
    let identity = timeout(TEST_BUDGET, identity_rx.recv())
        .await
        .expect("captured thread identity within budget")
        .expect("captured thread identity");
    assert_eq!(identity.card_id, b.card_id);
    assert_eq!(identity.session_id, b.session_id);
    assert_eq!(identity.thread_id, b.thread_id);

    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn daemon_trust_requires_thread_id_and_routes_present_thread_id() {
    let (registry, mut identity_rx) = registry_with_wave_cat_identity_capture();
    let daemon_token = calm_server::mcp_server::auth::CardMcpToken::generate().into_inner();
    let b = boot_with_registry(
        registry,
        Some(calm_server::mcp_server::auth::hash_token(&daemon_token)),
    )
    .await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    initialize_ok(&mut rd, &mut wr, &daemon_token).await;

    let missing = call_wave_cat(&mut rd, &mut wr, 20, None).await;
    let err = missing
        .get("error")
        .expect("daemon-trust tools/call without threadId must fail");
    assert_eq!(err["code"], json!(RpcError::INVALID_PARAMS), "{err:#?}");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("requires _meta.threadId"),
        "error should name the missing thread id: {err:#?}"
    );

    let routed = call_wave_cat(&mut rd, &mut wr, 21, Some(&b.thread_id)).await;
    assert!(
        routed.get("error").is_none(),
        "daemon-trust thread call failed: {routed:#?}"
    );
    let identity = timeout(TEST_BUDGET, identity_rx.recv())
        .await
        .expect("captured daemon-thread identity within budget")
        .expect("captured daemon-thread identity");
    assert_eq!(identity.card_id, b.card_id);
    assert_eq!(identity.session_id, b.session_id);
    assert_eq!(identity.thread_id, b.thread_id);

    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn corrupt_worker_session_hash_rejects_initialize_over_transport() {
    let b = boot_with_registry(build_default_registry(), None).await;
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash = ?1 WHERE id = ?2")
        .bind("ignored-session-hash")
        .bind(&b.session_id)
        .execute(b.sqlx_repo.pool())
        .await
        .unwrap();

    let (mut rd, mut wr) = connect(&b.socket_path).await;
    send_frame(&mut wr, initialize_frame(30, &b.raw_token)).await;
    let resp = recv_frame(&mut rd).await;
    let err = resp.get("error").expect("corrupt hash must reject");
    assert_eq!(err["code"], json!(-32401), "{err:#?}");
    assert_eof(&mut rd).await;
    let _ = &b.server;
}

#[tokio::test]
async fn stale_worker_session_rejects_initialize_over_transport() {
    let b = boot_with_registry(build_default_registry(), None).await;
    sqlx::query("UPDATE worker_sessions SET state = 'superseded' WHERE id = ?1")
        .bind(&b.session_id)
        .execute(b.sqlx_repo.pool())
        .await
        .unwrap();

    let (mut rd, mut wr) = connect(&b.socket_path).await;
    send_frame(&mut wr, initialize_frame(31, &b.raw_token)).await;
    let resp = recv_frame(&mut rd).await;
    let err = resp.get("error").expect("stale session must reject");
    assert_eq!(err["code"], json!(-32401), "{err:#?}");
    assert_eof(&mut rd).await;
    let _ = &b.server;
}

#[tokio::test]
async fn card_bound_cross_session_thread_id_is_rejected_before_handler_dispatch() {
    let (registry, mut identity_rx) = registry_with_wave_cat_identity_capture();
    let b = boot_with_registry(registry, None).await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    initialize_ok(&mut rd, &mut wr, &b.raw_token).await;

    let second_thread_id = format!("thread-{}-cross-session", b.card_id);
    let second_session_id =
        supersede_runtime_session(&b.sqlx_repo, b.card_id.as_str(), &second_thread_id).await;
    assert_ne!(second_session_id, b.session_id);

    let resp = call_wave_cat(&mut rd, &mut wr, 40, Some(&second_thread_id)).await;
    let err = resp
        .get("error")
        .expect("cross-session threadId must be rejected");
    assert_eq!(err["code"], json!(RpcError::INVALID_PARAMS), "{err:#?}");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("bound session"),
        "error should name the bound-session check: {err:#?}"
    );
    assert!(
        timeout(Duration::from_millis(50), identity_rx.recv())
            .await
            .is_err(),
        "handler must not run after same_bound_session rejection"
    );

    let _ = (&b.server, &b.repo);
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalDeltaVector {
    description: String,
    principal: PrincipalSpec,
    actor: Value,
    event: Value,
    scope: Value,
    recorder_gate: bool,
    expected: PrincipalExpected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalSpec {
    session_id: String,
    wave_id: String,
    cove_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalExpected {
    decision: ExpectedDecision,
    recorder: GateExpectation,
    role: GateExpectation,
    #[serde(default)]
    role_error_contains: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ExpectedDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum GateExpectation {
    Allow,
    Deny,
    Skip,
}

struct PrincipalFixture {
    repo: Arc<SqlxRepo>,
    cache: CardRoleCache,
    wcc: WaveCoveCache,
    home_wave: WaveId,
    subst: Vec<(&'static str, String)>,
}

impl PrincipalFixture {
    async fn boot() -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        repo.seed_card_role_cache(&cache).await.unwrap();
        repo.seed_wave_cove_cache(&wcc).await.unwrap();

        let cove = repo
            .cove_create(NewCove {
                name: "principal-delta".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "principal-delta".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let home_cove = CoveId::from(cove.id.as_str());
        let home_wave = WaveId::from(wave.id.as_str());
        wcc.insert(home_wave.clone(), home_cove.clone());

        let spec = seed_role_card(&repo, &cache, &home_wave, CardRole::Spec).await;
        let executor = seed_role_card(&repo, &cache, &home_wave, CardRole::Worker).await;
        let validator = seed_role_card(&repo, &cache, &home_wave, CardRole::Worker).await;
        let report = seed_role_card(&repo, &cache, &home_wave, CardRole::ReportCard).await;

        seed_sessions(&repo, &home_wave).await;

        let subst = vec![
            ("$ROOT_SESSION", "session-root".to_string()),
            ("$PLANNER_SESSION", "session-planner".to_string()),
            ("$EXECUTOR_SESSION", "session-executor".to_string()),
            ("$VALIDATOR_SESSION", "session-validator".to_string()),
            ("$VALIDATOR_CARD", validator.as_str().to_string()),
            ("$EXECUTOR_CARD", executor.as_str().to_string()),
            ("$REPORT_CARD", report.as_str().to_string()),
            ("$SPEC_CARD", spec.as_str().to_string()),
            ("$HOME_WAVE", home_wave.as_str().to_string()),
            ("$HOME_COVE", home_cove.as_str().to_string()),
        ];

        Self {
            repo,
            cache,
            wcc,
            home_wave,
            subst,
        }
    }
}

async fn seed_role_card(
    repo: &SqlxRepo,
    cache: &CardRoleCache,
    wave: &WaveId,
    role: CardRole,
) -> CardId {
    let card = repo
        .card_create(NewCard {
            wave_id: wave.as_str().into(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = ?1 WHERE id = ?2")
        .bind(role.as_db_str())
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(card.id.clone(), role, wave.clone());
    CardId::from(card.id.as_str())
}

async fn seed_sessions(repo: &SqlxRepo, wave_id: &WaveId) {
    let mut tx = repo.pool().begin().await.unwrap();
    for (id, contract) in [
        ("session-root", WorkerContract::Planner),
        ("session-planner", WorkerContract::Planner),
        ("session-executor", WorkerContract::Executor),
        ("session-validator", WorkerContract::Validator),
    ] {
        session_insert_tx(&mut tx, worker_session(id, wave_id.clone(), contract))
            .await
            .unwrap();
    }
    session_mark_wave_root_tx(&mut tx, wave_id, &WorkerSessionId::from("session-root"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

fn worker_session(id: &str, wave_id: WaveId, contract: WorkerContract) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        wave_id,
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Running,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    }
}

fn substitute(v: &Value, subst: &[(&'static str, String)]) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (key, val) in subst {
                out = out.replace(key, val);
            }
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|x| substitute(x, subst)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| (k.clone(), substitute(x, subst)))
                .collect(),
        ),
        other => other.clone(),
    }
}

async fn run_principal_delta_vector(
    fx: &PrincipalFixture,
    v: &PrincipalDeltaVector,
) -> Result<(), String> {
    let principal = Principal::Agent {
        session_id: WorkerSessionId::from(
            substitute(&Value::String(v.principal.session_id.clone()), &fx.subst)
                .as_str()
                .expect("principal session_id remains string"),
        ),
        wave_id: WaveId::from(
            substitute(&Value::String(v.principal.wave_id.clone()), &fx.subst)
                .as_str()
                .expect("principal wave_id remains string"),
        ),
        cove_id: CoveId::from(
            substitute(&Value::String(v.principal.cove_id.clone()), &fx.subst)
                .as_str()
                .expect("principal cove_id remains string"),
        ),
    };
    let actor: ActorId = serde_json::from_value(substitute(&v.actor, &fx.subst))
        .map_err(|e| format!("actor failed to deserialize: {e}"))?;
    let scope: EventScope = serde_json::from_value(substitute(&v.scope, &fx.subst))
        .map_err(|e| format!("scope failed to deserialize: {e}"))?;
    let event: Event = serde_json::from_value(substitute(&v.event, &fx.subst))
        .map_err(|e| format!("event failed to deserialize: {e}"))?;

    let recorder = if v.recorder_gate {
        let mut tx = fx.repo.pool().begin().await.unwrap();
        let grant = PrincipalDecisionGate::new(principal)
            .recorder_grant(&mut tx, &fx.home_wave)
            .await
            .map_err(|e| format!("recorder_grant errored: {e}"))?;
        tx.rollback().await.unwrap();
        if grant {
            GateExpectation::Allow
        } else {
            GateExpectation::Deny
        }
    } else {
        GateExpectation::Skip
    };
    if recorder != v.expected.recorder {
        return Err(format!(
            "recorder gate expected {:?}, got {:?}",
            v.expected.recorder, recorder
        ));
    }

    let role_res = enforce_role(&actor, &event, &scope, &fx.cache, &fx.wcc);
    let role = if role_res.is_ok() {
        GateExpectation::Allow
    } else {
        GateExpectation::Deny
    };
    if role != v.expected.role {
        return Err(format!(
            "role gate expected {:?}, got {:?} ({role_res:?})",
            v.expected.role, role
        ));
    }
    if let (Err(err), Some(needle)) = (&role_res, &v.expected.role_error_contains)
        && !err.to_string().contains(needle)
    {
        return Err(format!(
            "role error expected to contain {needle:?}, got {err:?}"
        ));
    }

    let actual_decision = if recorder == GateExpectation::Deny || role == GateExpectation::Deny {
        ExpectedDecision::Deny
    } else {
        ExpectedDecision::Allow
    };
    if actual_decision != v.expected.decision {
        return Err(format!(
            "composed decision expected {:?}, got {:?}",
            v.expected.decision, actual_decision
        ));
    }
    Ok(())
}

#[tokio::test]
async fn principal_delta_vectors_hold() {
    let fx = PrincipalFixture::boot().await;

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/principal_delta");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("vectors dir {} unreadable: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no principal_delta vector files found under {}",
        dir.display()
    );

    let mut total = 0usize;
    let mut failures = Vec::new();
    for file in &files {
        let raw = std::fs::read_to_string(file).unwrap();
        let vectors: Vec<PrincipalDeltaVector> = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{}: invalid vector JSON: {e}", file.display()));
        let file_name = file.file_name().unwrap().to_string_lossy().into_owned();
        for (idx, vector) in vectors.iter().enumerate() {
            total += 1;
            if let Err(reason) = run_principal_delta_vector(&fx, vector).await {
                failures.push(format!(
                    "{file_name}[{idx}] `{}`: {reason}",
                    vector.description
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} principal delta vector(s) diverged:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(
        total, EXPECTED_PRINCIPAL_DELTA_VECTOR_COUNT,
        "principal_delta corpus size changed; update its own expected count"
    );
}

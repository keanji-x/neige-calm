use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, card_mcp_token_set_tx, session_mcp_token_set_tx,
    session_projection_by_id_tx, session_start_runtime_tx,
};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessSnapshot, HarnessState, PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::{CardId, TrackId};
use calm_server::mcp_server::auth;
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, Track, new_id, now_ms};
use calm_server::operation::planner_harness_interrupt_adapter::PlannerHarnessInterruptOperationPayload;
use calm_server::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownOperationPayload;
use calm_server::operation::planner_harness_start_adapter::{
    HarnessProfile, PlannerHarnessStartOperationPayload,
};
use calm_server::operation::{OperationKey, OperationOutcome, PhaseTag, TxOutput};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::track_area_cache::TrackAreaCache;
use clap::Parser;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracing_subscriber::layer::Context as TracingContext;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, registry as tracing_registry};

/// Serializes intra-binary tests that toggle `FAKE_CODEX_CAPTURE_REQUESTS`
/// (or any other process env read by the fake codex shim). Peer test
/// binaries keep their own `ENV_LOCK` because each test binary is a separate
/// process.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard(&'static str);

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var(self.0);
        }
    }
}

struct TargetCaptureLayer {
    targets: Arc<Mutex<Vec<String>>>,
}

impl<S> Layer<S> for TargetCaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: TracingContext<'_, S>) {
        self.targets.lock().unwrap().push(format!(
            "{}:{}",
            event.metadata().level(),
            event.metadata().target()
        ));
    }
}

async fn state_with_fake_daemon() -> (AppState, Arc<SqlxRepo>, CardRoleCache) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(track_area_cache),
    );
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    (
        state.with_shared_codex_appserver(shared),
        repo,
        card_role_cache,
    )
}

fn fake_codex_bin() -> &'static str {
    env!("CARGO_BIN_EXE_osc-probe-child")
}

async fn state_with_live_daemon(tmp: &TempDir) -> (AppState, Arc<SqlxRepo>, CardRoleCache) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    let mut codex = CodexClient::new_stub();
    codex.codex_bin = fake_codex_bin().to_string();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(codex),
        Some(card_role_cache.clone()),
        Some(track_area_cache),
    );

    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ]);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed_from(None).unwrap();
    let pending = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let shared = SharedCodexAppServer::new_with_pending(
        &cfg,
        Arc::new(home),
        repo.clone(),
        Some(pending.clone()),
    );
    shared.start_or_takeover().await.unwrap();

    (
        state
            .with_shared_codex_appserver(shared)
            .with_pending_codex_threads(pending),
        repo,
        card_role_cache,
    )
}

async fn seed_track(repo: &SqlxRepo) -> calm_server::model::Track {
    let area = repo
        .area_create(NewArea {
            name: "adapter".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    repo.track_create(NewTrack {
        template_input: None,
        area_id: area.id,
        title: "adapter goal".into(),
        sort: None,
        cwd: "/tmp".into(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .unwrap()
}

async fn seed_planner_card(
    repo: &SqlxRepo,
    role_cache: &CardRoleCache,
    track: &Track,
    card_id: &str,
) {
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        card_id.to_string(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "codex_source": "shared",
                "planner_harness": true
            }),
        },
        CardRole::Planner,
        false,
        role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn seed_plain_chat_card(
    repo: &SqlxRepo,
    role_cache: &CardRoleCache,
    track: &Track,
    card_id: &str,
) {
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        card_id.to_string(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        },
        CardRole::Worker,
        false,
        role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// The track-assistant twin of [`seed_plain_chat_card`]: the same shape the
/// #1189 mint writes — `CardRole::Assistant` plus the `assistant` marker.
async fn seed_assistant_card(
    repo: &SqlxRepo,
    role_cache: &CardRoleCache,
    track: &Track,
    card_id: &str,
) {
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        card_id.to_string(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "assistant"}),
        },
        CardRole::Assistant,
        false,
        role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[test]
fn legacy_start_payload_defaults_to_planner_profile() {
    let mut payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::Kernel,
        track_id: "track-old".into(),
        planner_card_id: CardId::from("card-old".to_string()),
        report_card_id: None,
        sort: None,
        cwd: "/tmp".into(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: false,
        profile: HarnessProfile::PlainChat,
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    payload.as_object_mut().unwrap().remove("profile");
    let decoded: PlannerHarnessStartOperationPayload = serde_json::from_value(payload).unwrap();
    assert_eq!(decoded.profile, HarnessProfile::Planner);
}

fn key() -> OperationKey {
    OperationKey {
        operation_key: new_id(),
        idempotency_key: None,
        payload_hash: new_id(),
    }
}

async fn wait_op(state: &AppState, op_id: &String) -> OperationOutcome {
    state.operation_runtime.wait(op_id).await.unwrap().outcome
}

async fn wait_for_requests(path: &Path, min_count: usize) -> Vec<Value> {
    for _ in 0..100 {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let rows = raw
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<Value>>();
            if rows.len() >= min_count {
                return rows;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for captured fake app-server requests");
}

async fn card_mcp_hash(repo: &SqlxRepo, card_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens WHERE card_id = ?1")
        .bind(card_id)
        .fetch_optional(repo.pool())
        .await
        .unwrap()
}

async fn card_session_id(repo: &SqlxRepo, card_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

async fn track_root_session_id(repo: &SqlxRepo, track_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

async fn assert_card_session_mcp_hash_parity(
    repo: &SqlxRepo,
    card_id: &str,
    runtime_id: &str,
) -> String {
    let (card_hash, session_hash): (String, Option<String>) = sqlx::query_as(
        r#"SELECT c.hashed_token, ws.mcp_token_hash
             FROM card_mcp_tokens c
             JOIN worker_sessions ws ON ws.id = ?2
            WHERE c.card_id = ?1"#,
    )
    .bind(card_id)
    .bind(runtime_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(!card_hash.is_empty(), "card MCP hash must be populated");
    assert_eq!(
        session_hash.as_deref(),
        Some(card_hash.as_str()),
        "worker_sessions.mcp_token_hash must mirror the planner MCP hash"
    );
    card_hash
}

fn thread_start_token(req: &Value) -> &str {
    req.pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
        .and_then(Value::as_str)
        .expect("thread/start config must carry NEIGE_MCP_TOKEN")
}

#[tokio::test]
async fn start_interrupt_and_shutdown_adapters_drive_harness_lifecycle() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));

    let runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Idle);
    assert!(runtime.thread_id.is_some());
    assert!(state.harness.get(&runtime.id).is_some());

    let harness = state.harness.get(&runtime.id).unwrap();
    let thread_id = runtime.thread_id.clone().unwrap();
    let turn_id = "turn-interrupt".to_string();
    state
        .shared_codex_appserver
        .set_active_turn_for_test(&thread_id, &turn_id);
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: turn_id.clone(),
            started_at: Instant::now(),
        })
        .await;
    let interrupt_id = state
        .operation_runtime
        .submit(
            "planner-harness-interrupt",
            key(),
            serde_json::to_value(PlannerHarnessInterruptOperationPayload {
                runtime_id: runtime.id.clone(),
                reason: "test interrupt".into(),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &interrupt_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        state
            .shared_codex_appserver
            .active_turn_for_test(&thread_id)
            .is_none()
    );

    let shutdown_id = state
        .operation_runtime
        .submit(
            "planner-harness-shutdown",
            key(),
            serde_json::to_value(PlannerHarnessShutdownOperationPayload {
                runtime_id: runtime.id.clone(),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &shutdown_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let stored = repo
        .session_projection_by_id(&runtime.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, WorkerSessionState::Superseded);
    assert!(state.harness.get(&runtime.id).is_none());
}

#[tokio::test]
async fn shutdown_replay_after_crash_falls_back_to_thread_interrupt() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;
    let runtime_id = new_id();
    let thread_id = "thread-crash-replay".to_string();
    let turn_id = "turn-crash-replay".to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id,
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Superseded,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: Some(turn_id.clone()),
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(state.harness.get(&runtime_id).is_none());

    let shutdown_id = state
        .operation_runtime
        .submit(
            "planner-harness-shutdown",
            key(),
            serde_json::to_value(PlannerHarnessShutdownOperationPayload {
                runtime_id: runtime_id.clone(),
            })
            .unwrap(),
        )
        .await
        .unwrap();

    assert!(matches!(
        wait_op(&state, &shutdown_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    assert!(
        state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .contains(&(thread_id.clone(), turn_id.clone()))
    );
}

#[tokio::test]
async fn fresh_thread_sends_per_card_mcp_config_and_rotates_hash() {
    let _guard = ENV_LOCK.lock().await;
    let tmp = TempDir::new().unwrap();
    let capture_file = tmp.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");

    let (state, repo, role_cache) = state_with_live_daemon(&tmp).await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;
    assert!(card_mcp_hash(&repo, &card_id).await.is_none());

    let first_payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let first_op = state
        .operation_runtime
        .submit("planner-harness-start", key(), first_payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &first_op).await,
        OperationOutcome::Succeeded { .. }
    ));
    let first_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("first mint stores card MCP hash");
    let first_runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("first planner runtime");
    assert_eq!(
        assert_card_session_mcp_hash_parity(&repo, &card_id, &first_runtime.id).await,
        first_hash
    );

    let rows = wait_for_requests(&capture_file, 2).await;
    let starts = rows
        .iter()
        .filter(|row| row.get("method").and_then(Value::as_str) == Some("thread/start"))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1);
    let first_token = thread_start_token(starts[0]).to_string();
    assert_eq!(auth::hash_token(&first_token), first_hash);
    assert!(
        starts[0]
            .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_SOCKET")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    );

    let second_payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: true,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let second_op = state
        .operation_runtime
        .submit("planner-harness-start", key(), second_payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &second_op).await,
        OperationOutcome::Succeeded { .. }
    ));
    let second_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("second mint stores card MCP hash");
    assert_ne!(first_hash, second_hash);
    let second_runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("second planner runtime");
    assert_eq!(
        assert_card_session_mcp_hash_parity(&repo, &card_id, &second_runtime.id).await,
        second_hash
    );

    let rows = wait_for_requests(&capture_file, 3).await;
    let starts = rows
        .iter()
        .filter(|row| row.get("method").and_then(Value::as_str) == Some("thread/start"))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 2);
    let second_token = thread_start_token(starts[1]);
    assert_eq!(auth::hash_token(second_token), second_hash);
    assert_ne!(first_token, second_token);
}

/// #838 (lean Move 1) planner-path point-of-use test.
///
/// The sibling of `worker_exec_shell_env.rs::worker_thread_start_carries_neige_mcp_exec_shell_env`
/// for the PLANNER spawn path. It drives the production `planner-harness-start`
/// operation end-to-end through the operation runtime against a live fake
/// codex app-server, captures the inbound `thread/start` request, and asserts
/// the planner `thread/start` carries the channel-3 MCP exec-shell env in
/// `/params/config/shell_environment_policy/set` — `NEIGE_MCP_SOCKET`
/// value-pinned to the socket the planner path actually resolves, and a
/// non-empty `NEIGE_MCP_TOKEN`.
///
/// `thread/start` `config.shell_environment_policy.set` is THE ONLY channel
/// reaching the AI exec-shell, so this pins the byte shape at the planner
/// point-of-use. Before #838-2 the planner path built this shape via its own
/// parallel `PlannerThread*` structs; after the refactor it goes through the
/// shared `card_mcp_thread_start_config` helper. This test characterizes the
/// behavior and must stay GREEN across that migration (shape preserved).
///
/// In this harness the planner adapter is wired via `AppState::from_parts` with
/// no live `McpServer`, so `mcp_socket_path_for_thread()` resolves through the
/// `fixtures`-gated `fixture_socket_path()` — the value we pin against here,
/// the planner analogue of the worker sibling pinning to
/// `server.shim_config.socket_path`.
#[tokio::test]
async fn planner_thread_start_carries_neige_mcp_exec_shell_env() {
    let _guard = ENV_LOCK.lock().await;
    let tmp = TempDir::new().unwrap();
    let capture_file = tmp.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");

    let (state, repo, role_cache) = state_with_live_daemon(&tmp).await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;

    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("planner channel-3 point-of-use".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));

    let rows = wait_for_requests(&capture_file, 2).await;
    let starts = rows
        .iter()
        .filter(|row| row.get("method").and_then(Value::as_str) == Some("thread/start"))
        .collect::<Vec<_>>();
    assert_eq!(
        starts.len(),
        1,
        "planner spawn must send exactly one thread/start"
    );
    let thread_start = starts[0];

    // The #838 planner-path channel-3 assertions: the planner thread/start must
    // carry the MCP exec-shell env in shell_environment_policy.set so the
    // planner AI exec-shell can reach + authenticate to the MCP socket.
    let mcp_socket = thread_start
        .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_SOCKET")
        .and_then(Value::as_str);
    let mcp_token = thread_start
        .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
        .and_then(Value::as_str);

    // Value-pin the socket to the path the planner adapter actually resolves
    // (the `fixtures` socket, since this harness wires no live McpServer).
    let expected_socket =
        calm_server::operation::planner_harness_start_adapter::fixture_socket_path()
            .to_string_lossy()
            .into_owned();
    assert_eq!(
        mcp_socket,
        Some(expected_socket.as_str()),
        "#838: planner thread/start NEIGE_MCP_SOCKET must match the planner-resolved \
         MCP socket. Captured request: {thread_start}"
    );
    assert!(
        mcp_socket.is_some_and(|value| !value.is_empty()),
        "#838: planner thread/start must set a non-empty NEIGE_MCP_SOCKET in \
         shell_environment_policy.set. Captured request: {thread_start}"
    );
    assert!(
        mcp_token.is_some_and(|value| !value.is_empty()),
        "#838: planner thread/start must set a non-empty NEIGE_MCP_TOKEN in \
         shell_environment_policy.set — otherwise the planner AI exec-shell \
         cannot authenticate to the MCP socket. Captured request: {thread_start}"
    );
    // The token shipped on the wire must be the freshly minted raw token whose
    // hash is persisted for the card (parity with `thread_start_token` usage).
    let card_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("planner mint stores card MCP hash");
    assert_eq!(auth::hash_token(mcp_token.unwrap()), card_hash);
}

#[tokio::test]
async fn plain_chat_thread_start_has_no_mcp_config() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_plain_chat_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile: HarnessProfile::PlainChat,
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    let outcome = wait_op(&state, &op_id).await;
    assert!(
        matches!(outcome, OperationOutcome::Succeeded { .. }),
        "plain-chat start failed: {outcome:?}"
    );

    assert_eq!(
        state
            .shared_codex_appserver
            .started_thread_params_for_test(),
        vec![(None, true, None)],
        // The deferred mint API does not receive a card role, so this path can
        // only observe developer instructions and ThreadConfig::NoMcp. Role is
        // observable only on the non-deferred path and is locked by
        // plain_chat_non_deferred_thread_start_uses_worker_role.
        "plain-chat thread/start must select ThreadConfig::NoMcp"
    );
    assert_eq!(
        repo.card_get(&card_id)
            .await
            .unwrap()
            .expect("plain-chat card after thread start")
            .payload["harness_profile"],
        "plain_chat",
        "INV-CHAT-011 should turn red if thread start loses the boot-recovery marker"
    );
    assert!(card_mcp_hash(&repo, &card_id).await.is_some());
    let runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("plain-chat runtime");
    assert_eq!(runtime.kind, WorkerSessionKind::CodexCard);
    assert!(
        track_root_session_id(&repo, track.id.as_str())
            .await
            .is_none()
    );
}

/// #1189 A2 — the assistant's `thread/start` is the plain chat's opposite, item
/// for item.
///
/// The sibling above pins `(None, true, None)`: no developer instructions,
/// `ThreadConfig::NoMcp`. This one pins `(Some(assistant prompt), false, None)`
/// on the SAME observable, which is the only place in the process where the two
/// profiles are distinguishable.
///
/// Why not assert on `card_mcp_tokens` / `worker_sessions.mcp_token_hash`
/// instead: those rows prove nothing here. `mint_card_mcp_token_pair()` and the
/// `new_mcp_token_hash` write in `planner_harness_start_adapter` run for EVERY
/// profile — the plain-chat sibling above asserts the same non-null hash — and
/// the profile decides only whether the raw token reaches `ThreadConfig`. Move
/// `HarnessProfile::Assistant` into the `NoMcp` arm and every token-row
/// assertion in the suite stays green while the assistant's only write channel
/// is severed. `is_no_mcp` in this tuple is the assertion that turns red.
#[tokio::test]
async fn assistant_thread_start_carries_mcp_config_and_the_assistant_prompt() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_assistant_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile: HarnessProfile::Assistant,
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    let outcome = wait_op(&state, &op_id).await;
    assert!(
        matches!(outcome, OperationOutcome::Succeeded { .. }),
        "assistant start failed: {outcome:?}"
    );

    let expected_prompt =
        calm_server::planner_card::render_assistant_prompt_for_test(track.id.as_str());
    assert_eq!(
        state
            .shared_codex_appserver
            .started_thread_params_for_test(),
        vec![(Some(expected_prompt), false, None)],
        // `false` is `is_no_mcp`: the assistant must get ThreadConfig::McpShell.
        // The prompt is pinned by equality, not by a substring, so wiring the
        // assistant to the PLANNER prompt is red here too.
        "assistant thread/start must carry MCP config and the assistant prompt"
    );

    // The rest of the mint contract, restated as the plain-chat sibling's
    // point-for-point opposite.
    let card = repo
        .card_get(&card_id)
        .await
        .unwrap()
        .expect("assistant card after thread start");
    assert_eq!(
        card.payload["harness_profile"], "assistant",
        "thread start must not lose the boot-recovery marker"
    );
    assert_eq!(
        role_cache.get(&card.id),
        Some(CardRole::Assistant),
        "the persisted role is what the MCP tool gate reads"
    );
    assert!(card_mcp_hash(&repo, &card_id).await.is_some());
    let runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("assistant runtime");
    assert_eq!(
        runtime.kind,
        WorkerSessionKind::CodexCard,
        "SharedPlanner would make this a Planner session"
    );
    assert!(
        track_root_session_id(&repo, track.id.as_str())
            .await
            .is_none(),
        "the assistant must not become the track's root session"
    );
}

#[tokio::test]
async fn plain_chat_non_deferred_thread_start_uses_worker_role() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_plain_chat_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: false,
        profile: HarnessProfile::PlainChat,
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();

    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    let outcome = wait_op(&state, &op_id).await;
    assert!(
        matches!(outcome, OperationOutcome::Succeeded { .. }),
        "should turn red if non-deferred PlainChat uses the planner role: {outcome:?}"
    );
    assert_eq!(
        state
            .shared_codex_appserver
            .started_thread_params_for_test(),
        vec![(None, true, Some(CardRole::Worker))],
        "non-deferred PlainChat must start without MCP config as a worker"
    );
}

#[tokio::test]
async fn failed_thread_start_keeps_existing_token_hash_and_runtime() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;

    let old_hash = auth::hash_token("old-runtime-token");
    let old_runtime_id = new_id();
    let old_thread_id = "thread-old-token-preserved".to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card_id, &old_hash)
        .await
        .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card_id.clone(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(old_thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    session_mcp_token_set_tx(&mut tx, &old_runtime_id, &old_hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        card_session_id(&repo, &card_id).await.as_deref(),
        Some(old_runtime_id.as_str())
    );
    assert_eq!(
        track_root_session_id(&repo, track.id.as_str())
            .await
            .as_deref(),
        Some(old_runtime_id.as_str())
    );

    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: true,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();

    match wait_op(&state, &op_id).await {
        OperationOutcome::Failed {
            from_phase,
            last_error,
            ..
        } => {
            assert_eq!(from_phase, PhaseTag::AppServerInteract);
            assert!(
                last_error.contains("forced thread/start failure"),
                "unexpected error: {last_error}"
            );
        }
        other => panic!("expected failed thread/start operation, got {other:?}"),
    }
    assert_eq!(
        card_mcp_hash(&repo, &card_id).await.as_deref(),
        Some(old_hash.as_str())
    );
    assert_eq!(
        card_session_id(&repo, &card_id).await.as_deref(),
        Some(old_runtime_id.as_str()),
        "failed deferred mint must not leave card linked to the placeholder session"
    );
    assert_eq!(
        track_root_session_id(&repo, track.id.as_str())
            .await
            .as_deref(),
        Some(old_runtime_id.as_str()),
        "failed deferred mint must not leave recorder root on the placeholder session"
    );

    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("old runtime remains active");
    assert_eq!(active.id, old_runtime_id);
    assert_eq!(active.status, WorkerSessionState::Idle);
    assert_eq!(active.thread_id.as_deref(), Some(old_thread_id.as_str()));

    let session = repo
        .session_get_by_active_token_hash(&old_hash)
        .await
        .unwrap()
        .expect("old MCP token should still resolve after failed deferred mint");
    assert_eq!(session.id.as_str(), old_runtime_id.as_str());
    let identity = repo
        .card_identity_get_by_session(session.id.as_str())
        .await
        .unwrap()
        .expect("old session should still resolve card identity");
    assert_eq!(identity.card_id, CardId::from(card_id.clone()));
    assert_eq!(identity.track_id, track.id);
}

#[tokio::test]
async fn force_new_thread_kills_old_pty_immediately() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;

    let old_runtime_id = new_id();
    let old_thread_id = "thread-force-reset-old-pty".to_string();
    let old_snapshot = HarnessSnapshot::initial(0, vec![]);
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card_id.clone(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(old_thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&old_snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let old_harness = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: old_runtime_id.clone(),
        track_id: TrackId::from(track.id.to_string()),
        card_id: CardId::from(card_id.clone()),
        thread_id: Some(old_thread_id.clone()),
        repo: repo_dyn,
        events: state.events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: state.track_area_cache.clone(),
        daemon: state.shared_codex_appserver.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot: old_snapshot,
    });
    state.harness.insert(old_runtime_id.clone(), old_harness);
    assert!(state.harness.get(&old_runtime_id).is_some());

    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: true,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();

    match wait_op(&state, &op_id).await {
        OperationOutcome::Failed {
            from_phase,
            last_error,
            ..
        } => {
            assert_eq!(from_phase, PhaseTag::AppServerInteract);
            assert!(
                last_error.contains("forced thread/start failure"),
                "unexpected error: {last_error}"
            );
        }
        other => panic!("expected forced thread/start failure, got {other:?}"),
    }
    assert_eq!(
        state.shared_codex_appserver.turn_start_count_for_test(),
        0,
        "replacement harness must not spawn in this failure pin"
    );
    assert!(
        state.harness.get(&old_runtime_id).is_none(),
        "force_new_thread must remove the old PTY handle before replacement spawn"
    );
}

#[tokio::test]
async fn fresh_start_supersedes_existing_shared_planner_runtime() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;

    let old_runtime_id = new_id();
    let old_thread_id = "thread-existing-planner-runtime".to_string();
    let old_snapshot = HarnessSnapshot::initial(0, vec![]);
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id.clone(),
            card_id: card_id.clone(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(old_thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&old_snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let old_harness = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: old_runtime_id.clone(),
        track_id: TrackId::from(track.id.to_string()),
        card_id: CardId::from(card_id.clone()),
        thread_id: Some(old_thread_id.clone()),
        repo: repo_dyn,
        events: state.events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: state.track_area_cache.clone(),
        daemon: state.shared_codex_appserver.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot: old_snapshot,
    });
    state.harness.insert(old_runtime_id.clone(), old_harness);

    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();

    match wait_op(&state, &op_id).await {
        OperationOutcome::Succeeded { .. } => {}
        other => panic!("expected planner harness fresh start to succeed, got {other:?}"),
    }

    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("new active runtime");
    assert_ne!(active.id, old_runtime_id);
    assert_eq!(active.kind, WorkerSessionKind::SharedPlanner);
    assert_eq!(active.status, WorkerSessionState::Idle);
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));

    let mut tx = repo.pool().begin().await.unwrap();
    let old = session_projection_by_id_tx(&mut tx, &old_runtime_id)
        .await
        .unwrap()
        .expect("old runtime");
    tx.commit().await.unwrap();
    assert_eq!(old.status, WorkerSessionState::Superseded);
    assert_eq!(old.thread_id.as_deref(), Some(old_thread_id.as_str()));
    assert!(
        state.harness.get(&old_runtime_id).is_none(),
        "old harness handle must be shut down after supersede"
    );

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)
             FROM worker_sessions
            WHERE card_id = ?1
              AND state NOT IN ('failed', 'exited', 'superseded')"#,
    )
    .bind(&card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count.0, 1);
    if let Some(handle) = state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn start_adapter_reuses_checkpointed_thread_on_recovery() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let first_thread = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row")
        .thread_id;
    assert_eq!(first_thread.as_deref(), Some("fake-thread-0001"));

    sqlx::query(
        r#"UPDATE operations
              SET phase = 'app_server_interact',
                  phase_detail_json = ?1,
                  lease_owner = NULL,
                  lease_until_ms = NULL,
                  completed_at_ms = NULL
            WHERE id = ?2"#,
    )
    .bind(
        serde_json::to_string(&serde_json::json!({
            "kind": "mint_and_await",
            "thread_id": first_thread,
        }))
        .unwrap(),
    )
    .bind(&op_id)
    .execute(repo.pool())
    .await
    .unwrap();

    state.operation_runtime.drive().await.unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let recovered_thread = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row after recovery")
        .thread_id;
    assert_eq!(recovered_thread.as_deref(), Some("fake-thread-0001"));
    assert!(
        state
            .shared_codex_appserver
            .cached_card_for_thread("fake-thread-0002")
            .is_none(),
        "recovery must not mint a second planner thread"
    );
}

#[tokio::test]
async fn start_adapter_reuses_runtime_thread_when_output_lacks_thread_id() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let first_thread = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row")
        .thread_id;
    assert_eq!(first_thread.as_deref(), Some("fake-thread-0001"));
    let original_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("initial start stores card MCP hash");

    let (tx_output_json,): (String,) =
        sqlx::query_as("SELECT tx_output_json FROM operations WHERE id = ?1")
            .bind(&op_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut output: TxOutput = serde_json::from_str(&tx_output_json).unwrap();
    output
        .data
        .as_object_mut()
        .expect("operation output data")
        .remove("codex_thread_id");

    sqlx::query(
        r#"UPDATE operations
              SET phase = 'app_server_interact',
                  phase_detail_json = ?1,
                  tx_output_json = ?2,
                  lease_owner = NULL,
                  lease_until_ms = NULL,
                  completed_at_ms = NULL
            WHERE id = ?3"#,
    )
    .bind(
        serde_json::to_string(&serde_json::json!({
            "kind": "mint_and_await",
            "thread_id": Value::Null,
        }))
        .unwrap(),
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&op_id)
    .execute(repo.pool())
    .await
    .unwrap();

    state.operation_runtime.drive().await.unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let recovered_thread = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row after recovery")
        .thread_id;
    assert_eq!(recovered_thread.as_deref(), Some("fake-thread-0001"));
    assert!(
        state
            .shared_codex_appserver
            .cached_card_for_thread("fake-thread-0002")
            .is_none(),
        "recovery must reuse runtime thread_id instead of minting another planner thread"
    );
    assert_eq!(
        card_mcp_hash(&repo, &card_id).await.as_deref(),
        Some(original_hash.as_str()),
        "reuse with a valid per-card token row must leave the card MCP hash in place"
    );
}

#[tokio::test]
async fn reusable_thread_without_token_fails_op() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let original_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("initial start stores card MCP hash");

    sqlx::query("DELETE FROM card_mcp_tokens WHERE card_id = ?1")
        .bind(&card_id)
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(card_mcp_hash(&repo, &card_id).await.is_none());
    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active runtime before reusable-thread recovery");
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    let active_runtime_id = active.id.clone();
    let active_status = active.status;
    let active_thread_id = active.thread_id.clone();

    let (tx_output_json,): (String,) =
        sqlx::query_as("SELECT tx_output_json FROM operations WHERE id = ?1")
            .bind(&op_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut output: TxOutput = serde_json::from_str(&tx_output_json).unwrap();
    output
        .data
        .as_object_mut()
        .expect("operation output data")
        .remove("codex_thread_id");

    sqlx::query(
        r#"UPDATE operations
              SET phase = 'app_server_interact',
                  phase_detail_json = ?1,
                  tx_output_json = ?2,
                  lease_owner = NULL,
                  lease_until_ms = NULL,
                  completed_at_ms = NULL
            WHERE id = ?3"#,
    )
    .bind(
        serde_json::to_string(&serde_json::json!({
            "kind": "mint_and_await",
            "thread_id": Value::Null,
        }))
        .unwrap(),
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&op_id)
    .execute(repo.pool())
    .await
    .unwrap();

    let targets = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_registry().with(TargetCaptureLayer {
        targets: targets.clone(),
    });
    let _guard = tracing::subscriber::set_default(subscriber);
    state.operation_runtime.drive().await.unwrap();

    match wait_op(&state, &op_id).await {
        OperationOutcome::Failed {
            from_phase,
            last_error,
            ..
        } => {
            assert_eq!(from_phase, PhaseTag::AppServerInteract);
            assert!(
                last_error.contains("no per-card MCP token row"),
                "unexpected error: {last_error}"
            );
            assert!(
                last_error.contains(&card_id),
                "missing card id in error: {last_error}"
            );
            assert!(
                last_error.contains("fake-thread-0001"),
                "missing thread id in error: {last_error}"
            );
        }
        other => panic!("expected failed reusable-thread operation, got {other:?}"),
    }
    let observed_targets = targets.lock().unwrap().clone();
    assert!(
        observed_targets
            .iter()
            .any(|target| target == "WARN:planner_harness::reusable_thread_invariant"),
        "expected planner reusable-thread invariant warning; observed targets: {observed_targets:?}"
    );
    assert!(
        card_mcp_hash(&repo, &card_id).await.is_none(),
        "failed reuse path must not re-mint a card MCP token"
    );
    let active_after = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active runtime after failed reusable-thread recovery");
    assert_eq!(active_after.id, active_runtime_id);
    assert_eq!(active_after.status, active_status);
    assert_eq!(active_after.thread_id, active_thread_id);
    assert_eq!(
        card_session_id(&repo, &card_id).await.as_deref(),
        Some(active_runtime_id.as_str()),
        "failed reuse path must keep the card linked to the existing session"
    );
    assert_eq!(
        track_root_session_id(&repo, track.id.as_str())
            .await
            .as_deref(),
        Some(active_runtime_id.as_str()),
        "failed reuse path must keep the track root linked to the existing session"
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT mcp_token_hash FROM worker_sessions WHERE id = ?1"
        )
        .bind(&active_runtime_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
        .as_deref(),
        Some(original_hash.as_str()),
        "failed reuse path must leave the running session token unchanged"
    );
}

#[tokio::test]
async fn start_adapter_mints_new_thread_when_runtime_lacks_thread_id() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_planner_card(&repo, &role_cache, &track, &card_id).await;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let (tx_output_json,): (String,) =
        sqlx::query_as("SELECT tx_output_json FROM operations WHERE id = ?1")
            .bind(&op_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut output: TxOutput = serde_json::from_str(&tx_output_json).unwrap();
    output
        .data
        .as_object_mut()
        .expect("operation output data")
        .remove("codex_thread_id");

    sqlx::query(
        r#"UPDATE worker_sessions
              SET thread_id = NULL
            WHERE card_id = ?1"#,
    )
    .bind(&card_id)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE operations
              SET phase = 'app_server_interact',
                  phase_detail_json = ?1,
                  tx_output_json = ?2,
                  lease_owner = NULL,
                  lease_until_ms = NULL,
                  completed_at_ms = NULL
            WHERE id = ?3"#,
    )
    .bind(
        serde_json::to_string(&serde_json::json!({
            "kind": "mint_and_await",
            "thread_id": Value::Null,
        }))
        .unwrap(),
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&op_id)
    .execute(repo.pool())
    .await
    .unwrap();

    state.operation_runtime.drive().await.unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let recovered_thread = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row after recovery")
        .thread_id;
    assert_eq!(recovered_thread.as_deref(), Some("fake-thread-0002"));
    assert_eq!(
        state
            .shared_codex_appserver
            .cached_card_for_thread("fake-thread-0002")
            .as_deref(),
        Some(card_id.as_str()),
        "recovery must mint and bind a runtime thread when runtime thread_id is absent"
    );
}

// ---------------------------------------------------------------------------
// #1098 §5.6 — lazy chat-card minting (`create_card`). `validate` runs before
// the operation row is inserted, so each rejection below surfaces straight out
// of `submit`.
// ---------------------------------------------------------------------------

async fn chat_track(repo: &SqlxRepo) -> Track {
    let track = seed_track(repo).await;
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(track.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    repo.track_get(track.id.as_str()).await.unwrap().unwrap()
}

fn lazy_mint_payload(track: &Track, card_id: &str, profile: HarnessProfile) -> Value {
    serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.to_string()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile,
        create_card: Some(Default::default()),
        first_message_sha256: None,
        first_message: None,
    })
    .unwrap()
}

#[tokio::test]
async fn lazy_mint_requires_the_plain_chat_profile() {
    let (state, repo, _role_cache) = state_with_fake_daemon().await;
    let track = chat_track(&repo).await;
    let error = state
        .operation_runtime
        .submit(
            "planner-harness-start",
            key(),
            lazy_mint_payload(&track, "conv-profile", HarnessProfile::Planner),
        )
        .await
        .expect_err("minting a card under the planner profile must be refused");
    assert!(
        matches!(error, calm_server::error::CalmError::BadRequest(_)),
        "unexpected error: {error:?}"
    );
    assert!(repo.card_get("conv-profile").await.unwrap().is_none());
}

#[tokio::test]
async fn lazy_mint_refuses_a_track_that_is_not_a_area_chat_track() {
    let (state, repo, _role_cache) = state_with_fake_daemon().await;
    let track = seed_track(&repo).await;
    let error = state
        .operation_runtime
        .submit(
            "planner-harness-start",
            key(),
            lazy_mint_payload(&track, "conv-track", HarnessProfile::PlainChat),
        )
        .await
        .expect_err("chat cards may only be conjured onto an area chat track");
    assert!(
        matches!(error, calm_server::error::CalmError::Forbidden(_)),
        "unexpected error: {error:?}"
    );
    assert!(repo.card_get("conv-track").await.unwrap().is_none());
}

#[tokio::test]
async fn lazy_mint_refuses_to_adopt_an_existing_card() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = chat_track(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        "conv-existing".into(),
        NewCard {
            track_id: track.id.clone(),
            title: Some("someone else's card".into()),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Worker,
        true,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let error = state
        .operation_runtime
        .submit(
            "planner-harness-start",
            key(),
            lazy_mint_payload(&track, "conv-existing", HarnessProfile::PlainChat),
        )
        .await
        .expect_err("an existing card must not be adopted as a fresh conversation");
    assert!(
        matches!(error, calm_server::error::CalmError::Conflict(_)),
        "unexpected error: {error:?}"
    );
    let card = repo.card_get("conv-existing").await.unwrap().unwrap();
    assert_eq!(card.title.as_deref(), Some("someone else's card"));
    assert_eq!(card.payload.get("harness_profile"), None);
}

/// Positive control for the three refusals above, and the shape pin for the
/// minted card: Worker/codex, kernel-owned, marked, and carrying NO
/// `planner_harness` key (INV-CHAT-016 — the old FE's planner renderer claims any
/// card that has one).
#[tokio::test]
async fn lazy_mint_creates_a_marked_kernel_owned_worker_card() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let track = chat_track(&repo).await;
    let op_id = state
        .operation_runtime
        .submit(
            "planner-harness-start",
            key(),
            lazy_mint_payload(&track, "conv-ok", HarnessProfile::PlainChat),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            wait_op(&state, &op_id).await,
            OperationOutcome::Succeeded { .. }
        ),
        "lazy mint must succeed on a chat track"
    );
    let card = repo
        .card_get("conv-ok")
        .await
        .unwrap()
        .expect("minted card");
    assert_eq!(card.kind, "codex");
    assert!(
        !card.deletable,
        "conversation cards are kernel-owned for now"
    );
    assert_eq!(role_cache.get(&card.id), Some(CardRole::Worker));
    assert_eq!(card.payload["harness_profile"], json!("plain_chat"));
    assert_eq!(card.payload["schemaVersion"], json!(1));
    assert_eq!(card.payload.get("planner_harness"), None);
    let runtime = repo
        .session_projection_active_for_card(&"conv-ok".to_string())
        .await
        .unwrap()
        .expect("session row");
    assert_eq!(runtime.kind, WorkerSessionKind::CodexCard);
    if let Some(handle) = state.harness.remove(&runtime.id) {
        let _ = handle.shutdown().await;
    }
}

/// INV-CHAT-013(a) at the adapter boundary: a `thread/start` failure takes the
/// lazily minted card back out, along with its session row.
#[tokio::test]
async fn lazy_mint_is_compensated_away_when_thread_start_fails() {
    let (state, repo, _role_cache) = state_with_fake_daemon().await;
    let track = chat_track(&repo).await;
    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let op_id = state
        .operation_runtime
        .submit(
            "planner-harness-start",
            key(),
            lazy_mint_payload(&track, "conv-doomed", HarnessProfile::PlainChat),
        )
        .await
        .unwrap();
    match wait_op(&state, &op_id).await {
        OperationOutcome::Failed { from_phase, .. } => {
            assert_eq!(from_phase, PhaseTag::AppServerInteract);
        }
        other => panic!("expected a failed thread/start, got {other:?}"),
    }
    assert!(
        repo.card_get("conv-doomed").await.unwrap().is_none(),
        "compensation must delete the card this operation minted"
    );
    let sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
            .bind("conv-doomed")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(sessions, 0);
}

/// The lazy-mint branch keeps the ordinary branch's daemon preflight. Without
/// it a down app-server would still mint and commit the card (broadcasting
/// `card.added`), fail at `thread/start`, and only then compensate it away —
/// a visible flicker and a 500 in place of a clean rejection.
#[tokio::test]
async fn lazy_mint_refuses_to_mint_while_the_app_server_is_down() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join(format!("calm-plugins-data-down-{}", new_id())),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(track_area_cache),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_stub(repo.clone()));
    assert!(!state.shared_codex_appserver.is_running());
    let track = chat_track(&repo).await;
    state
        .operation_runtime
        .submit(
            "planner-harness-start",
            key(),
            lazy_mint_payload(&track, "conv-daemon-down", HarnessProfile::PlainChat),
        )
        .await
        .expect_err("a down app-server must be refused before anything is minted");
    assert!(repo.card_get("conv-daemon-down").await.unwrap().is_none());
}

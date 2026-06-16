use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, card_mcp_token_set_tx, runtime_get_by_id_tx,
    runtime_start_tx, session_mcp_token_set_tx,
};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessSnapshot, HarnessState, SpecHarness, SpecHarnessParams,
};
use calm_server::ids::{CardId, WaveId};
use calm_server::mcp_server::auth;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, Wave, new_id, now_ms};
use calm_server::operation::spec_harness_interrupt_adapter::SpecHarnessInterruptOperationPayload;
use calm_server::operation::spec_harness_shutdown_adapter::SpecHarnessShutdownOperationPayload;
use calm_server::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use calm_server::operation::{OperationKey, OperationOutcome, PhaseTag, TxOutput};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::wave_cove_cache::WaveCoveCache;
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
    let wave_cove_cache = WaveCoveCache::new();
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
            WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache),
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
    let wave_cove_cache = WaveCoveCache::new();
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
            WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(codex),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache),
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

async fn seed_wave(repo: &SqlxRepo) -> calm_server::model::Wave {
    let cove = repo
        .cove_create(NewCove {
            name: "adapter".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    repo.wave_create(NewWave {
        cove_id: cove.id,
        title: "adapter goal".into(),
        sort: None,
        cwd: "/tmp".into(),
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .unwrap()
}

async fn seed_spec_card(repo: &SqlxRepo, role_cache: &CardRoleCache, wave: &Wave, card_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        card_id.to_string(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "codex_source": "shared",
                "spec_harness": true
            }),
        },
        CardRole::Spec,
        false,
        role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
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

async fn wave_root_session_id(repo: &SqlxRepo, wave_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
        .bind(wave_id)
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
        "worker_sessions.mcp_token_hash must mirror the spec MCP hash"
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
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));

    let runtime = repo
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.status, RunStatus::Idle);
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
            "spec-harness-interrupt",
            key(),
            serde_json::to_value(SpecHarnessInterruptOperationPayload {
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
            "spec-harness-shutdown",
            key(),
            serde_json::to_value(SpecHarnessShutdownOperationPayload {
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
    let stored = repo.runtime_get_by_id(&runtime.id).await.unwrap().unwrap();
    assert_eq!(stored.status, RunStatus::Superseded);
    assert!(state.harness.get(&runtime.id).is_none());
}

#[tokio::test]
async fn shutdown_replay_after_crash_falls_back_to_thread_interrupt() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;
    let runtime_id = new_id();
    let thread_id = "thread-crash-replay".to_string();
    let turn_id = "turn-crash-replay".to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id,
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Superseded,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: Some(turn_id.clone()),
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
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
            "spec-harness-shutdown",
            key(),
            serde_json::to_value(SpecHarnessShutdownOperationPayload {
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
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;
    assert!(card_mcp_hash(&repo, &card_id).await.is_none());

    let first_payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
    })
    .unwrap();
    let first_op = state
        .operation_runtime
        .submit("spec-harness-start", key(), first_payload)
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
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("first spec runtime");
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

    let second_payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: true,
    })
    .unwrap();
    let second_op = state
        .operation_runtime
        .submit("spec-harness-start", key(), second_payload)
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
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("second spec runtime");
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

#[tokio::test]
async fn failed_thread_start_keeps_existing_token_hash_and_runtime() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;

    let old_hash = auth::hash_token("old-runtime-token");
    let old_runtime_id = new_id();
    let old_thread_id = "thread-old-token-preserved".to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card_id, &old_hash)
        .await
        .unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: old_runtime_id.clone(),
            card_id: card_id.clone(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some(old_thread_id.clone()),
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
    session_mcp_token_set_tx(&mut tx, &old_runtime_id, &old_hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        card_session_id(&repo, &card_id).await.as_deref(),
        Some(old_runtime_id.as_str())
    );
    assert_eq!(
        wave_root_session_id(&repo, wave.id.as_str())
            .await
            .as_deref(),
        Some(old_runtime_id.as_str())
    );

    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: true,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
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
        wave_root_session_id(&repo, wave.id.as_str())
            .await
            .as_deref(),
        Some(old_runtime_id.as_str()),
        "failed deferred mint must not leave recorder root on the placeholder session"
    );

    let active = repo
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("old runtime remains active");
    assert_eq!(active.id, old_runtime_id);
    assert_eq!(active.status, RunStatus::Idle);
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
    assert_eq!(identity.wave_id, wave.id);
}

#[tokio::test]
async fn fresh_start_supersedes_existing_shared_spec_runtime() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;

    let old_runtime_id = new_id();
    let old_thread_id = "thread-existing-spec-runtime".to_string();
    let old_snapshot = HarnessSnapshot::initial(0, vec![]);
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: old_runtime_id.clone(),
            card_id: card_id.clone(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some(old_thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&old_snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let old_harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: old_runtime_id.clone(),
        wave_id: WaveId::from(wave.id.to_string()),
        card_id: CardId::from(card_id.clone()),
        thread_id: Some(old_thread_id.clone()),
        repo: repo_dyn,
        events: state.events.clone(),
        card_role_cache: role_cache.clone(),
        wave_cove_cache: state.wave_cove_cache.clone(),
        daemon: state.shared_codex_appserver.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot: old_snapshot,
    });
    state.harness.insert(old_runtime_id.clone(), old_harness);

    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
        .await
        .unwrap();

    match wait_op(&state, &op_id).await {
        OperationOutcome::Succeeded { .. } => {}
        other => panic!("expected spec harness fresh start to succeed, got {other:?}"),
    }

    let active = repo
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("new active runtime");
    assert_ne!(active.id, old_runtime_id);
    assert_eq!(active.kind, RuntimeKind::SharedSpec);
    assert_eq!(active.status, RunStatus::Idle);
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));

    let mut tx = repo.pool().begin().await.unwrap();
    let old = runtime_get_by_id_tx(&mut tx, &old_runtime_id)
        .await
        .unwrap()
        .expect("old runtime");
    tx.commit().await.unwrap();
    assert_eq!(old.status, RunStatus::Superseded);
    assert_eq!(old.thread_id.as_deref(), Some(old_thread_id.as_str()));
    assert!(
        state.harness.get(&old_runtime_id).is_none(),
        "old harness handle must be shut down after supersede"
    );

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)
             FROM runtimes
            WHERE card_id = ?1
              AND status NOT IN ('failed', 'exited', 'superseded')"#,
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
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let first_thread = repo
        .runtime_get_active_for_card(&card_id)
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
        .runtime_get_active_for_card(&card_id)
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
        "recovery must not mint a second spec thread"
    );
}

#[tokio::test]
async fn start_adapter_reuses_runtime_thread_when_output_lacks_thread_id() {
    let (state, repo, role_cache) = state_with_fake_daemon().await;
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let first_thread = repo
        .runtime_get_active_for_card(&card_id)
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
        .runtime_get_active_for_card(&card_id)
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
        "recovery must reuse runtime thread_id instead of minting another spec thread"
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
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
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
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active runtime before reusable-thread recovery");
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    let active_runtime_id = active.id.clone();
    let active_status = active.status.clone();
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
            .any(|target| target == "WARN:spec_harness::reusable_thread_invariant"),
        "expected spec reusable-thread invariant warning; observed targets: {observed_targets:?}"
    );
    assert!(
        card_mcp_hash(&repo, &card_id).await.is_none(),
        "failed reuse path must not re-mint a card MCP token"
    );
    let active_after = repo
        .runtime_get_active_for_card(&card_id)
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
        wave_root_session_id(&repo, wave.id.as_str())
            .await
            .as_deref(),
        Some(active_runtime_id.as_str()),
        "failed reuse path must keep the wave root linked to the existing session"
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
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    seed_spec_card(&repo, &role_cache, &wave, &card_id).await;
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
        reset_harness_items: false,
        force_new_thread: false,
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
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

    sqlx::query("UPDATE runtimes SET thread_id = NULL WHERE card_id = ?1")
        .bind(&card_id)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE worker_sessions
              SET thread_id = NULL
            WHERE id IN (SELECT id FROM runtimes WHERE card_id = ?1)"#,
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
        .runtime_get_active_for_card(&card_id)
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

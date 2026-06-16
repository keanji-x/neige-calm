#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{Event, EventBus};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::plan::TOOL_PLAN_UPSERT;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::operation::codex_adapter::{CodexWorkerAdapter, CodexWorkerOperationPayload};
use calm_server::operation::{
    Operation, OperationCompletionBus, OperationKey, OperationOutcome, Phase, PhaseTag,
    ProviderAdapter, SpawnCtx, SqlxOperationRepo, TxOutput,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::runtime_lookup::project_runtime_into_cards_payload;
use calm_server::runtime_repo::{RunStatus, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use clap::Parser;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Serializes intra-binary tests that toggle `FAKE_CODEX_CAPTURE_REQUESTS`
/// (or any other process env read by the fake codex shim). Peer test
/// binaries keep their own `ENV_LOCK` because each test binary is a separate
/// process.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_codex_bin() -> String {
    env!("CARGO_BIN_EXE_osc-probe-child").to_string()
}

struct Boot {
    repo: Arc<dyn Repo>,
    events: EventBus,
    cache: CardRoleCache,
    wcc: calm_server::wave_cove_cache::WaveCoveCache,
    cove_id: CoveId,
    wave_id: WaveId,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    renderer: Arc<TerminalRendererRegistry>,
    shared: Arc<SharedCodexAppServer>,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    spec_card_id: CardId,
    _tmp: TempDir,
}

async fn boot(start_shared: bool) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "worker-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "worker-shared".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();

    let mut codex = CodexClient::new_stub();
    codex.codex_bin = fake_codex_bin();
    let codex = Arc::new(codex);
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let renderer = TerminalRendererRegistry::new_with_repo(route_repo);

    let fake_codex_bin = fake_codex_bin();
    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin.as_str(),
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
    let shared = SharedCodexAppServer::new_with_pending(&cfg, Arc::new(home), repo.clone(), None);
    if start_shared {
        shared.start_or_takeover().await.unwrap();
    }

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs_pool: repo.sqlite_pool(),
        events: events.clone(),
        write: WriteContext::new(cache.clone(), wcc.clone()),
        daemon_token_hash: None,
        gate_logs_dir: tmp.path().join("gate-logs"),
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        repo,
        events,
        cache,
        wcc,
        cove_id: cove.id,
        wave_id: wave.id,
        codex,
        daemon,
        renderer,
        shared,
        ctx,
        registry: Arc::new(registry),
        spec_card_id: spec_card.id,
        _tmp: tmp,
    }
}

fn spawn_dispatcher(boot: &Boot) -> Dispatcher {
    spawn_dispatcher_with_permits(boot, 4)
}

fn spawn_dispatcher_with_permits(boot: &Boot, permits: usize) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer(
        boot.repo.clone(),
        boot.events.clone(),
        calm_server::state::WriteContext::new(boot.cache.clone(), boot.wcc.clone()),
        boot.codex.clone(),
        boot.daemon.clone(),
        boot.renderer.clone(),
        None,
        boot.shared.clone(),
        permits,
    )
}

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        session_id: "spec-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "spec-thread".into(),
    }
}

fn task_id(boot: &Boot, key: &str) -> String {
    format!("{}:{key}", boot.wave_id.as_str())
}

async fn plan_codex_task(boot: &Boot, key: &str, goal: &str) {
    let handler = boot
        .registry
        .lookup(TOOL_PLAN_UPSERT)
        .expect("plan upsert registered");
    handler(
        boot.ctx.clone(),
        spec_identity(boot),
        json!({
            "tasks": [{
                "key": key,
                "kind": "codex",
                "goal": goal,
                "context": { "from": "worker-shared-test" },
                "acceptance_criteria": "finish",
                "no_gate_reason": "shared daemon spawn coverage"
            }],
            "message": "plan shared worker task"
        }),
    )
    .await
    .expect("plan codex task");
}

async fn wait_for<F, Fut, T>(timeout: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f().await {
            return Some(v);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_requests(path: &Path, min_count: usize) -> Vec<Value> {
    for _ in 0..50 {
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
    panic!("timed out waiting for fake codex requests");
}

async fn worker_card_count_by_idem(boot: &Boot, idem: &str) -> usize {
    boot.repo
        .cards_by_wave(boot.wave_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .filter(|card| card.payload.get("idempotency_key").and_then(Value::as_str) == Some(idem))
        .count()
}

async fn worker_card_count_with_prefix(boot: &Boot, prefix: &str) -> usize {
    boot.repo
        .cards_by_wave(boot.wave_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .filter(|card| {
            card.payload
                .get("idempotency_key")
                .and_then(Value::as_str)
                .is_some_and(|idem| idem.starts_with(prefix))
        })
        .count()
}

async fn insert_pending_operation_row(repo: &SqlxRepo, op: &Operation) {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               phase, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?10)"#,
    )
    .bind(&op.id)
    .bind(&op.operation_key)
    .bind(&op.kind)
    .bind(&op.idempotency_key)
    .bind(&op.payload_hash)
    .bind(&op.target_type)
    .bind(op.target_id.as_deref())
    .bind(serde_json::to_string(&op.target).unwrap())
    .bind(serde_json::to_string(&op.payload).unwrap())
    .bind(now)
    .execute(repo.pool())
    .await
    .unwrap();
}

async fn assert_card_session_mcp_hash_parity(repo: &SqlxRepo, card_id: &str, runtime_id: &str) {
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
        "worker_sessions.mcp_token_hash must mirror the final worker MCP hash"
    );
}

async fn app_state_with_fake_worker_daemon() -> (AppState, Arc<SqlxRepo>, WaveId) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "worker-recovery".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "worker recovery".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events,
            WriteContext::new(cache.clone(), wcc.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache),
        Some(wcc),
    );
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    (state.with_shared_codex_appserver(shared), repo, wave.id)
}

fn codex_worker_key(idempotency_key: &str) -> OperationKey {
    OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(idempotency_key.to_string()),
        payload_hash: format!("payload-{idempotency_key}"),
    }
}

#[tokio::test]
async fn worker_via_shared_daemon_dedupes_same_idempotency_key() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);

    let key = "shared-dup-key";
    let idempotency_key = task_id(&boot, key);
    plan_codex_task(&boot, key, "dedup shared worker").await;
    plan_codex_task(&boot, key, "dedup shared worker").await;

    wait_for(Duration::from_secs(5), || async {
        (worker_card_count_by_idem(&boot, &idempotency_key).await == 1).then_some(())
    })
    .await
    .expect("first shared worker card minted");
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        worker_card_count_by_idem(&boot, &idempotency_key).await,
        1,
        "duplicate scheduler task idempotency_key must create exactly one card"
    );
}

#[tokio::test]
async fn worker_via_shared_daemon_dedupes_under_real_concurrent_race() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let dispatcher = spawn_dispatcher(&boot);

    let key = "shared-race-key";
    let idempotency_key = task_id(&boot, key);
    plan_codex_task(&boot, key, "race shared worker").await;
    tokio::join!(
        async { dispatcher.scheduler().poke(boot.wave_id.clone()) },
        async { dispatcher.scheduler().poke(boot.wave_id.clone()) },
    );

    wait_for(Duration::from_secs(5), || async {
        (worker_card_count_by_idem(&boot, &idempotency_key).await == 1).then_some(())
    })
    .await
    .expect("one shared worker card minted after concurrent duplicate requests");
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        worker_card_count_by_idem(&boot, &idempotency_key).await,
        1,
        "concurrent scheduler pokes must not both mint cards for one task"
    );
}

#[tokio::test]
async fn worker_recovery_reuses_persisted_thread_and_turn() {
    let _guard = ENV_LOCK.lock().await;
    let (state, repo, wave_id) = app_state_with_fake_worker_daemon().await;
    let idem = "worker-recovery-thread";
    let op_id = state
        .operation_runtime
        .submit(
            "codex-worker",
            codex_worker_key(idem),
            serde_json::to_value(CodexWorkerOperationPayload {
                actor: ActorId::User,
                wave_id: wave_id.to_string(),
                idempotency_key: idem.to_string(),
                goal: "recover without duplicate worker".into(),
                context: json!({"from": "spawn-started-recovery"}),
                acceptance_criteria: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let initial_outcome = state.operation_runtime.wait(&op_id).await.unwrap().outcome;
    assert!(
        matches!(initial_outcome, OperationOutcome::Succeeded { .. }),
        "initial codex-worker operation failed: {initial_outcome:?}"
    );

    let (tx_output_json,): (String,) =
        sqlx::query_as("SELECT tx_output_json FROM operations WHERE id = ?1")
            .bind(&op_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let output: TxOutput = serde_json::from_str(&tx_output_json).unwrap();
    let runtime_id = output.data["runtime_id"].as_str().unwrap().to_string();
    let card_id = output.data["card_id"].as_str().unwrap().to_string();
    assert_card_session_mcp_hash_parity(&repo, &card_id, &runtime_id).await;
    let runtime = repo.runtime_get_by_id(&runtime_id).await.unwrap().unwrap();
    assert_eq!(runtime.thread_id.as_deref(), Some("fake-thread-0001"));
    assert_eq!(runtime.active_turn_id.as_deref(), Some("fake-turn-0001"));
    assert_eq!(state.shared_codex_appserver.turn_start_count_for_test(), 1);
    // Let the first fast-exit fake TUI finish its cleanup before forcing the
    // row back to spawn_started; otherwise the recovery write can hit SQLite
    // writer contention in this test.
    let _ = wait_for(Duration::from_secs(2), || {
        let repo = repo.clone();
        let runtime_id = runtime_id.clone();
        async move {
            let runtime = repo.runtime_get_by_id(&runtime_id).await.unwrap().unwrap();
            (runtime.status != RunStatus::Running).then_some(())
        }
    })
    .await;

    sqlx::query(
        r#"UPDATE operations
              SET phase = 'spawn_started',
                  phase_detail_json = NULL,
                  completed_at_ms = NULL,
                  lease_owner = NULL,
                  lease_until_ms = NULL,
                  last_error = NULL
            WHERE id = ?1"#,
    )
    .bind(&op_id)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE runtimes
              SET status = 'running',
                  completed_at_ms = NULL
            WHERE id = ?1"#,
    )
    .bind(&runtime_id)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE worker_sessions
              SET state = 'running',
                  completed_at_ms = NULL
            WHERE id = ?1"#,
    )
    .bind(&runtime_id)
    .execute(repo.pool())
    .await
    .unwrap();

    state.operation_runtime.drive().await.unwrap();
    assert!(matches!(
        state.operation_runtime.wait(&op_id).await.unwrap().outcome,
        OperationOutcome::Succeeded { .. }
    ));

    let recovered = repo.runtime_get_by_id(&runtime_id).await.unwrap().unwrap();
    assert_eq!(recovered.thread_id.as_deref(), Some("fake-thread-0001"));
    assert_eq!(recovered.active_turn_id.as_deref(), Some("fake-turn-0001"));
    assert_eq!(
        state.shared_codex_appserver.turn_start_count_for_test(),
        1,
        "spawn_started recovery must not start a duplicate worker turn"
    );
    assert!(
        state
            .shared_codex_appserver
            .cached_card_for_thread("fake-thread-0002")
            .is_none(),
        "spawn_started recovery must not mint a second shared worker thread"
    );
}

#[tokio::test]
async fn worker_recovery_compensation_falls_back_to_persisted_turn_interrupt() {
    let _guard = ENV_LOCK.lock().await;
    let (state, repo, wave_id) = app_state_with_fake_worker_daemon().await;
    let idem = "worker-recovery-compensation-turn";
    let payload = serde_json::to_value(CodexWorkerOperationPayload {
        actor: ActorId::User,
        wave_id: wave_id.to_string(),
        idempotency_key: idem.to_string(),
        goal: "recover and compensate with persisted turn".into(),
        context: json!({"from": "spawn-started-compensation-recovery"}),
        acceptance_criteria: None,
    })
    .unwrap();
    let op = Operation {
        id: new_id(),
        operation_key: new_id(),
        kind: "codex-worker".into(),
        idempotency_key: Some(idem.into()),
        payload_hash: "worker-recovery-compensation-turn-hash".into(),
        target_type: "wave".into(),
        target_id: Some(wave_id.to_string()),
        target: json!({ "type": "wave", "id": wave_id }),
        payload: payload.clone(),
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    };
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let adapter = CodexWorkerAdapter::new(
        route_repo.clone(),
        state.codex.clone(),
        state.shared_codex_appserver.clone(),
        None,
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
    );
    insert_pending_operation_row(&repo, &op).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();

    let runtime_id = output.data["runtime_id"].as_str().unwrap().to_string();
    let card_id = output.data["card_id"].as_str().unwrap().to_string();
    let terminal_id = output.data["terminal_id"].as_str().unwrap().to_string();
    let thread_id = "thread-crash-replay-worker";
    let turn_id = "turn-crash-replay-worker";
    sqlx::query(
        r#"UPDATE runtimes
              SET status = 'running',
                  thread_id = ?1,
                  active_turn_id = ?2,
                  completed_at_ms = NULL
            WHERE id = ?3"#,
    )
    .bind(thread_id)
    .bind(turn_id)
    .bind(&runtime_id)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE worker_sessions
              SET state = 'running',
                  thread_id = ?1,
                  active_turn_id = ?2,
                  completed_at_ms = NULL
            WHERE id = ?3"#,
    )
    .bind(thread_id)
    .bind(turn_id)
    .bind(&runtime_id)
    .execute(repo.pool())
    .await
    .unwrap();

    let recovered_shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    assert!(
        recovered_shared
            .active_turn_id_for_thread(thread_id)
            .is_none(),
        "fresh recovered daemon must start with an empty active_turns cache"
    );
    let recovered_adapter = CodexWorkerAdapter::new(
        route_repo.clone(),
        state.codex.clone(),
        recovered_shared.clone(),
        None,
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
    );
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo,
        state.daemon.clone(),
        state.terminal_renderer.clone(),
        state.events.clone(),
        completion,
    );
    let compensation = recovered_adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "forced replay failure",
            &output,
            &op,
        )
        .await
        .unwrap();
    assert_eq!(compensation.steps.len(), 1);
    assert_eq!(
        compensation.steps[0].args["card_id"].as_str(),
        Some(card_id.as_str())
    );
    assert_eq!(
        compensation.steps[0].args["terminal_id"].as_str(),
        Some(terminal_id.as_str())
    );

    recovered_adapter
        .compensate_step(&compensation.steps[0], &output, &op, &spawn_ctx)
        .await
        .unwrap();
    assert!(
        recovered_shared
            .interrupted_turns_for_test()
            .contains(&(thread_id.to_string(), turn_id.to_string())),
        "recovered worker compensation must interrupt the persisted turn when active_turns is empty"
    );
}

#[tokio::test]
async fn worker_via_shared_daemon_semaphore_caps_concurrent_spawns() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let dispatcher = spawn_dispatcher_with_permits(&boot, 1);
    assert_eq!(dispatcher.permits(), 1);
    let sem = dispatcher.semaphore();
    let held_permit = sem.clone().acquire_owned().await.unwrap();

    for i in 0..2 {
        plan_codex_task(&boot, &format!("shared-cap-{i}"), "cap shared worker").await;
    }

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        worker_card_count_with_prefix(&boot, boot.wave_id.as_str()).await,
        0,
        "shared workers must wait while the only permit is occupied"
    );
    assert_eq!(sem.available_permits(), 0);

    drop(held_permit);

    wait_for(Duration::from_secs(10), || async {
        (worker_card_count_with_prefix(&boot, boot.wave_id.as_str()).await >= 1).then_some(())
    })
    .await
    .expect("a queued shared worker should mint after the permit is released");
}

#[tokio::test]
async fn worker_via_shared_daemon_writes_runtime_and_projects_thread_id() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);
    let key = "shared-worker-1";
    let idempotency_key = task_id(&boot, key);
    plan_codex_task(&boot, key, "do shared worker thing").await;
    let card = wait_for(Duration::from_secs(5), || async {
        let mut cards = boot
            .repo
            .cards_by_wave(boot.wave_id.as_str())
            .await
            .unwrap();
        project_runtime_into_cards_payload(boot.repo.as_ref(), &mut cards)
            .await
            .unwrap();
        cards.into_iter().find(|c| {
            c.payload.get("idempotency_key").and_then(Value::as_str)
                == Some(idempotency_key.as_str())
                && c.payload.get("codex_thread_id").and_then(Value::as_str)
                    == Some("fake-thread-0001")
        })
    })
    .await
    .expect("shared worker card");
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    assert!(card.payload.get("codex_source").is_none());
    assert_eq!(card.payload["codex_thread_id"], "fake-thread-0001");
    assert_eq!(card.payload["appserver_sock"], boot.shared.remote_uri());
    assert!(card.payload.get("appserver_pgid").is_none());
    // Use projectable (broadened to include terminal-status rows) so the
    // assertion is robust to CI-only timing where the codex TUI fixture
    // exits quickly → attach_reader marks runtime Exited before this read.
    let runtime = boot
        .repo
        .runtime_get_projectable_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(runtime.kind, RuntimeKind::CodexCard);
    assert_eq!(runtime.thread_id.as_deref(), Some("fake-thread-0001"));
    let terminal_id = card.payload["terminal_id"].as_str().unwrap();
    let entry = wait_for(Duration::from_secs(3), || async {
        boot.renderer.get(terminal_id)
    })
    .await
    .expect("renderer entry");
    let shell_line = &entry.config().args[1];
    assert!(
        shell_line.contains("codex resume 'fake-thread-0001' --remote 'unix://"),
        "shared worker TUI must resume the shared thread: {shell_line}"
    );
    assert!(
        !shell_line.contains("do shared worker thing"),
        "shared worker TUI argv must not carry the positional prompt: {shell_line}"
    );
    let envs = entry.config().envs.to_vec();
    assert!(
        envs.iter()
            .any(|(k, v)| k == "CODEX_HOME" && v == &boot.shared.status_snapshot().codex_home),
        "shared worker TUI env must use shared CODEX_HOME: {envs:?}"
    );
    let rows = wait_for_requests(&capture_file, 3).await;
    assert!(
        rows.iter()
            .any(|row| row.get("method").and_then(Value::as_str) == Some("turn/start")),
        "shared daemon should receive turn/start: {rows:?}"
    );
    // The shared worker must be started with the Worker-role developer
    // instructions — otherwise the agent on the shared daemon behaves like
    // a plain prompt session and skips the neige task reporting contract.
    // Assert thread/start carried them.
    let thread_start = rows
        .iter()
        .find(|row| row.get("method").and_then(Value::as_str) == Some("thread/start"))
        .expect("shared daemon should receive thread/start");
    let developer_instructions = thread_start
        .pointer("/params/developerInstructions")
        .and_then(Value::as_str)
        .or_else(|| {
            thread_start
                .pointer("/params/developer_instructions")
                .and_then(Value::as_str)
        })
        .expect("thread/start params must carry developer_instructions");
    assert!(
        developer_instructions.contains("worker agent under spec card"),
        "developer_instructions must be the Worker prompt: {developer_instructions}"
    );
    assert!(
        developer_instructions.contains("neige task-completed"),
        "developer_instructions must include the task reporting contract: {developer_instructions}"
    );
}

#[tokio::test]
async fn worker_shared_daemon_stopped_rolls_back_card() {
    // ENV_LOCK protects against env-var pollution from concurrent tests
    // (FAKE_CODEX_CAPTURE_REQUESTS / FAKE_CODEX_PTY_FAIL / etc) that would
    // affect the fake daemon and the renderer-entry expectation here.
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(false).await;
    let _dispatcher = spawn_dispatcher(&boot);
    let mut rx = boot.events.subscribe();
    let key = "shared-stopped-1";
    let idempotency_key = task_id(&boot, key);
    plan_codex_task(&boot, key, "shared daemon stopped").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let env = rx.recv().await.unwrap();
            if let Event::TaskFailed {
                idempotency_key, ..
            } = env.event
                && idempotency_key == task_id(&boot, key)
            {
                break;
            }
        }
    })
    .await
    .expect("task.failed");

    let cards = boot
        .repo
        .cards_by_wave(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(
        cards.iter().all(|card| {
            card.payload.get("idempotency_key").and_then(Value::as_str)
                != Some(idempotency_key.as_str())
        }),
        "failed shared worker spawn must roll back orphan worker card"
    );
}

#[tokio::test]
async fn worker_turn_start_failure_rolls_back_mapping_and_payload() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_TURN_START", "1");
    }
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);
    let mut rx = boot.events.subscribe();
    let key = "turn-fail-1";
    let idempotency_key = task_id(&boot, key);
    plan_codex_task(&boot, key, "turn start should fail").await;
    let failed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let env = rx.recv().await.unwrap();
            if let Event::TaskFailed {
                idempotency_key, ..
            } = env.event
                && idempotency_key == task_id(&boot, key)
            {
                break;
            }
        }
    })
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_TURN_START");
    }
    failed.expect("task.failed");

    // Shared-worker turn_start failure runs worker compensation, which
    // deletes the card + terminal rows entirely.
    // The card with idempotency_key="turn-fail-1" should not exist anywhere
    // (cards_by_wave returns no row with that key). This clears the
    // card-payload idempotency key so no orphaned worker row remains.
    // We poll briefly because the dispatcher's rollback happens async after
    // task.failed is emitted.
    let leftover = wait_for(Duration::from_secs(2), || async {
        let cards = boot
            .repo
            .cards_by_wave(boot.wave_id.as_str())
            .await
            .unwrap();
        let any_left = cards.into_iter().any(|c| {
            c.payload.get("idempotency_key").and_then(Value::as_str)
                == Some(idempotency_key.as_str())
        });
        if any_left { None } else { Some(()) }
    })
    .await;
    assert!(
        leftover.is_some(),
        "turn_start rollback must delete the worker card row so idempotency_key clears for retry"
    );
}

#[tokio::test]
async fn worker_spawn_fail_after_turn_start_interrupts_turn() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
        std::env::set_var("FAKE_CODEX_PTY_FAIL", "1");
    }
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);
    let mut rx = boot.events.subscribe();
    let key = "pty-fail-1";
    let idempotency_key = task_id(&boot, key);
    plan_codex_task(&boot, key, "turn starts but pty fails").await;
    let failed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let env = rx.recv().await.unwrap();
            if let Event::TaskFailed {
                idempotency_key, ..
            } = env.event
                && idempotency_key == task_id(&boot, key)
            {
                break;
            }
        }
    })
    .await;
    let rows = wait_for_requests(&capture_file, 4).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
        std::env::remove_var("FAKE_CODEX_PTY_FAIL");
    }
    failed.expect("task.failed");

    assert!(
        rows.iter().any(|row| {
            row.get("method").and_then(Value::as_str) == Some("turn/interrupt")
                && row.pointer("/params/threadId").and_then(Value::as_str)
                    == Some("fake-thread-0001")
                && row.pointer("/params/turnId").and_then(Value::as_str) == Some("fake-turn-0001")
        }),
        "worker PTY spawn failure must interrupt the in-flight shared turn: {rows:?}"
    );
    let leftover = wait_for(Duration::from_secs(2), || async {
        let cards = boot
            .repo
            .cards_by_wave(boot.wave_id.as_str())
            .await
            .unwrap();
        let any_left = cards.into_iter().any(|c| {
            c.payload.get("idempotency_key").and_then(Value::as_str)
                == Some(idempotency_key.as_str())
        });
        if any_left { None } else { Some(()) }
    })
    .await;
    assert!(
        leftover.is_some(),
        "PTY spawn rollback must delete the worker card row so idempotency_key clears for retry"
    );
}

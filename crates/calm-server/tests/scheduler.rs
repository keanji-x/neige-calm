//! Issue #644 PR-B — kernel scheduler integration coverage.
//!
//! Boots an in-memory `SqlxRepo` + `EventBus` + pre-seeded role caches,
//! a real `OperationRuntime` with stub worker adapters (CI cannot spawn
//! real codex terminals — see project CI limits), and a `Scheduler`
//! built exactly like the dispatcher construction site builds it.
//!
//! Coverage map (design § → test):
//!   §5.2 ready set/budget/lifecycle — `budget_holds_second_task_until_first_done`,
//!     `draft_wave_is_not_scheduled`, plus the pure-fn unit tests in
//!     `scheduler.rs`.
//!   §5.4 claim tx + dispatch — `plan_to_done_end_to_end` (claim event
//!     actor/kind, Dispatching→Working promotion, running stamp).
//!   §5.5 claim race — `claim_race_two_schedulers_single_winner`.
//!   §3 fast-report race — `fast_worker_report_beats_running_stamp`.
//!   §5.4 spawn failure — `spawn_failure_marks_failed_and_emits_kernel_task_failed`.
//!   §3 emit-tx flips — `worker_report_flips_row_inside_emit_tx`,
//!     `duplicate_report_is_idempotent`,
//!     `gated_success_report_flips_to_verifying_and_suppresses_promotion`.
//!   §3 verdict isolation — `spec_verdict_never_flips_rows`.
//!   M2 live path — `terminal_hook_completes_task_on_exit`.
//!   §8 sweep arms — `sweep_reconciles_running_terminal_with_recorded_exit`,
//!     `sweep_resubmits_dispatched_task_with_missing_operation`.

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_codex_create_tx, session_start_runtime_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::error::Result as CalmResult;
use calm_server::event::{EditAuthor, Event, EventBus};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::emit::{TOOL_TASK_COMPLETE, TOOL_TASK_FAIL};
use calm_server::mcp_server::tools::wave_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::wave_report_blocks::{
    TOOL_REPORT_BLOCKS_UPSERT, TOOL_REPORT_WRITE_MARKDOWN,
};
use calm_server::mcp_server::tools::wave_state::TOOL_TASK_VERDICT;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{
    CardRole, NewCard, NewCove, NewTerminal, NewWave, RequestTheme, Task, TaskKind, TaskStatus,
    WaveLifecycle, WavePatch, new_id, now_ms,
};
use calm_server::operation::child_wave_adapter::ChildWaveAdapter;
use calm_server::operation::claude_adapter::ClaudeWorkerAdapter;
use calm_server::operation::codex_adapter::CodexWorkerAdapter;
use calm_server::operation::task_verify_adapter::{TaskVerifyAdapter, TaskVerifyOperationPayload};
use calm_server::operation::terminal_adapter::TerminalWorkerAdapter;
use calm_server::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, NON_TASK_BOUND_ADAPTER_KINDS, Operation,
    OperationCompletionBus, OperationKey, OperationOutcome, OperationRepo, OperationRuntime,
    ParkedOutcome, PhaseTag, ProviderAdapter, SpawnArtifacts, SpawnCtx, SpawnHandle, SpawnOutcome,
    SqlxOperationRepo, TASK_BOUND_ADAPTER_KINDS, Tx, TxOutput, complete_parked_for_test,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::terminal_cards::stable_payload_hash;
use calm_server::scheduler::{
    ClaimFenceTestHook, PostClaimDriveTestHook, Scheduler, TerminalTaskHook,
    build_child_wave_payload, build_worker_payload,
};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::task_context::{ResolveError, TaskContextMonitor};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_server::wave_report::{persist_report, resolve_report_for_wave, tasks_rebuild_tx};
use calm_types::event::TaskContextRef;
use calm_types::report_blocks::render_fence;
use calm_types::wave_report::{ReportBlock, WaveReportPayload};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct Boot {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    /// Same cache instance `write` wraps — kept so tests can register
    /// extra worker cards minted mid-test.
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
}

async fn boot() -> Boot {
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "scheduler-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "scheduler-test".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "spec".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();

    // PR-C activated rule 6 and new waves default `require_task_gates
    // = 1` (migration 0041 DB DEFAULT) — this suite mostly plans
    // ungated tasks, so the boot wave opts out; gate-specific tests
    // declare real gates regardless of the flag.
    repo.wave_update(
        wave.id.as_str(),
        WavePatch {
            require_task_gates: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("boot wave opts out of rule 6");

    let events = EventBus::new();
    let shared_codex_appserver =
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());
    seed_runtime_session(
        &sqlx_repo,
        spec_card.id.as_str(),
        "spec-session",
        "spec-thread",
    )
    .await;
    sqlx::query("UPDATE waves SET root_session_id = 'spec-session' WHERE id = ?1")
        .bind(wave.id.as_str())
        .execute(sqlx_repo.pool())
        .await
        .expect("mark spec session as wave root");
    seed_runtime_session(
        &sqlx_repo,
        worker_card.id.as_str(),
        "worker-session",
        "worker-thread",
    )
    .await;
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs: repo
            .sqlite_pool()
            .map(calm_truth::wave_vcs_repo::SqlxWaveVcsRepo::shared),
        events: events.clone(),
        write: write.clone(),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        repo,
        events,
        write,
        card_role_cache,
        wave_cove_cache,
        ctx,
        registry: Arc::new(registry),
        cove_id: cove.id,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
        worker_card_id: worker_card.id,
        shared_codex_appserver,
    }
}

async fn seed_runtime_session(repo: &SqlxRepo, card_id: &str, session_id: &str, thread_id: &str) {
    seed_runtime_session_in_pool(repo.pool(), card_id, session_id, thread_id).await;
}

fn app_state_for_context_events(boot: &Boot) -> AppState {
    AppState::from_parts(
        boot.repo.clone(),
        boot.events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            boot.repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-context-events"),
            Vec::new(),
            EventBus::new(),
            boot.write.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(boot.card_role_cache.clone()),
        Some(boot.wave_cove_cache.clone()),
    )
}

#[tokio::test]
async fn dispatcher_health_monitor_shares_scheduler_context_metrics() {
    let boot = boot().await;
    let state = app_state_for_context_events(&boot);
    assert!(
        Arc::ptr_eq(
            &state.dispatcher.scheduler().context_metrics(),
            &state.dispatcher.context_monitor().metrics(),
        ),
        "the Dispatcher health exporter must share the scheduler claim metrics Arc"
    );
}

async fn seed_runtime_session_in_pool(
    pool: &sqlx::SqlitePool,
    card_id: &str,
    session_id: &str,
    thread_id: &str,
) {
    let mut tx = pool.begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session_id.to_string(),
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
    tx.commit().await.unwrap();
}

/// Build a real `OperationRuntime` over the boot repo with the supplied
/// stub adapters, plus a `Scheduler` wired exactly like the dispatcher
/// construction site wires it (Weak runtime + shared semaphore).
fn build_scheduler(
    boot: &Boot,
    adapters: Vec<Arc<dyn ProviderAdapter>>,
) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
    build_scheduler_with_semaphore(boot, adapters, Arc::new(tokio::sync::Semaphore::new(8)))
}

/// `build_scheduler` with a caller-owned dispatch semaphore — the F2/F4
/// race tests hold its only permit to park a scheduling pass inside
/// `dispatch_task`, deterministically widening the snapshot → claim
/// window.
fn build_scheduler_with_semaphore(
    boot: &Boot,
    adapters: Vec<Arc<dyn ProviderAdapter>>,
    semaphore: Arc<tokio::sync::Semaphore>,
) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
    let (runtime, scheduler) = build_scheduler_unbooted(boot, adapters, semaphore);
    // Production opens the boot gate via the `scheduler_sweep_on_boot`
    // funnel; these tests model the post-boot steady state so backstop
    // sweeps run for real (round-3 review F2).
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    (runtime, scheduler)
}

fn build_scheduler_with_timeouts(
    boot: &Boot,
    adapters: Vec<Arc<dyn ProviderAdapter>>,
    task_run_timeout: std::time::Duration,
) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
    let (runtime, scheduler) = build_scheduler_unbooted_with_timeouts(
        boot,
        adapters,
        Arc::new(tokio::sync::Semaphore::new(8)),
        Some(task_run_timeout),
    );
    // Production opens the boot gate via the `scheduler_sweep_on_boot`
    // funnel; these tests model the post-boot steady state so backstop
    // sweeps run for real (round-3 review F2).
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    (runtime, scheduler)
}

/// `build_scheduler_with_semaphore` WITHOUT opening the boot gate —
/// the dispatcher-built scheduler's state before `main` runs
/// `recover_operations_on_boot` → `scheduler_sweep_on_boot` (round-3
/// review F2).
fn build_scheduler_unbooted(
    boot: &Boot,
    adapters: Vec<Arc<dyn ProviderAdapter>>,
    semaphore: Arc<tokio::sync::Semaphore>,
) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
    build_scheduler_unbooted_with_timeouts(boot, adapters, semaphore, None)
}

fn build_scheduler_unbooted_with_timeouts(
    boot: &Boot,
    adapters: Vec<Arc<dyn ProviderAdapter>>,
    semaphore: Arc<tokio::sync::Semaphore>,
    task_run_timeout: Option<std::time::Duration>,
) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
    let operation_repo = Arc::new(SqlxOperationRepo::new(
        boot.repo
            .sqlite_pool()
            .expect("scheduler test uses sqlite repo"),
    ));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let completion = OperationCompletionBus::new();
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        Arc::new(DaemonClient {
            data_dir: std::path::PathBuf::from("/tmp/neige-scheduler-test-noop"),
            proc_supervisor_sock: Some(std::path::PathBuf::from(
                "/tmp/neige-scheduler-test-missing.sock",
            )),
        }),
        TerminalRendererRegistry::new(),
        boot.events.clone(),
        completion.clone(),
    )
    .with_shared_codex_appserver(boot.shared_codex_appserver.clone());
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        adapters,
        boot.events.clone(),
        completion,
        spawn_ctx,
    ));
    let scheduler = if let Some(task_run_timeout) = task_run_timeout {
        Scheduler::new_with_timeouts_for_test(
            boot.repo.clone(),
            boot.events.clone(),
            boot.write.clone(),
            Arc::downgrade(&runtime),
            semaphore,
            task_run_timeout,
        )
    } else {
        Scheduler::new(
            boot.repo.clone(),
            boot.events.clone(),
            boot.write.clone(),
            Arc::downgrade(&runtime),
            semaphore,
        )
    };
    (runtime, scheduler)
}

fn plan_task(wave_id: &WaveId, key: &str, kind: TaskKind, deps: &[&str]) -> Task {
    let now = now_ms();
    Task {
        id: format!("{}:{key}", wave_id.as_str()),
        wave_id: wave_id.as_str().to_string(),
        key: key.into(),
        kind,
        goal: match kind {
            TaskKind::Codex | TaskKind::Claude => format!("do {key}"),
            TaskKind::Terminal => "true".into(),
        },
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: serde_json::to_string(deps).unwrap(),
        priority: 0,
        gate_json: None,
        status: TaskStatus::Pending,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    }
}

async fn seed_task(boot: &Boot, task: Task) {
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        r#"INSERT INTO tasks
           (id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,
            depends_on_json,priority,gate_json,status,status_detail,worker_card_id,
            gate_result_json,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,
            running_deadline_ms,spawn,created_at_ms,updated_at_ms,finished_at_ms)
           VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
                  ?18,?19,?20,?21,?22,?23,?24)"#,
    )
    .bind(task.id)
    .bind(task.wave_id)
    .bind(task.key)
    .bind(task.kind)
    .bind(task.goal)
    .bind(task.context_json)
    .bind(task.acceptance_criteria)
    .bind(task.cwd)
    .bind(task.depends_on_json)
    .bind(task.priority)
    .bind(task.gate_json)
    .bind(task.status)
    .bind(task.status_detail)
    .bind(task.worker_card_id)
    .bind(task.gate_result_json)
    .bind(task.gate_attempt)
    .bind(task.gate_pid)
    .bind(task.gate_pid_starttime)
    .bind(task.gate_pid_boot_id)
    .bind(task.running_deadline_ms)
    .bind(task.spawn)
    .bind(task.created_at_ms)
    .bind(task.updated_at_ms)
    .bind(task.finished_at_ms)
    .execute(&pool)
    .await
    .expect("seed task row");
}

async fn seed_projected_task(boot: &Boot, task: Task) {
    let pool = boot.repo.sqlite_pool().unwrap();
    let report_exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE wave_id=?1 AND kind='wave-report'")
            .bind(boot.wave_id.as_str())
            .fetch_one(&pool)
            .await
            .expect("check report fixture");
    if report_exists == 0 {
        sqlx::query(
            "INSERT INTO cards(id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
             VALUES(?1,?2,'wave-report',-1,?3,'reportcard',0,1,1)",
        )
        .bind(new_id())
        .bind(boot.wave_id.as_str())
        .bind(serde_json::to_string(&WaveReportPayload::initial()).unwrap())
        .execute(&pool)
        .await
        .expect("seed report fixture");
    }
    let report = call_tool(boot, TOOL_REPORT_READ, spec_identity(boot), json!({}))
        .await
        .expect("read report before task projection");
    let mut payload = serde_json::Map::from_iter([
        ("key".into(), json!(task.key)),
        ("kind".into(), json!(task.kind)),
        ("goal".into(), json!(task.goal)),
        (
            "context".into(),
            serde_json::from_str(&task.context_json).expect("task context JSON"),
        ),
        (
            "depends_on".into(),
            serde_json::from_str(&task.depends_on_json).expect("task dependencies JSON"),
        ),
        ("priority".into(), json!(task.priority)),
        ("declared_by".into(), json!(task.declared_by)),
        ("spawn".into(), json!(task.spawn)),
        ("ready".into(), json!(true)),
    ]);
    for (key, value) in [
        ("acceptance", task.acceptance_criteria.as_ref()),
        ("cwd", task.cwd.as_ref()),
    ] {
        if let Some(value) = value {
            payload.insert(key.into(), json!(value));
        }
    }
    if let Some(gate) = task.gate_json.as_deref() {
        payload.insert(
            "gate".into(),
            serde_json::from_str(gate).expect("task gate JSON"),
        );
    }
    call_tool(
        boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(boot),
        json!({
            "kind": "task",
            "payload": payload,
            "if_doc_rev": report["docRev"]
        }),
    )
    .await
    .expect("seed task through production projection");
    sqlx::query(
        "UPDATE tasks SET status=?1,status_detail=?2,worker_card_id=?3,gate_result_json=?4,\
         gate_attempt=?5,gate_pid=?6,gate_pid_starttime=?7,gate_pid_boot_id=?8,\
         running_deadline_ms=?9,context_stale_at_ms=?10,created_at_ms=?11,updated_at_ms=?12,\
         finished_at_ms=?13 WHERE id=?14",
    )
    .bind(task.status)
    .bind(task.status_detail)
    .bind(task.worker_card_id)
    .bind(task.gate_result_json)
    .bind(task.gate_attempt)
    .bind(task.gate_pid)
    .bind(task.gate_pid_starttime)
    .bind(task.gate_pid_boot_id)
    .bind(task.running_deadline_ms)
    .bind(task.context_stale_at_ms)
    .bind(task.created_at_ms)
    .bind(task.updated_at_ms)
    .bind(task.finished_at_ms)
    .bind(task.id)
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .expect("restore projected task runtime state");
}

async fn set_lifecycle(boot: &Boot, lifecycle: WaveLifecycle) {
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(lifecycle),
                ..Default::default()
            },
        )
        .await
        .expect("set wave lifecycle");
}

async fn task_row(boot: &Boot, key: &str) -> Task {
    boot.repo
        .task_get(&format!("{}:{key}", boot.wave_id.as_str()))
        .await
        .expect("task_get")
        .expect("task row exists")
}

async fn mark_context_stale(boot: &Boot, task_id: &str) {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query("UPDATE tasks SET context_stale_at_ms = ?1 WHERE id = ?2")
        .bind(now_ms())
        .bind(task_id)
        .execute(&pool)
        .await
        .expect("mark task context stale");
}

async fn seed_codex_worker_card_with_terminal(
    boot: &Boot,
    label: &str,
) -> (String, String, String) {
    let card_id = format!("card-timeout-{label}-{}", new_id());
    let runtime_id = format!("runtime-timeout-{label}-{}", new_id());
    let wave_id = boot.wave_id.clone();
    let card_role_cache = boot.card_role_cache.clone();
    let result = calm_server::db::write_in_tx_typed(boot.repo.as_ref(), {
        let card_id = card_id.clone();
        let runtime_id = runtime_id.clone();
        move |tx| {
            Box::pin(async move {
                let (_card, term, _mcp_token) = card_with_codex_create_tx(
                    tx,
                    card_id,
                    &runtime_id,
                    None,
                    wave_id,
                    None,
                    None,
                    "/tmp".to_string(),
                    json!({}),
                    None,
                    None,
                    None,
                    CardRole::Worker,
                    true,
                    &card_role_cache,
                    RequestTheme::default_dark(),
                )
                .await?;
                Ok(term.id)
            })
        }
    })
    .await
    .expect("seed codex worker card");
    boot.repo
        .session_projection_set_status_for_card(&card_id, WorkerSessionState::Running)
        .await
        .expect("mark seeded worker runtime running");
    (card_id, runtime_id, result)
}

async fn seed_held_workspace_lease(boot: &Boot, card_id: &str, label: &str) -> String {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let lease_id = format!("lease-timeout-{label}-{}", new_id());
    let path = std::env::temp_dir().join(format!("neige-timeout-lease-{label}-{}", new_id()));
    std::fs::create_dir_all(&path).expect("create lease dir");
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, wave_id, path, state, lease_owner, lease_until_ms,
               boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', 'test-owner', ?5, NULL, ?6, ?6)"#,
    )
    .bind(&lease_id)
    .bind(card_id)
    .bind(boot.wave_id.as_str())
    .bind(path.display().to_string())
    .bind(now + 60_000)
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert held workspace lease");
    lease_id
}

async fn seed_active_codex_turn(boot: &Boot, runtime_id: &str, thread_id: &str, turn_id: &str) {
    sqlx::query(
        "UPDATE worker_sessions \
         SET thread_id = ?1, active_turn_id = ?2, updated_at_ms = ?3 \
         WHERE id = ?4",
    )
    .bind(thread_id)
    .bind(turn_id)
    .bind(now_ms())
    .bind(runtime_id)
    .execute(&boot.repo.sqlite_pool().expect("sqlite pool"))
    .await
    .expect("seed active codex turn");
    boot.shared_codex_appserver
        .set_active_turn_for_test(thread_id, turn_id);
}

async fn workspace_lease_state(boot: &Boot, lease_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_one(&boot.repo.sqlite_pool().expect("sqlite pool"))
        .await
        .expect("workspace lease state")
}

async fn workspace_lease_path(boot: &Boot, lease_id: &str) -> String {
    sqlx::query_scalar("SELECT path FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_one(&boot.repo.sqlite_pool().expect("sqlite pool"))
        .await
        .expect("workspace lease path")
}

async fn timeout_cleanup_marker_exists(boot: &Boot, card_id: &str) -> bool {
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
           FROM worker_sessions
           WHERE card_id = ?1
             AND json_extract(handle_state_json, '$.timeout_cleanup.requested_at_ms')
                 IS NOT NULL"#,
    )
    .bind(card_id)
    .fetch_one(&boot.repo.sqlite_pool().expect("sqlite pool"))
    .await
    .expect("timeout cleanup marker query");
    count > 0
}

async fn call_tool(
    boot: &Boot,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args).await
}

/// Stamp the boot worker card's payload `idempotency_key` to `task_id`
/// — the binding every scheduler-spawned worker card carries from
/// `prepare_tx`. Round-4 review F1: this payload binding is mutable
/// (`PATCH /api/cards/{id}`) and therefore NOT the ownership proof —
/// it only lets the live exit hook and the emit handlers FIND the task.
/// Tests that exercise unstamped-row reports must also seed the real
/// proof via [`seed_worker_op_target`].
async fn bind_worker_card_payload(boot: &Boot, task_id: &str) {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query("UPDATE cards SET payload = ?1 WHERE id = ?2")
        .bind(json!({ "idempotency_key": task_id }).to_string())
        .bind(boot.worker_card_id.as_str())
        .execute(&pool)
        .await
        .expect("bind worker card payload");
}

/// Seed the worker-spawn operation row whose immutable target binds
/// `card_id` to `task_id` — the shape production leaves behind after
/// `prepare_tx_and_advance` (op inserted under
/// `(kind, idempotency_key = task id)`, then `target_type = 'card'` /
/// `target_id` stamped in the same tx that creates the worker card).
/// Round-4 review F1/F2: this op target — not the patchable card
/// payload — is the unstamped-row ownership proof. Round-5 review F2:
/// the payload carries the production scheduler actor
/// (`ActorId::KernelDispatcher`, exactly what `build_worker_payload`
/// stamps) — the proof also requires the op to be scheduler-created.
async fn seed_worker_op_target(boot: &Boot, kind: &str, task_id: &str, card_id: &str) {
    seed_worker_op_target_with_payload(
        boot,
        kind,
        task_id,
        card_id,
        json!({
            "actor": ActorId::KernelDispatcher,
            "wave_id": boot.wave_id.as_str()
        }),
    )
    .await;
}

/// [`seed_worker_op_target`] with a caller-supplied persisted payload —
/// the round-5 F2 legacy-actor test seeds a `calm.task.dispatch`-shaped
/// op (actor = the requesting spec card) under the task's idempotency
/// key to prove it does NOT count as ownership.
async fn seed_worker_op_target_with_payload(
    boot: &Boot,
    kind: &str,
    task_id: &str,
    card_id: &str,
    payload: Value,
) {
    let op_repo = SqlxOperationRepo::new(boot.repo.sqlite_pool().expect("sqlite pool"));
    op_repo
        .insert_operation(
            kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task_id.to_string()),
                payload_hash: "seeded-ownership-test".into(),
            },
            payload,
        )
        .await
        .expect("seed worker op row");
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query(
        "UPDATE operations SET target_type = 'card', target_id = ?1, target_json = ?2 \
         WHERE kind = ?3 AND idempotency_key = ?4",
    )
    .bind(card_id)
    .bind(json!({ "type": "card", "id": card_id }).to_string())
    .bind(kind)
    .bind(task_id)
    .execute(&pool)
    .await
    .expect("stamp op target card");
}

fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id: "worker-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "worker-thread".into(),
    }
}

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: "spec-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "spec-thread".into(),
    }
}

/// `(kind, actor_json, payload_json)` rows from the events table —
/// actor attribution matters for the verdict classifier, so assertions
/// read the persisted column rather than the broadcast.
async fn event_rows(boot: &Boot, kind: &str) -> Vec<(String, Value)> {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT actor, payload FROM events WHERE kind = ?1 ORDER BY id ASC")
            .bind(kind)
            .fetch_all(&pool)
            .await
            .expect("events query");
    rows.into_iter()
        .map(|(actor, payload)| (actor, serde_json::from_str(&payload).expect("payload json")))
        .collect()
}

async fn operation_count(boot: &Boot, kind: &str) -> i64 {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(kind)
        .fetch_one(&pool)
        .await
        .expect("operations count");
    count
}

// ---------------------------------------------------------------------------
// Stub adapters
// ---------------------------------------------------------------------------

const STUB_PHASES: &[PhaseTag] = &[];
const BLOCKING_BOOTSTRAP_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];

fn unexpected(name: &str) -> calm_server::error::CalmError {
    calm_server::error::CalmError::Internal(format!("scheduler test stub unexpected call: {name}"))
}

/// Successful worker spawn: `prepare_tx` returns a card-shaped result
/// (the scheduler reads `result["id"]` for the `worker_card_id` stamp);
/// spawn is a no-op.
struct CardSpawnAdapter {
    kind: &'static str,
    card_id: String,
}

fn context_checked_terminal_adapter(
    boot: &Boot,
    spawned: Arc<AtomicUsize>,
) -> Arc<dyn ProviderAdapter> {
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let hook = Arc::new(move |terminal_id: String, _, _, _| {
        let spawned = Arc::clone(&spawned);
        Box::pin(async move {
            spawned.fetch_add(1, Ordering::SeqCst);
            Ok(SpawnHandle::Terminal {
                renderer_id: terminal_id.clone(),
                terminal_id,
            })
        }) as futures::future::BoxFuture<'static, CalmResult<SpawnHandle>>
    });
    Arc::new(TerminalWorkerAdapter::new_with_spawn_hook(
        route_repo,
        boot.card_role_cache.clone(),
        boot.wave_cove_cache.clone(),
        hook,
    ))
}

fn pending_operation(kind: &str, task_id: &str, payload: Value) -> Operation {
    Operation {
        id: new_id(),
        operation_key: new_id(),
        kind: kind.into(),
        idempotency_key: Some(task_id.into()),
        payload_hash: "test-hash".into(),
        target_type: "unknown".into(),
        target_id: None,
        target: json!({ "type": "unknown", "id": null }),
        payload,
        tx_output: None,
        phase: calm_server::operation::Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    }
}

#[async_trait]
impl ProviderAdapter for CardSpawnAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn phases(&self) -> &'static [PhaseTag] {
        STUB_PHASES
    }
    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        Ok(TxOutput::new(
            "card",
            Some(self.card_id.clone()),
            json!({ "id": self.card_id }),
        ))
    }
    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        _reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Err(unexpected("plan_compensation"))
    }
    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(unexpected("compensate_step"))
    }
}

struct BootstrapAdapter {
    minted: Arc<AtomicUsize>,
    block: Option<BootstrapBlockHook>,
}

#[derive(Clone)]
struct BootstrapBlockHook {
    wait_entered: Arc<tokio::sync::Notify>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl BootstrapAdapter {
    fn new(minted: Arc<AtomicUsize>) -> Self {
        Self {
            minted,
            block: None,
        }
    }

    fn new_blocking(minted: Arc<AtomicUsize>, block: BootstrapBlockHook) -> Self {
        Self {
            minted,
            block: Some(block),
        }
    }
}

#[async_trait]
impl ProviderAdapter for BootstrapAdapter {
    fn kind(&self) -> &'static str {
        "spec-harness-start"
    }
    fn phases(&self) -> &'static [PhaseTag] {
        if self.block.is_some() {
            BLOCKING_BOOTSTRAP_PHASES
        } else {
            STUB_PHASES
        }
    }
    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        let wave_id = input["wave_id"].as_str().unwrap();
        sqlx::query("UPDATE waves SET lifecycle='planning' WHERE id=?1 AND lifecycle='draft'")
            .bind(wave_id)
            .execute(&mut **tx)
            .await?;
        self.minted.fetch_add(1, Ordering::SeqCst);
        Ok(TxOutput::new(
            "wave",
            Some(wave_id.into()),
            json!({"id": wave_id}),
        ))
    }
    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        if let Some(block) = &self.block {
            ctx.record_spawn_artifacts(
                op,
                &SpawnArtifacts {
                    pid: i32::MAX,
                    pgid: i32::MAX,
                    start_time: 1,
                    boot_id: "bootstrap-block-test".into(),
                    log_path: None,
                    extra: Value::Null,
                },
            )
            .await?;
            let entered = block.entered.clone();
            let wait_entered = block.wait_entered.clone();
            let release = block.release.clone();
            let pool = ctx.operation_repo.sqlite_pool();
            let op_id = op.id.clone();
            let result = output.data.clone();
            return Ok(SpawnOutcome::Parked {
                deadline_ms: now_ms() + 10_000,
                observer: Box::pin(async move {
                    // A production wait-entry hook supplies the happens-before:
                    // no scheduler-state observation relies on a timing window.
                    wait_entered.notified().await;
                    entered.notify_one();
                    release.notified().await;
                    complete_parked_for_test(&pool, &op_id, &ParkedOutcome::Succeeded { result })
                        .await
                        .expect("complete blocked bootstrap operation");
                }),
            });
        }
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        _reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Err(unexpected("bootstrap compensation"))
    }
    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(unexpected("bootstrap compensation step"))
    }
}

/// Fast-worker-report fixture: the spawn side effect itself reports
/// `calm.task.complete` BEFORE the scheduler's `wait()` can return —
/// the §3 race, deterministically sequenced. `prepare_tx` returns the
/// card-shaped target production worker adapters return (round-4
/// review F1): the runtime stamps it as the op's immutable target
/// before `spawn_side_effect` runs, so the in-spawn report carries the
/// op-target ownership proof exactly like a real fast worker.
struct FastReportAdapter {
    kind: &'static str,
    card_id: String,
    handler: calm_server::mcp_server::registry::ToolHandler,
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    idempotency_key: String,
}

#[async_trait]
impl ProviderAdapter for FastReportAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn phases(&self) -> &'static [PhaseTag] {
        STUB_PHASES
    }
    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        Ok(TxOutput::new(
            "card",
            Some(self.card_id.clone()),
            json!({ "id": self.card_id }),
        ))
    }
    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        (self.handler)(
            self.ctx.clone(),
            self.identity.clone(),
            json!({
                "idempotency_key": self.idempotency_key.clone(),
                "result": { "ok": true }
            }),
        )
        .await
        .map_err(|e| {
            calm_server::error::CalmError::Internal(format!("fast report tool call failed: {e:?}"))
        })?;
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        _reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Err(unexpected("plan_compensation"))
    }
    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(unexpected("compensate_step"))
    }
}

/// Spawn failure fixture — `spawn_side_effect` errors, compensation is
/// empty, the operation terminates `failed`.
struct FailingSpawnAdapter {
    kind: &'static str,
}

#[async_trait]
impl ProviderAdapter for FailingSpawnAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn phases(&self) -> &'static [PhaseTag] {
        STUB_PHASES
    }
    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        Ok(TxOutput::new("failing-spawn", None, json!({ "ok": false })))
    }
    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        Err(calm_server::error::CalmError::Internal(
            "forced spawn failure".into(),
        ))
    }
    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: Vec::new(),
        })
    }
    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(unexpected("compensate_step"))
    }
}

// ---------------------------------------------------------------------------
// §5 — plan → auto-dispatch → worker completes → done (e2e, fake worker)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_to_done_end_to_end() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Dispatching).await;
    seed_projected_task(&boot, plan_task(&boot.wave_id, "t1", TaskKind::Codex, &[])).await;
    seed_projected_task(
        &boot,
        plan_task(&boot.wave_id, "t2", TaskKind::Codex, &["t1"]),
    )
    .await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;

    // t1 claimed + spawned + running-stamped; t2 dep-blocked.
    let t1 = task_row(&boot, "t1").await;
    assert_eq!(t1.status, TaskStatus::Running);
    assert_eq!(
        t1.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str()),
        "running stamp carries the op result card id"
    );
    let t2 = task_row(&boot, "t2").await;
    assert_eq!(t2.status, TaskStatus::Pending, "dep on t1 not yet done");

    // The claim record landed: actor KernelDispatcher, kind codex.
    let dispatched = event_rows(&boot, "task.dispatched").await;
    assert_eq!(dispatched.len(), 1, "one claim record for t1");
    assert!(
        dispatched[0].0.contains("KernelDispatcher"),
        "task.dispatched actor must be KernelDispatcher, got {}",
        dispatched[0].0
    );
    assert_eq!(dispatched[0].1["idempotency_key"], json!(t1.id));
    assert_eq!(dispatched[0].1["kind"], json!("codex"));

    // Dispatching → Working auto-promotion rode the claim tx.
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Working);

    // Worker reports success → emit tx flips the row to done.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": t1.id, "result": { "ok": true } }),
    )
    .await
    .expect("task complete");
    let t1 = task_row(&boot, "t1").await;
    assert_eq!(t1.status, TaskStatus::Done);
    assert!(t1.finished_at_ms.is_some());

    // The completion freed budget + satisfied t2's dep — in production
    // the task.completed envelope pokes the scheduler; drive it here.
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let t2 = task_row(&boot, "t2").await;
    assert_eq!(
        t2.status,
        TaskStatus::Running,
        "t2 dispatched once t1 is done"
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 2);
}

#[tokio::test]
async fn live_dispatch_claude_does_not_reconcile_recorded_pty_exit() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_projected_task(
        &boot,
        plan_task(&boot.wave_id, "claude-live", TaskKind::Claude, &[]),
    )
    .await;
    let terminal = boot
        .repo
        .terminal_create(NewTerminal {
            card_id: boot.worker_card_id.clone(),
            program: "claude".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("terminal row");
    boot.repo
        .terminal_set_exit(&terminal.id, Some(0), false)
        .await
        .expect("pre-record terminal exit");
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "claude-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;

    let row = task_row(&boot, "claude-live").await;
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str())
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}

// ---------------------------------------------------------------------------
// §5.2 — budget + lifecycle gating
// ---------------------------------------------------------------------------

#[tokio::test]
async fn budget_holds_second_task_until_first_done() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_projected_task(&boot, plan_task(&boot.wave_id, "a", TaskKind::Codex, &[])).await;
    seed_projected_task(&boot, plan_task(&boot.wave_id, "b", TaskKind::Codex, &[])).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    // Kernel default budget is 1 (no env override in CI): only `a` runs.
    assert_eq!(scheduler.budget_default(), 1);
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert_eq!(task_row(&boot, "a").await.status, TaskStatus::Running);
    assert_eq!(task_row(&boot, "b").await.status, TaskStatus::Pending);

    // Re-running while `a` occupies the budget changes nothing.
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert_eq!(task_row(&boot, "b").await.status, TaskStatus::Pending);

    // Per-wave override: budget 2 admits `b` (a is running, 2-1 = 1 slot).
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                task_budget: Some(Some(2)),
                ..Default::default()
            },
        )
        .await
        .expect("set wave budget");
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert_eq!(task_row(&boot, "b").await.status, TaskStatus::Running);
}

#[tokio::test]
async fn draft_wave_is_not_scheduled() {
    let boot = boot().await;
    // Wave stays Draft (the create default) — §5.2 lifecycle gate holds.
    seed_task(&boot, plan_task(&boot.wave_id, "a", TaskKind::Codex, &[])).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert_eq!(task_row(&boot, "a").await.status, TaskStatus::Pending);
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);
    assert!(event_rows(&boot, "task.dispatched").await.is_empty());
}

// ---------------------------------------------------------------------------
// §5.5 — claim race: two concurrent schedulers, one winner
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_race_two_schedulers_single_winner() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_projected_task(
        &boot,
        plan_task(&boot.wave_id, "race", TaskKind::Codex, &[]),
    )
    .await;
    // Two independent Scheduler instances (separate wave locks — the
    // per-wave mutex cannot serialize them; the claim UPDATE must).
    let (_rt1, s1) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );
    let (_rt2, s2) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    // Real race (review F8d): a multi_thread runtime + barrier release
    // both passes simultaneously on separate workers, instead of the
    // cooperative interleaving a current_thread `join!` produces.
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let h1 = tokio::spawn({
        let barrier = Arc::clone(&barrier);
        let s1 = Arc::clone(&s1);
        let w1 = boot.wave_id.clone();
        async move {
            barrier.wait().await;
            s1.schedule_wave(w1).await;
        }
    });
    let h2 = tokio::spawn({
        let barrier = Arc::clone(&barrier);
        let s2 = Arc::clone(&s2);
        let w2 = boot.wave_id.clone();
        async move {
            barrier.wait().await;
            s2.schedule_wave(w2).await;
        }
    });
    h1.await.expect("scheduler 1 task");
    h2.await.expect("scheduler 2 task");

    assert_eq!(task_row(&boot, "race").await.status, TaskStatus::Running);
    assert_eq!(
        event_rows(&boot, "task.dispatched").await.len(),
        1,
        "single-winner claim → exactly one dispatch record"
    );
    assert_eq!(
        operation_count(&boot, "codex-worker").await,
        1,
        "operations (kind, idempotency_key) unique index is the backstop"
    );
}

// ---------------------------------------------------------------------------
// §3 — fast worker report vs. the scheduler's running stamp
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fast_worker_report_beats_running_stamp() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let task = plan_task(&boot.wave_id, "fast", TaskKind::Terminal, &[]);
    let task_id = task.id.clone();
    seed_projected_task(&boot, task).await;
    // The report lands while the row is dispatched + UNSTAMPED, so the
    // reporting card must be the op's target card (round-4 F1) — the
    // FastReportAdapter's card-shaped `prepare_tx` output provides
    // that, exactly like production; the payload binding mirrors what
    // the real adapters also stamp.
    bind_worker_card_payload(&boot, &task_id).await;
    let handler = boot
        .registry
        .lookup(TOOL_TASK_COMPLETE)
        .expect("task complete tool");
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(FastReportAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
            handler,
            ctx: boot.ctx.clone(),
            identity: worker_identity(&boot),
            idempotency_key: task_id.clone(),
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;

    // The report's emit tx ran during spawn_side_effect — strictly
    // before the scheduler's wait() returned. The report flip
    // (dispatched → done) must win and the late running stamp must
    // no-op (its guard is `WHERE status = 'dispatched'`).
    let row = task_row(&boot, "fast").await;
    assert_eq!(
        row.status,
        TaskStatus::Done,
        "running stamp must never regress a reported row"
    );
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str()),
        "worker_card_id COALESCE-stamped from the report side"
    );
    assert!(row.finished_at_ms.is_some());
}

// ---------------------------------------------------------------------------
// §5.4 — spawn failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawn_failure_marks_failed_and_emits_kernel_task_failed() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let task = plan_task(&boot.wave_id, "doomed", TaskKind::Codex, &[]);
    let task_id = task.id.clone();
    seed_projected_task(&boot, task).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(FailingSpawnAdapter {
            kind: "codex-worker",
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;

    let row = task_row(&boot, "doomed").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("spawn-failed"));
    assert!(row.finished_at_ms.is_some());

    let failed = event_rows(&boot, "task.failed").await;
    assert_eq!(failed.len(), 1, "kernel task.failed pushed for the spec");
    assert!(
        failed[0].0.contains("KernelDispatcher"),
        "spawn-failure task.failed actor must be KernelDispatcher, got {}",
        failed[0].0
    );
    assert_eq!(failed[0].1["idempotency_key"], json!(task_id));
    let reason = failed[0].1["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("forced spawn failure"),
        "reason should carry the operation error, got {reason:?}"
    );

    // Working → Reviewing promotion rode the same tx.
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);
}

// ---------------------------------------------------------------------------
// §3 — emit-tx flips + guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_report_flips_row_inside_emit_tx() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "r", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    // Unstamped row → the report needs the op-target ownership proof
    // (round-4 F1); the payload binding mirrors production but is not
    // the proof.
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;

    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "reason": "could not finish" }),
    )
    .await
    .expect("task fail");

    let row = task_row(&boot, "r").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("worker-reported"));
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str()),
        "report tx stamps worker_card_id (COALESCE)"
    );
}

#[tokio::test]
async fn claude_worker_op_target_proves_unstamped_ownership() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "claude-owned", TaskKind::Claude, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target(
        &boot,
        "claude-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect("claude-worker target proves ownership");

    let row = task_row(&boot, "claude-owned").await;
    assert_eq!(row.status, TaskStatus::Done);
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str())
    );
}

#[tokio::test]
async fn duplicate_report_is_idempotent() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "dup", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": {} }),
    )
    .await
    .expect("first report");
    let first = task_row(&boot, "dup").await;
    assert_eq!(first.status, TaskStatus::Done);

    // A retried report appends another event but the guarded flip
    // no-ops — the row keeps its original terminal state + timestamps.
    // This is round-2 F3 case (iii): the row is already TERMINAL, so
    // the 0-row flip must NOT be treated as an ownership rejection —
    // the duplicate report still succeeds and still persists its event
    // (consumers tolerate duplicate task events per key, design §1.3).
    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "reason": "retry confusion" }),
    )
    .await
    .expect("second report");
    let second = task_row(&boot, "dup").await;
    assert_eq!(
        second.status,
        TaskStatus::Done,
        "terminal rows never flip again"
    );
    assert_eq!(second.finished_at_ms, first.finished_at_ms);
    assert_eq!(second.updated_at_ms, first.updated_at_ms);
    assert_eq!(
        event_rows(&boot, "task.failed").await.len(),
        1,
        "F3 case (iii): the duplicate report's event still persists"
    );
}

#[tokio::test]
async fn gated_success_report_flips_to_verifying_and_suppresses_promotion() {
    // §3 (PR-C): a gated row's success report is a claim, not
    // evidence — the emit tx hands the row to the gate runner
    // (`running → verifying`) and the `Working → Reviewing`
    // auto-promotion is suppressed (the gate-result tx promotes
    // instead, on ANY verdict).
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "gated", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.gate_json = Some(json!({ "steps": [{ "name": "t", "cmd": "true" }] }).to_string());
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": {} }),
    )
    .await
    .expect("task complete");
    let row = task_row(&boot, "gated").await;
    assert_eq!(
        row.status,
        TaskStatus::Verifying,
        "gated success report flips running → verifying, never done"
    );
    assert_eq!(row.gate_attempt, 0, "no gate attempt prepared yet");
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "Working → Reviewing promotion is suppressed for gated tasks (§3)"
    );

    // A worker `task.fail` against the now-`verifying` row is moot —
    // the verify pipeline owns it (verifying → failed only via gate
    // verdict). The legacy event persists; the row is untouched.
    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "reason": "boom" }),
    )
    .await
    .expect("task fail persists as a non-flip report");
    assert_eq!(
        task_row(&boot, "gated").await.status,
        TaskStatus::Verifying,
        "a verifying row is owned by the gate; worker reports cannot flip it"
    );
}

#[tokio::test]
async fn spec_verdict_never_flips_rows() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Reviewing).await;
    let mut task = plan_task(&boot.wave_id, "v", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // The spec records an accepted verdict — a duplicate-key
    // task.completed emission from the SPEC actor. The emit-tx hook
    // lives only in the worker-gated handlers, so the row must not move.
    call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        spec_identity(&boot),
        json!({
            "idempotency_key": task_id,
            "status": "accepted",
            "message": "looks good"
        }),
    )
    .await
    .expect("verdict");

    assert_eq!(
        task_row(&boot, "v").await.status,
        TaskStatus::Running,
        "spec verdict emissions must never flip task rows"
    );
    let completed = event_rows(&boot, "task.completed").await;
    assert_eq!(completed.len(), 1, "the verdict event itself persisted");
    assert!(
        completed[0].0.contains("AiSpec"),
        "verdict actor is the spec, got {}",
        completed[0].0
    );
}

// ---------------------------------------------------------------------------
// M2 — terminal completion: live hook + sweep arm share one guarded tx
// ---------------------------------------------------------------------------

/// Seed a terminal-worker card + terminal row wired to a plan task, the
/// shape the terminal adapter produces (payload `idempotency_key`).
async fn seed_terminal_worker(boot: &Boot, task_id: &str) -> (CardId, String) {
    let card = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({ "idempotency_key": task_id }),
        })
        .await
        .expect("terminal worker card");
    boot.card_role_cache
        .insert(card.id.clone(), CardRole::Worker, boot.wave_id.clone());
    let term = boot
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("terminal row");
    (card.id, term.id)
}

#[tokio::test]
async fn terminal_hook_completes_task_on_exit() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "term", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    // Unstamped row → exit-hook completion needs the op-target proof
    // (round-4 F2), exactly what the real spawn leaves behind.
    seed_worker_op_target(&boot, "terminal-worker", &task_id, card_id.as_str()).await;

    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    hook.on_terminal_exit(&terminal_id, Some(0), false).await;

    let row = task_row(&boot, "term").await;
    assert_eq!(row.status, TaskStatus::Done);
    assert_eq!(row.worker_card_id.as_deref(), Some(card_id.as_str()));
    let completed = event_rows(&boot, "task.completed").await;
    assert_eq!(completed.len(), 1);
    assert!(
        completed[0].0.contains("KernelDispatcher"),
        "terminal-exit completion must use the kernel actor (never a verdict), got {}",
        completed[0].0
    );
    assert_eq!(completed[0].1["idempotency_key"], json!(task_id));

    // Idempotency: a second exit delivery (or a racing sweep) no-ops —
    // no extra event, row untouched.
    hook.on_terminal_exit(&terminal_id, Some(0), false).await;
    assert_eq!(event_rows(&boot, "task.completed").await.len(), 1);
    assert_eq!(task_row(&boot, "term").await.status, TaskStatus::Done);
}

#[tokio::test]
async fn terminal_exit_beats_running_stamp() {
    // §3 fast-terminal-exit: the exit lands while the row is still
    // `dispatched` (the scheduler's `wait()` has not returned, so the
    // running stamp hasn't happened). The completion guard includes
    // `dispatched`, the hook resolves the task from the card payload's
    // `idempotency_key` (not `worker_card_id`, which is still NULL),
    // and the late running stamp must then no-op.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "fast-term", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    // Dispatched + unstamped: only the op-target proof (round-4 F2)
    // lets the exit hook win this window.
    seed_worker_op_target(&boot, "terminal-worker", &task_id, card_id.as_str()).await;

    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    hook.on_terminal_exit(&terminal_id, Some(0), false).await;

    let row = task_row(&boot, "fast-term").await;
    assert_eq!(
        row.status,
        TaskStatus::Done,
        "dispatched → done via the exit hook"
    );
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(card_id.as_str()),
        "hook stamps worker_card_id even before the scheduler could"
    );

    // The scheduler's late running stamp (guard `WHERE status =
    // 'dispatched'`) must be a no-op — it can never regress the row.
    let stamped = calm_server::db::write_in_tx_typed(boot.repo.as_ref(), {
        let task_id = task_id.clone();
        move |tx| {
            Box::pin(async move {
                let now = now_ms();
                calm_server::db::sqlite::task_mark_running_tx(
                    tx,
                    &task_id,
                    Some("late"),
                    now,
                    now + 7_200_000,
                )
                .await
            })
        }
    })
    .await
    .expect("late running stamp tx");
    assert_eq!(
        stamped, 0,
        "late running stamp must lose to the completed flip"
    );
    let row = task_row(&boot, "fast-term").await;
    assert_eq!(row.status, TaskStatus::Done);
    assert_eq!(row.worker_card_id.as_deref(), Some(card_id.as_str()));
}

#[tokio::test]
async fn terminal_hook_nonzero_exit_fails_task() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "term-fail", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    seed_worker_op_target(&boot, "terminal-worker", &task_id, card_id.as_str()).await;

    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    hook.on_terminal_exit(&terminal_id, Some(2), false).await;

    let row = task_row(&boot, "term-fail").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("worker-reported"));
    let failed = event_rows(&boot, "task.failed").await;
    assert_eq!(failed.len(), 1);
    assert!(failed[0].0.contains("KernelDispatcher"));
    let reason = failed[0].1["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("code 2"),
        "reason carries the exit code: {reason:?}"
    );
}

#[tokio::test]
async fn sweep_reconciles_running_terminal_with_recorded_exit() {
    // §8 downtime path: the exit landed while the kernel was down; the
    // boot supervisor reconcile persisted `exit_code = -1`; the sweep's
    // running-terminal arm runs the SAME guarded completion tx.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "swept", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    task.worker_card_id = Some(card_id.as_str().to_string());
    seed_task(&boot, task).await;
    boot.repo
        .terminal_set_exit(&terminal_id, Some(-1), false)
        .await
        .expect("persist synthetic exit");

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: card_id.as_str().to_string(),
        })],
    );
    scheduler.sweep_all().await;

    let row = task_row(&boot, "swept").await;
    assert_eq!(
        row.status,
        TaskStatus::Failed,
        "synthetic -1 = outcome unknown = failed"
    );
    assert_eq!(row.status_detail.as_deref(), Some("worker-reported"));
    let failed = event_rows(&boot, "task.failed").await;
    assert_eq!(failed.len(), 1);
    assert!(failed[0].0.contains("KernelDispatcher"));

    // Sweeping again is a no-op (guarded completion, first writer won).
    scheduler.sweep_all().await;
    assert_eq!(event_rows(&boot, "task.failed").await.len(), 1);
}

#[tokio::test]
async fn sweep_running_codex_past_liveness_deadline_fails_and_releases_lease_row() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let (card_id, runtime_id, _terminal_id) =
        seed_codex_worker_card_with_terminal(&boot, "expired").await;
    let lease_id = seed_held_workspace_lease(&boot, &card_id, "expired").await;
    let lease_path = workspace_lease_path(&boot, &lease_id).await;

    let mut task = plan_task(&boot.wave_id, "expired", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(now_ms() - 1);
    seed_task(&boot, task).await;

    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;

    let row = task_row(&boot, "expired").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("worker-timeout"));
    assert_eq!(
        workspace_lease_state(&boot, &lease_id).await,
        "released",
        "CAS-success timeout releases the workspace lease"
    );
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "scheduler timeout release preserves the workspace artifact"
    );
    let runtime = boot
        .repo
        .session_projection_by_id(&runtime_id)
        .await
        .expect("runtime lookup")
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Failed);
    let failed = event_rows(&boot, "task.failed").await;
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].1["reason"],
        json!("worker exceeded the running liveness deadline")
    );
}

#[tokio::test]
async fn sweep_running_codex_past_liveness_deadline_interrupts_shared_turn() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let (card_id, runtime_id, _terminal_id) =
        seed_codex_worker_card_with_terminal(&boot, "expired-turn").await;
    let lease_id = seed_held_workspace_lease(&boot, &card_id, "expired-turn").await;
    let thread_id = "thread-expired-turn";
    let turn_id = "turn-expired-turn";
    seed_active_codex_turn(&boot, &runtime_id, thread_id, turn_id).await;

    let mut task = plan_task(&boot.wave_id, "expired-turn", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(now_ms() - 1);
    seed_task(&boot, task).await;

    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;

    let row = task_row(&boot, "expired-turn").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert!(
        boot.shared_codex_appserver
            .interrupted_turns_for_test()
            .contains(&(thread_id.to_string(), turn_id.to_string())),
        "running timeout must interrupt the active shared codex turn"
    );
    assert_eq!(
        boot.shared_codex_appserver
            .active_turn_id_for_thread(thread_id),
        None
    );
    assert_eq!(workspace_lease_state(&boot, &lease_id).await, "released");
}

#[tokio::test]
async fn sweep_running_codex_timeout_cleanup_releases_row_without_touching_lease_path() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let (card_id, runtime_id, _terminal_id) =
        seed_codex_worker_card_with_terminal(&boot, "expired-retry").await;
    let lease_id = seed_held_workspace_lease(&boot, &card_id, "expired-retry").await;
    let thread_id = "thread-expired-retry";
    let turn_id = "turn-expired-retry";
    seed_active_codex_turn(&boot, &runtime_id, thread_id, turn_id).await;

    let mut task = plan_task(&boot.wave_id, "expired-retry", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(now_ms() - 1);
    seed_task(&boot, task).await;

    let lease_path = workspace_lease_path(&boot, &lease_id).await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;

    let row = task_row(&boot, "expired-retry").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("worker-timeout"));
    assert!(
        boot.shared_codex_appserver
            .interrupted_turns_for_test()
            .contains(&(thread_id.to_string(), turn_id.to_string())),
        "first attempt interrupts the active shared codex turn"
    );
    assert_eq!(
        boot.shared_codex_appserver
            .active_turn_id_for_thread(thread_id),
        None
    );
    let runtime = boot
        .repo
        .session_projection_by_id(&runtime_id)
        .await
        .expect("runtime lookup")
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Failed);
    assert_eq!(
        workspace_lease_state(&boot, &lease_id).await,
        "released",
        "timeout cleanup releases the lease row"
    );
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "timeout cleanup preserves the lease path"
    );
    assert!(
        !timeout_cleanup_marker_exists(&boot, &card_id).await,
        "cleanup marker clears once the lease row is released"
    );
    assert_eq!(event_rows(&boot, "task.failed").await.len(), 1);

    scheduler.sweep_all().await;

    assert_eq!(workspace_lease_state(&boot, &lease_id).await, "released");
    assert!(
        !timeout_cleanup_marker_exists(&boot, &card_id).await,
        "cleanup retry remains cleared after row release"
    );
    assert_eq!(
        event_rows(&boot, "task.failed").await.len(),
        1,
        "cleanup retry must not emit another task.failed"
    );
}

#[tokio::test]
async fn sweep_running_codex_timeout_cleanup_retry_treats_missing_terminal_as_reaped() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let (card_id, runtime_id, terminal_id) =
        seed_codex_worker_card_with_terminal(&boot, "expired-missing-terminal").await;
    let lease_id = seed_held_workspace_lease(&boot, &card_id, "expired-missing-terminal").await;

    let mut task = plan_task(
        &boot.wave_id,
        "expired-missing-terminal",
        TaskKind::Codex,
        &[],
    );
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(now_ms() - 1);
    seed_task(&boot, task).await;

    let lease_path = workspace_lease_path(&boot, &lease_id).await;

    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;

    let row = task_row(&boot, "expired-missing-terminal").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("worker-timeout"));
    let runtime = boot
        .repo
        .session_projection_by_id(&runtime_id)
        .await
        .expect("runtime lookup")
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Failed);
    assert_eq!(workspace_lease_state(&boot, &lease_id).await, "released");
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "timeout cleanup preserves the lease path when releasing the row"
    );
    assert!(
        !timeout_cleanup_marker_exists(&boot, &card_id).await,
        "cleanup marker clears once row release marks the lease released"
    );

    boot.repo
        .terminal_delete(&terminal_id)
        .await
        .expect("delete terminal row before retry");
    assert!(
        boot.repo
            .terminal_get_by_card(&card_id)
            .await
            .expect("terminal lookup")
            .is_none(),
        "retry fixture starts with the terminal row already gone"
    );
    scheduler.sweep_all().await;

    assert_eq!(workspace_lease_state(&boot, &lease_id).await, "released");
    assert!(
        !timeout_cleanup_marker_exists(&boot, &card_id).await,
        "missing terminal row is treated as already reaped so the marker clears"
    );
    let runtime = boot
        .repo
        .session_projection_by_id(&runtime_id)
        .await
        .expect("runtime lookup")
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Failed);
    assert_eq!(
        event_rows(&boot, "task.failed").await.len(),
        1,
        "cleanup retry must not emit another task.failed"
    );
}

#[tokio::test]
async fn sweep_running_codex_within_liveness_deadline_is_untouched() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let (card_id, runtime_id, _terminal_id) =
        seed_codex_worker_card_with_terminal(&boot, "fresh").await;
    let lease_id = seed_held_workspace_lease(&boot, &card_id, "fresh").await;

    let mut task = plan_task(&boot.wave_id, "fresh", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(now_ms() + 60_000);
    seed_task(&boot, task).await;

    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;

    let row = task_row(&boot, "fresh").await;
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(row.status_detail, None);
    assert_eq!(workspace_lease_state(&boot, &lease_id).await, "held");
    let runtime = boot
        .repo
        .session_projection_by_id(&runtime_id)
        .await
        .expect("runtime lookup")
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Running);
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}

#[tokio::test]
async fn sweep_running_terminal_past_liveness_deadline_is_not_timed_out() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "terminal-timeout", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    task.running_deadline_ms = Some(now_ms() - 1);
    seed_task(&boot, task).await;

    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;

    let row = task_row(&boot, "terminal-timeout").await;
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(row.status_detail, None);
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}

#[tokio::test]
async fn first_server_sweep_keeps_upgraded_inflight_empty_context_non_material() {
    let boot = boot().await;
    let mut task = plan_task(&boot.wave_id, "upgraded-inflight", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE tasks SET claim_context_json='[]',decl_ready=1,decl_released_by_user=0,context_verify_failures=0 WHERE id=?1",
    )
    .bind(&task_id)
    .execute(&pool)
    .await
    .unwrap();

    let monitor =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    monitor.sweep().await.unwrap();

    let stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stale, None,
        "the first production sweep must accept the migration's explicit empty frozen set"
    );
    assert!(event_rows(&boot, "task.context_advanced").await.is_empty());
}

#[tokio::test]
async fn sweep_stamps_null_running_codex_liveness_deadline_before_timing_out() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;

    let mut running = plan_task(&boot.wave_id, "upgrade-running", TaskKind::Codex, &[]);
    running.status = TaskStatus::Running;
    seed_task(&boot, running).await;

    let mut terminal_running =
        plan_task(&boot.wave_id, "terminal-running", TaskKind::Terminal, &[]);
    terminal_running.status = TaskStatus::Running;
    seed_task(&boot, terminal_running).await;

    let (_runtime, scheduler) =
        build_scheduler_with_timeouts(&boot, vec![], std::time::Duration::from_millis(70));

    let before = now_ms();
    scheduler.sweep_all().await;
    let after = now_ms();

    let running = task_row(&boot, "upgrade-running").await;
    let running_deadline = running
        .running_deadline_ms
        .expect("running sweep stamps missing deadline");
    assert_eq!(running.status, TaskStatus::Running);
    assert!(
        (before + 70..=after + 70).contains(&running_deadline),
        "running deadline {running_deadline} must use the configured timeout"
    );

    let terminal_running = task_row(&boot, "terminal-running").await;
    assert_eq!(terminal_running.status, TaskStatus::Running);
    assert_eq!(terminal_running.running_deadline_ms, None);

    tokio::time::sleep(std::time::Duration::from_millis(90)).await;
    scheduler.sweep_all().await;

    let running = task_row(&boot, "upgrade-running").await;
    assert_eq!(running.status, TaskStatus::Failed);
    assert_eq!(running.status_detail.as_deref(), Some("worker-timeout"));

    let terminal_running = task_row(&boot, "terminal-running").await;
    assert_eq!(terminal_running.status, TaskStatus::Running);
    assert_eq!(terminal_running.running_deadline_ms, None);
}

#[tokio::test]
async fn sweep_running_claude_ignores_recorded_pty_exit() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "claude-swept", TaskKind::Claude, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    task.worker_card_id = Some(card_id.as_str().to_string());
    seed_task(&boot, task).await;
    boot.repo
        .terminal_set_exit(&terminal_id, Some(0), false)
        .await
        .expect("persist synthetic exit");

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "claude-worker",
            card_id: card_id.as_str().to_string(),
        })],
    );
    scheduler.sweep_all().await;

    let row = task_row(&boot, "claude-swept").await;
    assert_eq!(
        row.status,
        TaskStatus::Running,
        "running claude tasks ignore terminal-row exit evidence"
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}

// ---------------------------------------------------------------------------
// §8 — sweep `dispatched` arm: crash between claim and operation insert
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sweep_resubmits_dispatched_task_with_missing_operation() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "orphan", TaskKind::Codex, &[]);
    // Simulate the §5.5 crash window: row claimed (`dispatched`) but the
    // worker operation was never inserted.
    task.status = TaskStatus::Dispatched;
    seed_task(&boot, task).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);

    scheduler.sweep_all().await;

    assert_eq!(
        operation_count(&boot, "codex-worker").await,
        1,
        "deterministic resubmit"
    );
    let row = task_row(&boot, "orphan").await;
    assert_eq!(
        row.status,
        TaskStatus::Running,
        "row reconciled after re-drive"
    );
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str())
    );

    // Idempotency: another sweep dedupes on (kind, idempotency_key).
    scheduler.sweep_all().await;
    assert_eq!(operation_count(&boot, "codex-worker").await, 1);
}

#[tokio::test]
async fn stale_dispatched_worker_without_operation_fails_without_spawn_or_budget_pin() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "stale-orphan", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    mark_context_stale(&boot, &task_id).await;
    let spawned = Arc::new(AtomicUsize::new(0));
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![context_checked_terminal_adapter(&boot, spawned.clone())],
    );

    scheduler.sweep_all().await;

    assert_eq!(spawned.load(Ordering::SeqCst), 0, "no worker was started");
    let row = task_row(&boot, "stale-orphan").await;
    assert_eq!(row.status, TaskStatus::Failed, "budget is no longer pinned");
    let op = runtime
        .find_by_kind_and_idempotency("terminal-worker", &task_id)
        .await
        .unwrap()
        .expect("terminal worker op");
    assert_eq!(op.phase.tag(), PhaseTag::Failed);
    assert!(
        op.last_error
            .as_deref()
            .unwrap_or_default()
            .contains("context-stale")
    );
}

#[tokio::test]
async fn every_registered_task_adapter_refuses_material_context() {
    let boot = boot().await;
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let codex: Arc<dyn ProviderAdapter> = Arc::new(CodexWorkerAdapter::new(
        route_repo.clone(),
        Arc::new(CodexClient::new_stub()),
        boot.shared_codex_appserver.clone(),
        None,
        boot.card_role_cache.clone(),
        boot.wave_cove_cache.clone(),
    ));
    let claude: Arc<dyn ProviderAdapter> = Arc::new(ClaudeWorkerAdapter::new(
        route_repo.clone(),
        Arc::new(CodexClient::new_stub()),
        None,
        boot.card_role_cache.clone(),
        boot.wave_cove_cache.clone(),
    ));
    let terminal: Arc<dyn ProviderAdapter> = Arc::new(TerminalWorkerAdapter::new(
        route_repo,
        boot.card_role_cache.clone(),
        boot.wave_cove_cache.clone(),
    ));

    let mut cases = Vec::new();
    for (key, task_kind, adapter) in [
        ("meta-codex", TaskKind::Codex, codex),
        ("meta-claude", TaskKind::Claude, claude),
        ("meta-terminal", TaskKind::Terminal, terminal),
    ] {
        let mut task = plan_task(&boot.wave_id, key, task_kind, &[]);
        task.status = TaskStatus::Dispatched;
        let task_id = task.id.clone();
        let (kind, payload) = build_worker_payload(&task).unwrap();
        seed_task(&boot, task).await;
        mark_context_stale(&boot, &task_id).await;
        cases.push((adapter, pending_operation(kind, &task_id, payload)));
    }

    let mut verify_task = plan_task(&boot.wave_id, "meta-verify", TaskKind::Codex, &[]);
    verify_task.status = TaskStatus::Verifying;
    verify_task.gate_json =
        Some(json!({"cwd":"/tmp","steps":[{"name":"legal","cmd":"true"}] }).to_string());
    let verify_task_id = verify_task.id.clone();
    seed_task(&boot, verify_task).await;
    mark_context_stale(&boot, &verify_task_id).await;
    let verify_payload = serde_json::to_value(TaskVerifyOperationPayload {
        actor: ActorId::KernelDispatcher,
        wave_id: boot.wave_id.to_string(),
        task_id: verify_task_id.clone(),
        attempt: 1,
    })
    .unwrap();
    cases.push((
        Arc::new(TaskVerifyAdapter::new(std::env::temp_dir())) as Arc<dyn ProviderAdapter>,
        pending_operation(
            "task-verify",
            &format!("{verify_task_id}#g1"),
            verify_payload,
        ),
    ));
    let mut child_task = plan_task(&boot.wave_id, "meta-child", TaskKind::Codex, &[]);
    child_task.status = TaskStatus::Dispatched;
    child_task.spawn = "sub-wave".into();
    let child_task_id = child_task.id.clone();
    let child_payload = build_child_wave_payload(&child_task).unwrap();
    seed_task(&boot, child_task).await;
    mark_context_stale(&boot, &child_task_id).await;
    cases.push((
        Arc::new(ChildWaveAdapter::new(
            boot.card_role_cache.clone(),
            boot.wave_cove_cache.clone(),
        )) as Arc<dyn ProviderAdapter>,
        pending_operation("child-wave", &child_task_id, child_payload),
    ));

    let fenced = cases
        .iter()
        .map(|(adapter, _)| adapter.kind())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        fenced,
        TASK_BOUND_ADAPTER_KINDS.into_iter().collect(),
        "task-bound classification changed; add a stale payload proof"
    );

    // The left side comes from the adapters actually built by AppState's
    // production `build_operation_adapters`, not from this test's cases.
    let state = AppState::from_parts(
        boot.repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            boot.repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-context-registry"),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let registered = state
        .operation_runtime
        .registered_adapter_kinds()
        .collect::<std::collections::BTreeSet<_>>();
    let classified = TASK_BOUND_ADAPTER_KINDS
        .into_iter()
        .chain(NON_TASK_BOUND_ADAPTER_KINDS)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        registered, classified,
        "every production adapter must be classified as task-bound or explicitly non-task-bound"
    );
    let pool = boot.repo.sqlite_pool().unwrap();
    for (adapter, op) in cases {
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        let before: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM waves), (SELECT count(*) FROM cards), \
                    (SELECT count(*) FROM terminals), (SELECT count(*) FROM worker_sessions)",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &op.payload, &op)
            .await
            .expect_err(adapter.kind());
        assert!(
            matches!(error, calm_server::error::CalmError::Conflict(ref message) if message.contains("context-stale")),
            "{} did not fail closed: {error}",
            adapter.kind()
        );
        let after: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM waves), (SELECT count(*) FROM cards), \
                    (SELECT count(*) FROM terminals), (SELECT count(*) FROM worker_sessions)",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(
            before,
            after,
            "{} wrote before its stale fence",
            adapter.kind()
        );
        tx.rollback().await.unwrap();
    }

    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    let missing = calm_server::operation::refuse_if_context_stale(
        &mut tx,
        Some("missing-task-bound-operation"),
    )
    .await
    .expect_err("a task binding whose row vanished must fail closed");
    assert!(
        matches!(missing, calm_server::error::CalmError::Conflict(ref message) if message.contains("does not exist"))
    );
    tx.rollback().await.unwrap();

    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    calm_server::operation::refuse_if_context_stale(&mut tx, None)
        .await
        .expect("an operation without a task binding is outside the task-context fence");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn stale_pending_worker_operation_is_rejected_during_boot_recovery() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "stale-pending", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    let (_, payload) = build_worker_payload(&task).unwrap();
    seed_task(&boot, task).await;
    mark_context_stale(&boot, &task_id).await;
    let spawned = Arc::new(AtomicUsize::new(0));
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![context_checked_terminal_adapter(&boot, spawned.clone())],
    );
    let repo = SqlxOperationRepo::new(boot.repo.sqlite_pool().unwrap());
    repo.insert_operation(
        "terminal-worker",
        OperationKey {
            operation_key: new_id(),
            idempotency_key: Some(task_id.clone()),
            payload_hash: stable_payload_hash(&payload).unwrap(),
        },
        payload,
    )
    .await
    .unwrap();

    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        0,
        "Pending is not already started"
    );
    let op = runtime
        .find_by_kind_and_idempotency("terminal-worker", &task_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Failed);
    assert!(
        op.last_error
            .as_deref()
            .unwrap_or_default()
            .contains("context-stale")
    );

    scheduler.sweep_all().await;
    assert_eq!(
        task_row(&boot, "stale-pending").await.status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn stale_spawn_started_worker_is_recovered_without_rechecking_context() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "stale-started", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    let (_, payload) = build_worker_payload(&task).unwrap();
    seed_task(&boot, task).await;
    let spawned = Arc::new(AtomicUsize::new(0));
    let adapter = context_checked_terminal_adapter(&boot, spawned.clone());
    let (runtime, scheduler) = build_scheduler(&boot, vec![adapter.clone()]);
    let repo = SqlxOperationRepo::new(boot.repo.sqlite_pool().unwrap());
    let op_id = repo
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task_id.clone()),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let pending_op = runtime
        .find_by_kind_and_idempotency("terminal-worker", &task_id)
        .await
        .unwrap()
        .unwrap();
    let mut prepare_tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    let output = adapter
        .prepare_tx(&mut prepare_tx, &payload, &pending_op)
        .await
        .unwrap();
    prepare_tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE operations SET phase = 'spawn_started', tx_output_json = ?1, \
         target_json = ?2 WHERE id = ?3",
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(json!({"type": "card", "id": boot.worker_card_id.as_str()}).to_string())
    .bind(&op_id)
    .execute(&pool)
    .await
    .unwrap();
    mark_context_stale(&boot, &task_id).await;

    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        1,
        "already-started terminal work resumes without re-entering prepare_tx"
    );
    let op = runtime
        .find_by_kind_and_idempotency("terminal-worker", &task_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Succeeded);
    assert!(!matches!(op.phase.tag(), PhaseTag::Failed));

    scheduler.sweep_all().await;
    assert_eq!(
        task_row(&boot, "stale-started").await.status,
        TaskStatus::Running
    );
}

#[tokio::test]
async fn later_successful_context_sweep_opens_gate_and_redrives_dispatched_same_turn() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut pending = plan_task(&boot.wave_id, "boot-pending-op", TaskKind::Terminal, &[]);
    pending.status = TaskStatus::Dispatched;
    seed_task(&boot, pending).await;
    let mut missing = plan_task(&boot.wave_id, "boot-missing-op", TaskKind::Terminal, &[]);
    missing.status = TaskStatus::Dispatched;
    seed_task(&boot, missing).await;
    sqlx::query("UPDATE tasks SET claim_context_json='[]' WHERE wave_id=?1")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let spawned = Arc::new(AtomicUsize::new(0));
    let (runtime, _) = build_scheduler_unbooted(
        &boot,
        vec![context_checked_terminal_adapter(&boot, spawned.clone())],
        Arc::new(tokio::sync::Semaphore::new(8)),
    );
    let dispatcher = Dispatcher::spawn_with_terminal_renderer_and_operation_runtime(
        boot.repo.clone(),
        boot.events.clone(),
        boot.write.clone(),
        Arc::new(CodexClient::new_stub()),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        None,
        boot.shared_codex_appserver.clone(),
        runtime.clone(),
        8,
    );
    let scheduler = dispatcher.scheduler();
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("ALTER TABLE task_ref_index RENAME TO task_ref_index_boot_failure")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        calm_server::task_context::sweep_with_timeout(
            dispatcher.context_monitor().as_ref(),
            Duration::from_secs(30),
        )
        .await
        .is_err(),
        "the real boot context sweep must fail"
    );
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    scheduler.sweep_boot().await;
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    assert_eq!(operation_count(&boot, "terminal-worker").await, 0);

    sqlx::query("ALTER TABLE task_ref_index_boot_failure RENAME TO task_ref_index")
        .execute(&pool)
        .await
        .unwrap();
    dispatcher.reconcile_tick_for_test().await;
    for _ in 0..100 {
        if spawned.load(Ordering::SeqCst) == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        operation_count(&boot, "terminal-worker").await,
        2,
        "tick operation count"
    );
    assert_eq!(spawned.load(Ordering::SeqCst), 2);
    assert_eq!(operation_count(&boot, "terminal-worker").await, 2);
    assert_eq!(
        task_row(&boot, "boot-pending-op").await.status,
        TaskStatus::Running
    );
    assert_eq!(
        task_row(&boot, "boot-missing-op").await.status,
        TaskStatus::Running
    );
}

// ---------------------------------------------------------------------------
// Review round 1 — F1: PTY exits never complete codex-kind tasks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_task_pty_exit_does_not_complete_task() {
    // Codex worker cards are terminal-row-backed too and carry the task
    // id in their payload `idempotency_key`. A codex PTY exiting 0 says
    // nothing about the task outcome — only `calm.task.complete` may
    // finish it; the live hook must kind-gate exactly like the sweep's
    // running-terminal arm.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "cx", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (_card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;

    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    hook.on_terminal_exit(&terminal_id, Some(0), false).await;
    assert_eq!(
        task_row(&boot, "cx").await.status,
        TaskStatus::Running,
        "codex task must stay running after its backing PTY exits 0"
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());

    // Non-zero exits are equally not the hook's business for codex.
    hook.on_terminal_exit(&terminal_id, Some(2), false).await;
    assert_eq!(task_row(&boot, "cx").await.status, TaskStatus::Running);
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}

#[tokio::test]
async fn claude_task_pty_exit_does_not_complete_task() {
    // Claude worker cards are PTY-backed like terminal tasks, but a PTY
    // exit is not a task verdict. Completion must come from
    // `calm.task.complete`.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "claude-exit", TaskKind::Claude, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (_card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;

    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    hook.on_terminal_exit(&terminal_id, Some(0), false).await;
    assert_eq!(
        task_row(&boot, "claude-exit").await.status,
        TaskStatus::Running,
        "claude task must stay running after its backing PTY exits 0"
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());

    hook.on_terminal_exit(&terminal_id, Some(2), false).await;
    assert_eq!(
        task_row(&boot, "claude-exit").await.status,
        TaskStatus::Running
    );
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}

// ---------------------------------------------------------------------------
// Review round 1 — F2: the dispatched payload is built from the frozen
// post-claim row, never the pre-claim snapshot
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_payload_frozen_against_pre_claim_revision() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "revise", TaskKind::Terminal, &[]);
    task.goal = "echo old".into();
    let task_id = task.id.clone();
    seed_projected_task(&boot, task).await;

    // Hold the dispatcher semaphore's only permit: the scheduling pass
    // snapshots the plan rows in `schedule_pass`, then parks inside
    // `dispatch_task` awaiting the permit — exactly the unbounded
    // snapshot → claim window the review flagged.
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&semaphore)
        .acquire_owned()
        .await
        .expect("test holds the only permit");
    let (runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        semaphore,
    );
    let handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let wave_id = boot.wave_id.clone();
        async move { scheduler.schedule_wave(wave_id).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Revise the still-pending row mid-window (pending rows are
    // mutable; post-claim they are frozen).
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let revised =
        sqlx::query("UPDATE tasks SET goal = 'echo new' WHERE id = ?1 AND status = 'pending'")
            .bind(&task_id)
            .execute(&pool)
            .await
            .expect("revise pending row");
    assert_eq!(
        revised.rows_affected(),
        1,
        "revision landed while the pass was parked pre-claim"
    );

    drop(permit);
    handle.await.expect("schedule_wave task");

    let op = runtime
        .find_by_kind_and_idempotency("terminal-worker", &task_id)
        .await
        .expect("op lookup")
        .expect("worker op row");
    assert_eq!(
        op.payload["cmd"],
        json!("echo new"),
        "payload must reflect the claimed (frozen) row, not the pre-claim snapshot"
    );
    assert_eq!(task_row(&boot, "revise").await.status, TaskStatus::Running);
}

#[tokio::test]
async fn block_task_production_claim_freezes_nonempty_root_context() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let key = "block-freeze";
    let task = plan_task(&boot.wave_id, key, TaskKind::Terminal, &[]);
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 7,
        summary: String::new(),
        body: "# Task\n".into(),
        blocks: Some(vec![ReportBlock {
            id: "b_task_root".into(),
            kind: "task".into(),
            rev: 1,
            payload: json!({"key": key, "kind": "terminal", "goal": "echo hi"}),
        }]),
    };
    sqlx::query(
        "INSERT INTO cards \
         (id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES (?1,?2,'wave-report',-1,?3,'reportcard',0,1,1)",
    )
    .bind(format!("report-{key}"))
    .bind(boot.wave_id.as_str())
    .bind(serde_json::to_string(&report).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;

    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id,kind FROM events \
         WHERE kind IN ('task.dispatched','task.context_frozen') ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("read dispatch batch events");
    assert_eq!(rows.len(), 2, "one dispatch must have exactly one freeze");
    assert_eq!(rows[0].1, "task.dispatched");
    assert_eq!(rows[1].1, "task.context_frozen");
    assert_eq!(rows[1].0, rows[0].0 + 1, "batch events stay adjacent");

    let context: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let refs: Vec<TaskContextRef> = serde_json::from_str(&context).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].block_id, "b_task_root");
    assert!(refs[0].is_root);
    let frozen_events = event_rows(&boot, "task.context_frozen").await;
    assert_eq!(frozen_events.len(), 1);
    assert_eq!(
        frozen_events[0].1["doc_revs"][boot.wave_id.as_str()],
        json!(7),
        "freeze event must preserve the production report's docRev fence baseline"
    );
    assert_eq!(frozen_events[0].1["truncated"], json!(false));
    assert_eq!(frozen_events[0].1["refs"][0]["is_root"], json!(true));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM task_ref_index WHERE task_id = ?1 AND block_id = 'b_task_root'",
        )
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn claim_missing_task_root_fails_closed_without_dispatch() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_task(
        &boot,
        plan_task(&boot.wave_id, "missing-root", TaskKind::Terminal, &[]),
    )
    .await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;

    assert_eq!(
        task_row(&boot, "missing-root").await.status,
        TaskStatus::Pending
    );
    assert_eq!(operation_count(&boot, "terminal-worker").await, 0);
    assert!(event_rows(&boot, "task.dispatched").await.is_empty());
}

async fn insert_report_payload(boot: &Boot, id: &str, payload: Value) {
    sqlx::query(
        "INSERT INTO cards \
         (id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES (?1,?2,'wave-report',-1,?3,'reportcard',0,1,1)",
    )
    .bind(id)
    .bind(boot.wave_id.as_str())
    .bind(payload.to_string())
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
}

async fn edit_report_blocks(boot: &Boot, blocks: &[(&str, &str, Value)], if_doc_rev: u64) {
    let body = blocks
        .iter()
        .map(|(id, kind, payload)| {
            let content = if *kind == "prose" {
                payload["markdown"].as_str().unwrap().to_string()
            } else {
                render_fence(kind, payload)
            };
            format!("<!-- neige:{id} -->\n{content}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    call_tool(
        boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(boot),
        json!({"body": body, "if_doc_rev": if_doc_rev}),
    )
    .await
    .expect("production report edit");
}

#[tokio::test]
async fn report_without_doc_rev_is_retryable_malformed_instead_of_revision_zero() {
    let boot = boot().await;
    insert_report_payload(
        &boot,
        "v2-report",
        json!({"schemaVersion": 2, "summary": "", "body": "", "blocks": []}),
    )
    .await;
    let monitor =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    assert!(matches!(
        monitor
            .resolve_task_closure(boot.wave_id.as_str(), "missing")
            .await,
        Err(ResolveError::MalformedStoredReport(_))
    ));
}

#[tokio::test]
async fn missing_blocks_is_empty_and_unrelated_malformed_block_is_ignored() {
    let boot = boot().await;
    insert_report_payload(
        &boot,
        "empty-report",
        json!({"schemaVersion": 3, "docRev": 1, "summary": "", "body": ""}),
    )
    .await;
    let monitor =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    assert!(matches!(
        monitor
            .resolve_task_closure(boot.wave_id.as_str(), "missing")
            .await,
        Err(ResolveError::RootAbsent)
    ));

    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("DELETE FROM cards WHERE id = 'empty-report'")
        .execute(&pool)
        .await
        .unwrap();
    insert_report_payload(
        &boot,
        "mixed-report",
        json!({
            "schemaVersion": 3,
            "docRev": 2,
            "summary": "",
            "body": "",
            "blocks": [
                {"id": "b_broken", "kind": 7, "rev": "bad", "payload": null},
                {"id": "b_target", "kind": "task", "rev": 1,
                 "payload": {"key": "target", "kind": "terminal", "goal": "ok"}}
            ]
        }),
    )
    .await;
    let closure = monitor
        .resolve_task_closure(boot.wave_id.as_str(), "target")
        .await
        .expect("an unrelated malformed block must not poison the target");
    assert_eq!(closure.refs[0].block_id, "b_target");
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.blocks[1].rev', 'bad') \
         WHERE id = 'mixed-report'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        monitor
            .resolve_task_closure(boot.wave_id.as_str(), "target")
            .await,
        Err(ResolveError::MalformedStoredReport(_))
    ));
}

async fn assert_claim_fence_race_lost(cross_wave: bool) {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let key = if cross_wave {
        "fence-cross"
    } else {
        "fence-same"
    };
    let mut refs = Vec::new();
    if cross_wave {
        let child = boot
            .repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: boot.cove_id.clone(),
                title: "fence child".into(),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        refs.push(format!("neige://wave/{}#b_2000", child.id));
        let child_report = WaveReportPayload {
            schema_version: WaveReportPayload::SCHEMA_VERSION,
            doc_rev: 4,
            summary: String::new(),
            body: "child".into(),
            blocks: Some(vec![ReportBlock {
                id: "b_2000".into(),
                kind: "prose".into(),
                rev: 1,
                payload: json!({"markdown": "child original"}),
            }]),
        };
        sqlx::query(
            "INSERT INTO cards \
             (id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
             VALUES ('fence-child-report',?1,'wave-report',-1,?2,'reportcard',0,1,1)",
        )
        .bind(child.id.as_str())
        .bind(serde_json::to_string(&child_report).unwrap())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    }
    let root_report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 7,
        summary: String::new(),
        body: "root".into(),
        blocks: Some(vec![ReportBlock {
            id: "b_fence_root".into(),
            kind: "task".into(),
            rev: 1,
            payload: json!({
                "key": key, "kind": "codex", "goal": "true", "refs": refs,
                "spawn": "in-wave"
            }),
        }]),
    };
    insert_report_payload(
        &boot,
        "fence-root-report",
        serde_json::to_value(root_report).unwrap(),
    )
    .await;
    let task = plan_task(&boot.wave_id, key, TaskKind::Codex, &[]);
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let resolved_closure =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone())
            .resolve_task_closure(boot.wave_id.as_str(), key)
            .await
            .expect("fence fixture closure must resolve");
    assert_eq!(resolved_closure.refs.len(), if cross_wave { 2 } else { 1 });

    let spawned = Arc::new(AtomicUsize::new(0));
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![context_checked_terminal_adapter(&boot, spawned.clone())],
    );
    let resolved = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    scheduler.set_claim_fence_test_hook(ClaimFenceTestHook {
        resolved: resolved.clone(),
        resume: resume.clone(),
    });
    let scheduled = {
        let scheduler = scheduler.clone();
        let wave_id = boot.wave_id.clone();
        tokio::spawn(async move { scheduler.schedule_wave(wave_id).await })
    };
    resolved.notified().await;
    let card_id = if cross_wave {
        "fence-child-report"
    } else {
        "fence-root-report"
    };
    let (block_path, replacement) = if cross_wave {
        ("$.blocks[0].payload.markdown", "edited after resolve")
    } else {
        ("$.blocks[0].payload.spawn", "sub-wave")
    };
    sqlx::query(&format!(
        "UPDATE cards SET payload = json_set(payload, '$.docRev', json_extract(payload, '$.docRev') + 1, '{block_path}', ?2) WHERE id = ?1"
    ))
    .bind(card_id)
    .bind(replacement)
    .execute(&pool)
    .await
    .unwrap();
    resume.notify_one();
    scheduled.await.unwrap();

    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Pending
    );
    assert!(event_rows(&boot, "task.context_frozen").await.is_empty());
    let index_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_ref_index WHERE task_id = ?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(index_rows, 0);
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    assert_eq!(scheduler.claim_fence_race_lost_count(), 1);
    assert_eq!(
        scheduler.context_metrics().snapshot().claim_fence_race_lost,
        1,
        "claim fence counter must be present in the production health snapshot"
    );
}

#[tokio::test]
async fn acceptance_2_claim_fence_rejects_spawn_edit_after_resolution_without_side_effects() {
    assert_claim_fence_race_lost(false).await;
}

#[tokio::test]
async fn claim_fence_rejects_cross_wave_edit_after_resolution_without_side_effects() {
    assert_claim_fence_race_lost(true).await;
}

#[tokio::test]
async fn deterministic_root_location_failures_do_not_freeze_or_index() {
    for case in ["duplicate", "tombstoned", "absent", "invalid-root-ref"] {
        let boot = boot().await;
        set_lifecycle(&boot, WaveLifecycle::Draft).await;
        let key = format!("root-{case}");
        let valid_root = json!({
            "key": key, "kind": "terminal", "goal": "true",
            "declared_by": "spec", "ready": true
        });
        insert_report_payload(
            &boot,
            &format!("report-{case}"),
            serde_json::to_value(WaveReportPayload {
                schema_version: WaveReportPayload::SCHEMA_VERSION,
                doc_rev: 0,
                summary: String::new(),
                body: String::new(),
                blocks: Some(Vec::new()),
            })
            .unwrap(),
        )
        .await;
        let root_id = "b_1001";
        let initial = vec![(root_id, "task", valid_root.clone())];
        edit_report_blocks(&boot, &initial, 0).await;
        set_lifecycle(&boot, WaveLifecycle::Draft).await;
        let task_id = format!("{}:{key}", boot.wave_id);
        assert!(
            boot.repo.task_get(&task_id).await.unwrap().is_some(),
            "initial projection for case {case}"
        );
        let (_runtime, scheduler) = build_scheduler(
            &boot,
            vec![Arc::new(CardSpawnAdapter {
                kind: "terminal-worker",
                card_id: boot.worker_card_id.to_string(),
            })],
        );
        scheduler.schedule_wave(boot.wave_id.clone()).await;

        let broken = match case {
            "duplicate" => vec![
                (root_id, "task", valid_root.clone()),
                ("b_1002", "task", valid_root.clone()),
            ],
            "tombstoned" => vec![(
                root_id,
                "task",
                json!({
                    "key": key, "declared_by": "spec", "tombstoned_by": "spec",
                    "tombstone": {"reason": "gone"}
                }),
            )],
            "absent" => vec![("b_1004", "prose", json!({"markdown": "nothing"}))],
            "invalid-root-ref" => vec![(
                root_id,
                "task",
                json!({
                    "key": key, "kind": "terminal", "goal": "true",
                    "declared_by": "spec", "ready": true,
                    "refs": ["not-a-neige-link"]
                }),
            )],
            _ => unreachable!(),
        };
        let pool = boot.repo.sqlite_pool().unwrap();
        if case == "invalid-root-ref" {
            // The public writer rejects malformed refs before persistence. Model
            // an older/corrupt stored report, then run the production rebuild
            // projection boundary; the task row itself was still created by the
            // real report edit above.
            let malformed = WaveReportPayload {
                schema_version: WaveReportPayload::SCHEMA_VERSION,
                doc_rev: 2,
                summary: String::new(),
                body: String::new(),
                blocks: Some(vec![ReportBlock {
                    id: root_id.into(),
                    kind: "task".into(),
                    rev: 2,
                    payload: broken[0].2.clone(),
                }]),
            };
            let mut tx = pool.begin().await.unwrap();
            sqlx::query(
                "UPDATE cards SET payload = ?1, body_crdt = NULL \
                 WHERE wave_id = ?2 AND kind = 'wave-report'",
            )
            .bind(serde_json::to_string(&malformed).unwrap())
            .bind(boot.wave_id.as_str())
            .execute(&mut *tx)
            .await
            .unwrap();
            tasks_rebuild_tx(&mut tx, boot.wave_id.as_str())
                .await
                .unwrap();
            tx.commit().await.unwrap();
        } else {
            edit_report_blocks(&boot, &broken, 1).await;
        }
        set_lifecycle(&boot, WaveLifecycle::Working).await;
        scheduler.schedule_wave(boot.wave_id.clone()).await;
        assert!(event_rows(&boot, "task.context_frozen").await.is_empty());
        assert!(event_rows(&boot, "task.context_advanced").await.is_empty());
        let indexed: i64 =
            sqlx::query_scalar("SELECT count(*) FROM task_ref_index WHERE task_id = ?1")
                .bind(&task_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(indexed, 0, "case {case}");
        let stale: Option<Option<i64>> =
            sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id = ?1")
                .bind(&task_id)
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(
            stale.is_none_or(|value| value.is_none()),
            "case {case}: context_stale_at_ms must remain NULL when the row survives"
        );
        assert!(
            boot.repo.task_get(&task_id).await.unwrap().is_none(),
            "case {case}: production projection must guard-delete the pending row"
        );
    }
}

#[tokio::test]
async fn depth_two_deleted_reference_is_counted_does_not_block_and_recovers_next_claim() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    insert_report_payload(
        &boot,
        "blocked-head-report",
        serde_json::to_value(WaveReportPayload {
            schema_version: WaveReportPayload::SCHEMA_VERSION,
            doc_rev: 1,
            summary: String::new(),
            body: String::new(),
            blocks: Some(vec![
                ReportBlock {
                    id: "b_head_1".into(),
                    kind: "task".into(),
                    rev: 1,
                    payload: json!({
                        "key": "blocked-head", "kind": "terminal", "goal": "true",
                        "declared_by": "spec", "ready": true,
                        "refs": [format!("neige://wave/{}#b_cafe", boot.wave_id)]
                    }),
                },
                ReportBlock {
                    id: "b_cafe".into(),
                    kind: "prose".into(),
                    rev: 1,
                    payload: json!({"markdown": format!("[leaf](neige://wave/{}#b_dead)", boot.wave_id)}),
                },
                ReportBlock {
                    id: "b_dead".into(),
                    kind: "prose".into(),
                    rev: 1,
                    payload: json!({"markdown": "present before projection edit"}),
                },
                ReportBlock {
                    id: "b_tail".into(),
                    kind: "task".into(),
                    rev: 1,
                    payload: json!({
                        "key": "healthy-tail", "kind": "terminal", "goal": "true",
                        "declared_by": "spec", "ready": true
                    }),
                },
            ]),
        })
        .unwrap(),
    )
    .await;
    let mut blocked = plan_task(&boot.wave_id, "blocked-head", TaskKind::Terminal, &[]);
    blocked.priority = 10;
    let blocked_id = blocked.id.clone();
    seed_task(&boot, blocked).await;
    let healthy = plan_task(&boot.wave_id, "healthy-tail", TaskKind::Terminal, &[]);
    let healthy_id = healthy.id.clone();
    seed_task(&boot, healthy).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json_remove(json_set(payload,'$.docRev',json_extract(payload,'$.docRev')+1),'$.blocks[2]') WHERE wave_id=?1 AND kind='wave-report'",
    )
    .bind(boot.wave_id.as_str())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE waves SET task_budget = 1 WHERE id = ?1")
        .bind(boot.wave_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;

    let blocked = boot.repo.task_get(&blocked_id).await.unwrap().unwrap();
    let healthy = boot.repo.task_get(&healthy_id).await.unwrap().unwrap();
    assert_eq!(blocked.status, TaskStatus::Pending);
    assert_eq!(blocked.context_stale_at_ms, None);
    assert_ne!(
        healthy.status,
        TaskStatus::Pending,
        "the same pass must continue until one claim consumes capacity"
    );
    assert_eq!(
        scheduler.context_resolve_failure_count("referenced_block_absent"),
        1
    );
    assert_eq!(
        scheduler
            .context_metrics()
            .snapshot()
            .context_resolve_failures["referenced_block_absent"],
        1,
        "the depth-two claim failure must be visible in the production health snapshot"
    );

    sqlx::query("UPDATE tasks SET status='done' WHERE id=?1")
        .bind(&healthy_id)
        .execute(&pool)
        .await
        .unwrap();
    let restored = json!({
        "id": "b_dead", "kind": "prose", "rev": 2,
        "payload": {"markdown": "restored"}
    });
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.docRev',json_extract(payload,'$.docRev')+1,'$.blocks',json_insert(json_extract(payload,'$.blocks'),'$[#]',json(?1))) WHERE wave_id=?2 AND kind='wave-report'",
    )
    .bind(restored.to_string())
    .bind(boot.wave_id.as_str())
    .execute(&pool)
    .await
    .unwrap();
    TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone())
        .resolve_task_closure(boot.wave_id.as_str(), "blocked-head")
        .await
        .expect("the repaired depth-two closure resolves");
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert!(
        event_rows(&boot, "task.context_frozen")
            .await
            .iter()
            .any(|(_, payload)| payload["task_id"] == blocked_id.as_str()),
        "restoring the depth-two target must let the next claim freeze context"
    );
}

#[tokio::test]
async fn production_claim_uses_narrow_root_hash_and_full_child_hash() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let key = "hash-shapes";
    let report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 1,
        summary: String::new(),
        body: String::new(),
        blocks: Some(vec![
            ReportBlock {
                id: "b_3001".into(),
                kind: "task".into(),
                rev: 1,
                payload: json!({
                    "key": key, "kind": "terminal", "goal": "true", "priority": 1,
                    "refs": [format!("neige://wave/{}#b_3002", boot.wave_id)]
                }),
            },
            ReportBlock {
                id: "b_3002".into(),
                kind: "prose".into(),
                rev: 1,
                payload: json!({"markdown": "child original"}),
            },
        ]),
    };
    insert_report_payload(
        &boot,
        "hash-shapes-report",
        serde_json::to_value(report).unwrap(),
    )
    .await;
    let task = plan_task(&boot.wave_id, key, TaskKind::Terminal, &[]);
    seed_task(&boot, task).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert!(
        scheduler.context_metrics().snapshot().closure_total > 0,
        "one production claim must increment the exported closure counter"
    );
    let monitor =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.blocks[0].payload.priority', 9) \
         WHERE id = 'hash-shapes-report'",
    )
    .execute(&pool)
    .await
    .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(event_rows(&boot, "task.context_advanced").await.is_empty());
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.blocks[1].payload.markdown', 'changed') \
         WHERE id = 'hash-shapes-report'",
    )
    .execute(&pool)
    .await
    .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(event_rows(&boot, "task.context_advanced").await.len(), 1);
}

async fn seed_frozen_context_fixture(boot: &Boot, key: &str) -> TaskContextMonitor {
    set_lifecycle(boot, WaveLifecycle::Working).await;
    let report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 3,
        summary: String::new(),
        body: "# Task\n".into(),
        blocks: Some(vec![ReportBlock {
            id: "b_1000".into(),
            kind: "task".into(),
            rev: 3,
            payload: json!({"key": key, "kind": "terminal", "goal": "original contract"}),
        }]),
    };
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "INSERT INTO cards \
         (id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES ('context-report',?1,'wave-report',-1,?2,'reportcard',0,1,1)",
    )
    .bind(boot.wave_id.as_str())
    .bind(serde_json::to_string(&report).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let task = plan_task(&boot.wave_id, key, TaskKind::Terminal, &[]);
    let task_id = task.id.clone();
    seed_task(boot, task).await;
    let (_runtime, scheduler) = build_scheduler(
        boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let claimed = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_ne!(
        claimed.status,
        TaskStatus::Pending,
        "fixture must use production claim"
    );
    let context: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(context, "[]");
    TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone())
}

async fn seed_production_report_context_fixture(
    boot: &Boot,
    key: &str,
) -> (TaskContextMonitor, String, String) {
    set_lifecycle(boot, WaveLifecycle::Working).await;
    let target_id = "b_2000";
    let task_payload = json!({
        "key": key,
        "kind": "terminal",
        "goal": "use the referenced contract",
        "refs": [format!("neige://wave/{}#{target_id}", boot.wave_id)],
        "ready": true,
        "declared_by": "spec",
    });
    let task_fence = render_fence("task", &task_payload);
    let original_body = format!("referenced original\n\n{task_fence}");
    let report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 0,
        summary: String::new(),
        body: original_body.clone(),
        blocks: Some(vec![
            ReportBlock {
                id: target_id.into(),
                kind: "prose".into(),
                rev: 1,
                payload: json!({"markdown": "referenced original\n\n"}),
            },
            ReportBlock {
                id: "b_1000".into(),
                kind: "task".into(),
                rev: 1,
                payload: task_payload,
            },
        ]),
    };
    insert_report_payload(
        boot,
        "context-report-production-writes",
        serde_json::to_value(report).unwrap(),
    )
    .await;
    let task = plan_task(&boot.wave_id, key, TaskKind::Terminal, &[]);
    let task_id = task.id.clone();
    seed_task(boot, task).await;
    sqlx::query("UPDATE tasks SET decl_ready=1 WHERE id=?1")
        .bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let (_runtime, scheduler) = build_scheduler(
        boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert!(matches!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying
    ));
    let frozen: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
        .bind(&task_id)
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_ne!(frozen, "[]", "fixture must use production context freeze");

    (
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone()),
        task_id,
        original_body,
    )
}

async fn persist_context_report_body(boot: &Boot, body: String) {
    let (wave, report_card, current) =
        resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
            .await
            .unwrap();
    let next = WaveReportPayload::new(current.summary.clone(), body);
    let doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.events,
        &boot.write,
        ActorId::User,
        EditAuthor::User,
        wave,
        report_card,
        current,
        next,
        doc_rev,
        None,
        None,
        false,
    )
    .await
    .unwrap();
}

async fn context_verdicts_for_task(boot: &Boot, task_id: &str) -> Vec<(String, String)> {
    event_rows(boot, "task.context_advanced")
        .await
        .into_iter()
        .filter(|(_, payload)| payload["task_id"] == task_id)
        .map(|(_, payload)| {
            (
                payload["verdict"].as_str().unwrap().to_string(),
                payload["rationale"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

async fn seed_fresh_context_copies(
    boot: &Boot,
    source_task_id: &str,
    count: usize,
    index_destination: bool,
) {
    let pool = boot.repo.sqlite_pool().unwrap();
    let frozen: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id=?1")
        .bind(source_task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let refs: Vec<TaskContextRef> = serde_json::from_str(&frozen).unwrap();
    assert_eq!(
        refs.len(),
        1,
        "budget fixture needs one tuple per fresh row"
    );
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    for index in 0..count {
        let key = format!("fresh-budget-{index:04}");
        let task_id = format!("{}:{key}", boot.wave_id);
        sqlx::query(
            "INSERT INTO tasks \
             (id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,\
              declared_by,claim_context_json,context_closure_truncated,\
              decl_ready,decl_released_by_user,context_verify_failures,spawn,\
              created_at_ms,updated_at_ms) \
             VALUES (?1,?2,?3,'terminal','true','null','[]',0,'dispatched',\
                     'spec',?4,0,0,0,0,'in-wave',1,1)",
        )
        .bind(&task_id)
        .bind(boot.wave_id.as_str())
        .bind(key)
        .bind(&frozen)
        .execute(&mut *tx)
        .await
        .unwrap();
        if index_destination {
            sqlx::query(
                "INSERT INTO task_ref_index(task_id,dst_wave_id,block_id) VALUES (?1,?2,?3)",
            )
            .bind(task_id)
            .bind(refs[0].wave_id.as_str())
            .bind(&refs[0].block_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
    }
    tx.commit().await.unwrap();
}

async fn seed_stale_context_copies(
    boot: &Boot,
    source_task_id: &str,
    key_prefix: &str,
    count: usize,
    refs_per_row: usize,
    mismatch: bool,
    index_destination: bool,
) -> Vec<String> {
    assert!(refs_per_row > 0);
    let pool = boot.repo.sqlite_pool().unwrap();
    let frozen: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id=?1")
        .bind(source_task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let source_key: String = sqlx::query_scalar("SELECT key FROM tasks WHERE id=?1")
        .bind(source_task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let source_wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let source_refs: Vec<TaskContextRef> = serde_json::from_str(&frozen).unwrap();
    let mut refs = vec![source_refs[0].clone()];
    for index in 1..refs_per_row {
        let mut reference = source_refs[0].clone();
        reference.block_id = format!("b_missing_{index:04}");
        reference.is_root = false;
        refs.push(reference);
    }
    if mismatch {
        refs[0].hash = "permanent-mismatch".into();
    }
    let frozen = serde_json::to_string(&refs).unwrap();
    let mut clone_wave_ids = Vec::with_capacity(count);
    for index in 0..count {
        let wave = boot
            .repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: source_wave.cove_id.clone(),
                title: format!("restore cursor fixture {key_prefix} {index}"),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        clone_wave_ids.push(wave.id);
    }
    let mut ids = Vec::with_capacity(count);
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    for (index, clone_wave_id) in clone_wave_ids.iter().enumerate() {
        let task_id = format!("{key_prefix}-{index:04}");
        sqlx::query(
            "INSERT INTO tasks \
             (id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,\
              declared_by,claim_context_json,context_stale_at_ms,context_closure_truncated,\
              decl_ready,decl_released_by_user,context_verify_failures,spawn,\
              created_at_ms,updated_at_ms) \
             VALUES (?1,?2,?3,'terminal','true','null','[]',0,'dispatched',\
                     'spec',?4,1,0,0,0,0,'in-wave',1,1)",
        )
        .bind(&task_id)
        .bind(clone_wave_id.as_str())
        .bind(&source_key)
        .bind(&frozen)
        .execute(&mut *tx)
        .await
        .unwrap();
        if index_destination {
            sqlx::query(
                "INSERT INTO task_ref_index(task_id,dst_wave_id,block_id) VALUES (?1,?2,?3)",
            )
            .bind(&task_id)
            .bind(source_refs[0].wave_id.as_str())
            .bind(&source_refs[0].block_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        ids.push(task_id);
    }
    tx.commit().await.unwrap();
    ids
}

async fn materialize_then_restore_root_bytes(
    boot: &Boot,
    monitor: &TaskContextMonitor,
    task_id: &str,
) {
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.blocks[0].payload.goal','temporary') \
         WHERE id='context-report'",
    )
    .execute(&pool)
    .await
    .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        stale.is_some(),
        "fixture must commit material before revert"
    );
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.blocks[0].payload.goal','original contract') \
         WHERE id='context-report'",
    )
    .execute(&pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn fresh_and_stale_event_fanout_budgets_are_independent() {
    let boot = boot().await;
    let key = "aa-event-restore-liveness";
    let monitor = seed_frozen_context_fixture(&boot, key).await;
    let task_id = format!("{}:{key}", boot.wave_id);
    materialize_then_restore_root_bytes(&boot, &monitor, &task_id).await;
    seed_fresh_context_copies(
        &boot,
        &task_id,
        calm_server::task_context::MAX_RERESOLVE_FANOUT,
        true,
    )
    .await;

    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();

    let stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(
        stale, None,
        "64 stable fresh rows must not consume the stale restore budget"
    );
}

#[tokio::test]
async fn consecutive_sweeps_restore_after_a_full_fresh_budget_without_starvation() {
    let boot = boot().await;
    let key = "aa-sweep-restore-liveness";
    let monitor = seed_frozen_context_fixture(&boot, key).await;
    let task_id = format!("{}:{key}", boot.wave_id);
    materialize_then_restore_root_bytes(&boot, &monitor, &task_id).await;
    seed_fresh_context_copies(
        &boot,
        &task_id,
        calm_server::task_context::MAX_SWEEP_NODES,
        false,
    )
    .await;

    monitor.sweep().await.unwrap();
    let after_first: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(
        after_first, None,
        "a full 4096-node fresh budget must still leave an independent restore budget"
    );
    monitor.sweep().await.unwrap();
    let after_second: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(after_second, None, "the next sweep must preserve recovery");
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![
            ("material".into(), "content_changed".into()),
            ("restored".into(), "content_restored_to_frozen".into()),
        ],
        "two consecutive sweeps restore once and never re-stale a stable row"
    );
}

#[tokio::test]
async fn event_budget_material_verdict_recovers_when_frozen_content_is_equal() {
    let boot = boot().await;
    let key = "zz-event-budget-recovery";
    let monitor = seed_frozen_context_fixture(&boot, key).await;
    let task_id = format!("{}:{key}", boot.wave_id);
    seed_fresh_context_copies(
        &boot,
        &task_id,
        calm_server::task_context::MAX_RERESOLVE_FANOUT + 1,
        true,
    )
    .await;

    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms
            .is_some(),
        "the first over-budget event pass must fail closed"
    );
    sqlx::query("DELETE FROM task_ref_index WHERE task_id IN (SELECT id FROM tasks WHERE key LIKE 'fresh-budget-%')")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("DELETE FROM tasks WHERE key LIKE 'fresh-budget-%'")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms,
        None,
        "a material budget rationale must not permanently veto equal frozen evidence"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![
            (
                "material".into(),
                "MAX_RERESOLVE_FANOUT budget exceeded".into()
            ),
            ("restored".into(), "content_restored_to_frozen".into()),
        ]
    );
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms,
        None,
        "the recovered task stays fresh after the fanout returns below the cap"
    );
}

#[tokio::test]
async fn sweep_budget_material_verdict_recovers_and_metrics_stay_split() {
    let boot = boot().await;
    let key = "zz-sweep-budget-recovery";
    let monitor = seed_frozen_context_fixture(&boot, key).await;
    let task_id = format!("{}:{key}", boot.wave_id);
    seed_fresh_context_copies(
        &boot,
        &task_id,
        calm_server::task_context::MAX_SWEEP_NODES + 1,
        false,
    )
    .await;

    monitor.sweep().await.unwrap();
    assert!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms
            .is_some(),
        "the first over-budget sweep must fail closed"
    );
    sqlx::query("DELETE FROM tasks WHERE key LIKE 'fresh-budget-%'")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    assert_eq!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms,
        None
    );
    let metrics = monitor.metrics().snapshot();
    assert_eq!(metrics.sweep_verified_tuples, 0);
    assert_eq!(metrics.sweep_restore_verified_tuples, 1);
    assert_eq!(metrics.sweep_hits, 0);
    assert_eq!(metrics.sweep_restore_hits, 1);
    assert_eq!(metrics.sweep_caps, 1);
    assert_eq!(metrics.sweep_restore_caps, 0);
    monitor.sweep().await.unwrap();
    assert_eq!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms,
        None,
        "the recovered task stays fresh after sweep pressure returns below the cap"
    );
}

#[tokio::test]
async fn event_restore_cursor_reaches_multiple_targets_beyond_stale_fanout_share() {
    let boot = boot().await;
    let source_key = "zz-event-cursor-source";
    let monitor = seed_frozen_context_fixture(&boot, source_key).await;
    let source_task_id = format!("{}:{source_key}", boot.wave_id);
    seed_stale_context_copies(&boot, &source_task_id, "a-blocker", 64, 1, true, true).await;
    let first = seed_stale_context_copies(&boot, &source_task_id, "b-target", 1, 1, false, true)
        .await
        .remove(0);
    seed_stale_context_copies(&boot, &source_task_id, "c-blocker", 64, 1, true, true).await;
    let second = seed_stale_context_copies(&boot, &source_task_id, "d-target", 1, 1, false, true)
        .await
        .remove(0);

    for _ in 0..3 {
        monitor
            .detect_wave_edit(boot.wave_id.as_str())
            .await
            .unwrap();
    }
    for task_id in [&first, &second] {
        assert_eq!(
            boot.repo
                .task_get(task_id)
                .await
                .unwrap()
                .unwrap()
                .context_stale_at_ms,
            None,
            "persistent event rotation must eventually attempt every tail target"
        );
        assert_eq!(
            context_verdicts_for_task(&boot, task_id).await,
            vec![("restored".into(), "content_restored_to_frozen".into())]
        );
    }
}

#[tokio::test]
async fn sweep_restore_cursor_reaches_multiple_targets_beyond_stale_tuple_share() {
    let boot = boot().await;
    let source_key = "zz-sweep-cursor-source";
    let monitor = seed_frozen_context_fixture(&boot, source_key).await;
    let source_task_id = format!("{}:{source_key}", boot.wave_id);
    seed_stale_context_copies(&boot, &source_task_id, "a-blocker", 64, 64, true, false).await;
    let first = seed_stale_context_copies(&boot, &source_task_id, "b-target", 1, 1, false, false)
        .await
        .remove(0);
    seed_stale_context_copies(&boot, &source_task_id, "c-blocker", 64, 64, true, false).await;
    let second = seed_stale_context_copies(&boot, &source_task_id, "d-target", 1, 1, false, false)
        .await
        .remove(0);

    for _ in 0..3 {
        monitor.sweep().await.unwrap();
    }
    for task_id in [&first, &second] {
        assert_eq!(
            boot.repo
                .task_get(task_id)
                .await
                .unwrap()
                .unwrap()
                .context_stale_at_ms,
            None,
            "persistent sweep rotation must eventually attempt every tail target"
        );
        assert_eq!(
            context_verdicts_for_task(&boot, task_id).await,
            vec![("restored".into(), "content_restored_to_frozen".into())]
        );
    }
}

#[tokio::test]
async fn committed_material_then_byte_identical_revert_restores_inflight_task() {
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, "restore-production-order").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let temporary_body =
        original_body.replacen("referenced original", "referenced original temporary", 1);

    persist_context_report_body(&boot, temporary_body).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let stale_after_first_detection: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id = ?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        stale_after_first_detection.is_some(),
        "the first detection must finish and commit stale before the revert"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())],
        "the reproduction must exercise the content_changed verdict"
    );

    persist_context_report_body(&boot, original_body).await;
    let (restore_winner, duplicate_restore) = tokio::join!(
        monitor.detect_wave_edit(boot.wave_id.as_str()),
        monitor.detect_wave_edit(boot.wave_id.as_str())
    );
    restore_winner.unwrap();
    duplicate_restore.unwrap();

    let stale_after_revert: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id = ?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stale_after_revert, None,
        "the committed stale episode must be restored"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![
            ("material".into(), "content_changed".into()),
            ("restored".into(), "content_restored_to_frozen".into()),
        ],
        "restore must be an audited edge paired with the material edge"
    );
}

#[tokio::test]
async fn terminal_transition_during_stale_episode_never_clears_or_emits_restored() {
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, "terminal-stale-restore-guard").await;
    let temporary_body =
        original_body.replacen("referenced original", "referenced terminal edit", 1);
    persist_context_report_body(&boot, temporary_body).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let stale_at: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(stale_at.is_some());
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())],
        "the end-to-end fixture must carry the real material event"
    );

    sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1")
        .bind(&task_id)
        .execute(&pool)
        .await
        .unwrap();
    persist_context_report_body(&boot, original_body).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();

    let task = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.context_stale_at_ms, stale_at);
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())],
        "terminal candidates must never emit the restored edge"
    );
}

#[tokio::test]
async fn material_commit_rechecks_after_locked_revert_without_production_hook() {
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, "material-a5-fence").await;
    let (_, original_card, _) = resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
        .await
        .unwrap();
    let original_payload = original_card.payload.to_string();
    let temporary_body =
        original_body.replacen("referenced original", "referenced original temporary", 1);
    persist_context_report_body(&boot, temporary_body).await;

    let pool = boot.repo.sqlite_pool().unwrap();
    let mut locked_revert = pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *locked_revert)
        .await
        .unwrap();

    let monitor = Arc::new(monitor);
    let metrics = monitor.metrics();
    let detector = {
        let monitor = Arc::clone(&monitor);
        let wave_id = boot.wave_id.clone();
        tokio::spawn(async move { monitor.detect_wave_edit(wave_id.as_str()).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if metrics
                .snapshot()
                .context_resolve_failures
                .get("content_changed")
                == Some(&1)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detector must classify the committed temporary content before the lock releases");
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(original_payload)
        .bind(original_card.id.as_str())
        .execute(&mut *locked_revert)
        .await
        .unwrap();
    sqlx::query("COMMIT")
        .execute(&mut *locked_revert)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), detector)
        .await
        .expect("detector must finish after the writer lock releases")
        .unwrap()
        .unwrap();

    let stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stale, None);
    assert!(context_verdicts_for_task(&boot, &task_id).await.is_empty());
    assert_eq!(
        metrics.snapshot().material_verdict_obsolete,
        1,
        "the in-transaction A5 fence must suppress the obsolete material verdict"
    );
}

#[tokio::test]
async fn restore_and_new_material_serialize_both_commit_orders_without_fail_open() {
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, "restore-race-fence").await;
    let (_, report_card, _) = resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
        .await
        .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let first_material =
        original_body.replacen("referenced original", "referenced first material", 1);
    persist_context_report_body(&boot, first_material).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let mut restored_report: WaveReportPayload = serde_json::from_str(
        &sqlx::query_scalar::<_, String>("SELECT payload FROM cards WHERE id=?1")
            .bind(report_card.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap(),
    )
    .unwrap();
    restored_report.body = original_body.clone();
    restored_report.blocks.as_mut().unwrap()[0].payload["markdown"] =
        json!("referenced original\n\n");
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&restored_report).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    // W3 owns the real SQLite writer slot while the old restore R reads the
    // last committed Equal evidence. R must park at BEGIN IMMEDIATE; W3 then
    // commits a newer mismatch before R's in-transaction evidence reread.
    let mut w3 = pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *w3)
        .await
        .unwrap();
    let monitor = Arc::new(monitor);
    let metrics = monitor.metrics();
    let mut old_restore = {
        let monitor = Arc::clone(&monitor);
        let wave_id = boot.wave_id.clone();
        tokio::spawn(async move { monitor.detect_wave_edit(wave_id.as_str()).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if metrics.snapshot().restore_checks >= 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old R must begin its restore check while W3 holds the writer slot");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut old_restore)
            .await
            .is_err(),
        "old R must be parked behind W3's BEGIN IMMEDIATE before W3 commits"
    );
    let second_material =
        original_body.replacen("referenced original", "referenced W3 material", 1);
    let mut w3_report: WaveReportPayload = serde_json::from_str(
        &sqlx::query_scalar::<_, String>("SELECT payload FROM cards WHERE id=?1")
            .bind(report_card.id.as_str())
            .fetch_one(&mut *w3)
            .await
            .unwrap(),
    )
    .unwrap();
    w3_report.body = second_material;
    w3_report.blocks.as_mut().unwrap()[0].payload["markdown"] = json!("referenced W3 material\n\n");
    w3_report.doc_rev += 1;
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&w3_report).unwrap())
        .bind(report_card.id.as_str())
        .execute(&mut *w3)
        .await
        .unwrap();
    sqlx::query("COMMIT").execute(&mut *w3).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), old_restore)
        .await
        .expect("old R must finish after W3 releases the writer slot")
        .unwrap()
        .unwrap();
    let stale_after_w3: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        stale_after_w3.is_some(),
        "W3-first serialization must not let old R clear newer material"
    );
    assert_eq!(
        metrics
            .snapshot()
            .restore_deferred
            .get("transaction_evidence_changed"),
        Some(&1),
        "the transaction fence, not only the unlocked prefilter, must veto old R"
    );

    // Reverse the order: let R commit restoration first, then commit W3 and
    // detect it. The durable edge sequence must rise again to material.
    restored_report.body = original_body.clone();
    restored_report.blocks.as_mut().unwrap()[0].payload["markdown"] =
        json!("referenced original\n\n");
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&restored_report).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let stale_after_r: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stale_after_r, None, "R-first serialization must commit R");
    restored_report.body =
        original_body.replacen("referenced original", "referenced material after R", 1);
    restored_report.blocks.as_mut().unwrap()[0].payload["markdown"] =
        json!("referenced material after R\n\n");
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&restored_report).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT context_stale_at_ms FROM tasks WHERE id=?1",)
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .is_some(),
        "W3 after committed R must produce a fresh material level"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![
            ("material".into(), "content_changed".into()),
            ("restored".into(), "content_restored_to_frozen".into()),
            ("material".into(), "content_changed".into()),
        ],
        "the two writer orders must converge to F,R,F without a fail-open clear"
    );
}

#[tokio::test]
async fn context_verdicts_alternate_material_restored_material_once_per_episode() {
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, "material-restored-material").await;
    let first_edit =
        original_body.replacen("referenced original", "referenced original first edit", 1);
    persist_context_report_body(&boot, first_edit).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    assert_eq!(context_verdicts_for_task(&boot, &task_id).await.len(), 1);

    persist_context_report_body(&boot, original_body.clone()).await;
    monitor.sweep().await.unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    assert_eq!(context_verdicts_for_task(&boot, &task_id).await.len(), 2);

    let second_edit =
        original_body.replacen("referenced original", "referenced original second edit", 1);
    persist_context_report_body(&boot, second_edit).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![
            ("material".into(), "content_changed".into()),
            ("restored".into(), "content_restored_to_frozen".into()),
            ("material".into(), "content_changed".into()),
        ],
        "each stable level emits zero duplicates while a new mismatch starts a new episode"
    );
}

#[tokio::test]
async fn restored_content_does_not_override_withdrawn_declaration() {
    let boot = boot().await;
    let key = "withdrawal-vetoes-restore";
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, key).await;
    let temporary_body =
        original_body.replacen("referenced original", "referenced original temporary", 1);
    persist_context_report_body(&boot, temporary_body).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();

    let (_, report_card, current) =
        resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
            .await
            .unwrap();
    let blocks = current.blocks.as_ref().unwrap();
    assert_eq!(
        (blocks[0].id.as_str(), blocks[1].id.as_str()),
        ("b_2000", "b_1000")
    );
    // One atomic DB-bypass mutation constructs the safety boundary directly:
    // frozen projection hashes are equal again while `ready` remains withdrawn.
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,\
         '$.blocks[0].payload.markdown',?1,'$.blocks[1].payload.ready',json('false')) WHERE id=?2",
    )
    .bind("referenced original\n\n")
    .bind(report_card.id.as_str())
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();

    let row: (Option<i64>, i64) =
        sqlx::query_as("SELECT context_stale_at_ms,context_verify_failures FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert!(
        row.0.is_some(),
        "a withdrawn declaration vetoes content restoration"
    );
    assert_eq!(
        row.1, 0,
        "the stale restore pass must not consume verify retries"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())],
        "a vetoed restoration emits no restored edge"
    );
    let metrics = monitor.metrics().snapshot();
    assert_eq!(metrics.restores, 0);
    assert_eq!(
        metrics.restore_deferred.get("declaration_withdrawn"),
        Some(&2),
        "event and sweep stale passes record their veto independently"
    );
    assert_eq!(
        metrics.context_resolve_failures.get("content_changed"),
        Some(&1),
        "stale restore vetoes must not pollute the fresh resolve-failure metric"
    );
    assert_eq!(
        metrics
            .context_resolve_failures
            .get("declaration_withdrawn"),
        None,
        "the declaration restore-deferred reason must exist only in its dedicated metric"
    );
    assert_eq!(
        (metrics.hits, row.1),
        (1, 0),
        "deferred restore checks neither add fresh hits nor consume verify retries"
    );
}

async fn assert_excluded_root_field_vetoes_restore(
    key: &str,
    align_frozen_root: bool,
    prepare: impl FnOnce(&mut WaveReportPayload),
    withdraw: impl FnOnce(&mut WaveReportPayload),
) {
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, key).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let (_, report_card, current) =
        resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
            .await
            .unwrap();
    let mut prepared = current;
    prepare(&mut prepared);
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&prepared).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    persist_context_report_body(
        &boot,
        original_body.replacen("referenced original", "referenced temporary", 1),
    )
    .await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let (_, report_card, mut restored) =
        resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
            .await
            .unwrap();
    restored.body = original_body;
    restored.blocks.as_mut().unwrap()[0].payload["markdown"] = json!("referenced original\n\n");
    withdraw(&mut restored);
    let restored_root_payload = restored.blocks.as_ref().unwrap()[1].payload.clone();
    restored.doc_rev += 1;
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&restored).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    if align_frozen_root {
        let mut projected = serde_json::Map::new();
        for field in calm_server::task_context::ROOT_HASH_TASK_FIELDS {
            if let Some(value) = restored_root_payload
                .get(*field)
                .filter(|value| !value.is_null())
            {
                projected.insert((*field).into(), value.clone());
            }
        }
        let hash = format!(
            "{:x}",
            Sha256::digest(
                calm_types::report_blocks::canonical_json(&Value::Object(projected)).as_bytes()
            )
        );
        let frozen_json: String =
            sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id=?1")
                .bind(&task_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let mut frozen: Vec<TaskContextRef> = serde_json::from_str(&frozen_json).unwrap();
        frozen
            .iter_mut()
            .find(|reference| reference.is_root)
            .unwrap()
            .hash = hash;
        sqlx::query("UPDATE tasks SET claim_context_json=?1,decl_ready=0 WHERE id=?2")
            .bind(serde_json::to_string(&frozen).unwrap())
            .bind(&task_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();

    assert!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT context_stale_at_ms FROM tasks WHERE id=?1",)
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .is_some(),
        "an excluded declaration field withdrawal must veto restoration"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())],
        "the declaration veto must emit no restored edge"
    );
}

#[tokio::test]
async fn renamed_root_key_vetoes_restore() {
    assert_excluded_root_field_vetoes_restore(
        "withdraw-key",
        false,
        |_| {},
        |report| report.blocks.as_mut().unwrap()[1].payload["key"] = json!("renamed-key"),
    )
    .await;
}

#[tokio::test]
async fn tombstoned_root_vetoes_restore() {
    assert_excluded_root_field_vetoes_restore(
        "withdraw-tombstone",
        true,
        |_| {},
        |report| {
            report.blocks.as_mut().unwrap()[1].payload = json!({
                "key": "withdraw-tombstone",
                "tombstone": {"reason": null},
                "declared_by": "spec",
                "tombstoned_by": "user",
            });
        },
    )
    .await;
}

#[tokio::test]
async fn released_by_user_withdrawal_vetoes_restore() {
    let boot = boot().await;
    let key = "withdraw-release";
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, key).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET decl_released_by_user=1 WHERE id=?1")
        .bind(&task_id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, report_card, mut report) =
        resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
            .await
            .unwrap();
    report.blocks.as_mut().unwrap()[1].payload["released_by_user"] = json!(true);
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&report).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    persist_context_report_body(
        &boot,
        original_body.replacen("referenced original", "referenced temporary", 1),
    )
    .await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let (_, report_card, mut restored) =
        resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
            .await
            .unwrap();
    restored.body = original_body;
    restored.blocks.as_mut().unwrap()[0].payload["markdown"] = json!("referenced original\n\n");
    restored.blocks.as_mut().unwrap()[1].payload["released_by_user"] = json!(false);
    restored.doc_rev += 1;
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&restored).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();

    assert!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT context_stale_at_ms FROM tasks WHERE id=?1",)
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .is_some(),
        "withdrawn user release must veto restoration"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())]
    );
}

#[tokio::test]
async fn unreverted_material_content_stays_stale_across_writes_and_sweeps() {
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, "material-stays-stale").await;
    let material_body =
        original_body.replacen("referenced original", "referenced original lasting edit", 1);
    persist_context_report_body(&boot, material_body.clone()).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(stale.is_some());
    sqlx::query("UPDATE tasks SET context_verify_failures=2 WHERE id=?1")
        .bind(&task_id)
        .execute(&pool)
        .await
        .unwrap();

    persist_context_report_body(&boot, material_body).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    monitor.sweep().await.unwrap();

    let row: (Option<i64>, i64) =
        sqlx::query_as("SELECT context_stale_at_ms,context_verify_failures FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row,
        (stale, 2),
        "failed restore checks preserve stale and retry state"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())],
        "real material content emits once and never self-restores"
    );
    let metrics = monitor.metrics().snapshot();
    assert_eq!((metrics.restore_checks, metrics.restores), (3, 0));
    assert_eq!(
        metrics.restore_deferred.get("content_changed"),
        Some(&3),
        "stale event/sweep checks use an independent deferred metric"
    );
    assert_eq!(
        metrics.context_resolve_failures.get("content_changed"),
        Some(&1),
        "three stale checks must not enter the fresh resolve-failure bucket"
    );
    assert_eq!(
        metrics.hits, 1,
        "deferred restore checks must not inflate fresh material hits"
    );
}

#[tokio::test]
async fn edit_then_byte_identical_revert_and_unrelated_doc_rev_do_not_mark_material() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "reverted").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.docRev', 4, \
         '$.blocks[0].rev', 4, '$.blocks[0].payload.goal', 'temporary edit') \
         WHERE id = 'context-report'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.docRev', 5, \
         '$.blocks[0].rev', 5, '$.blocks[0].payload.goal', 'original contract') \
         WHERE id = 'context-report'",
    )
    .execute(&pool)
    .await
    .unwrap();

    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    assert!(
        event_rows(&boot, "task.context_advanced").await.is_empty(),
        "post-claim equality ignores both rev churn and the report-level docRev fence"
    );
}

#[tokio::test]
async fn context_sweep_detects_db_bypass_and_emits_advanced_once() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "stale-once").await;
    monitor.sweep().await.unwrap();
    assert!(event_rows(&boot, "task.context_advanced").await.is_empty());

    let pool = boot.repo.sqlite_pool().unwrap();
    let card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE wave_id = ?1 AND kind = 'wave-report'")
            .bind(boot.wave_id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.blocks[0].payload.goal', 'replacement') WHERE id = ?1",
    )
    .bind(card_id)
    .execute(&pool)
    .await
    .unwrap();

    monitor.sweep().await.unwrap();
    monitor.sweep().await.unwrap();
    monitor.sweep().await.unwrap();
    assert_eq!(
        event_rows(&boot, "task.context_advanced").await.len(),
        1,
        "once-per-condition guard must survive three more sweeps"
    );
    let stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id = ?1")
            .bind(format!("{}:stale-once", boot.wave_id))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(stale.is_some());
}

#[tokio::test]
async fn context_sweep_marks_material_when_closure_was_truncated() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "truncated-still-matches").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET context_closure_truncated = 1 WHERE id = ?1")
        .bind(format!("{}:truncated-still-matches", boot.wave_id))
        .execute(&pool)
        .await
        .unwrap();

    monitor.sweep().await.unwrap();

    assert_eq!(
        event_rows(&boot, "task.context_advanced").await.len(),
        1,
        "a truncated closure has an unverifiable suffix and is fail-closed"
    );
    monitor.sweep().await.unwrap();
    let task_id = format!("{}:truncated-still-matches", boot.wave_id);
    assert!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms
            .is_some(),
        "equal visible refs cannot prove equality for the truncated suffix"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![(
            "material".into(),
            "frozen reference closure was truncated".into()
        )]
    );
}

#[tokio::test]
async fn malformed_stored_report_is_deterministic_and_marks_material_immediately() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "retryable-verify").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let task_id = format!("{}:retryable-verify", boot.wave_id);
    sqlx::query(
        "UPDATE cards SET payload=json_remove(payload,'$.docRev') WHERE id='context-report'",
    )
    .execute(&pool)
    .await
    .unwrap();
    monitor.sweep().await.unwrap();
    let row: (i64, Option<i64>) =
        sqlx::query_as("SELECT context_verify_failures,context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, 0);
    assert!(row.1.is_some());
    assert_eq!(event_rows(&boot, "task.context_advanced").await.len(), 1);
}

#[tokio::test]
async fn storage_unavailable_during_system_cove_lookup_is_retryable() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "storage-retry").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let task_id = format!("{}:storage-retry", boot.wave_id);

    sqlx::query("ALTER TABLE coves RENAME TO coves_unavailable")
        .execute(&pool)
        .await
        .unwrap();
    for expected in [(1, None), (2, None)] {
        monitor.sweep().await.unwrap();
        let row: (i64, Option<i64>) = sqlx::query_as(
            "SELECT context_verify_failures,context_stale_at_ms FROM tasks WHERE id=?1",
        )
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, expected);
        assert!(event_rows(&boot, "task.context_advanced").await.is_empty());
    }
    monitor.sweep().await.unwrap();
    let row: (i64, Option<i64>) =
        sqlx::query_as("SELECT context_verify_failures,context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, 0, "material winner clears the retry streak");
    assert!(row.1.is_some(), "third consecutive failure is material");
    assert_eq!(event_rows(&boot, "task.context_advanced").await.len(), 1);
    sqlx::query("ALTER TABLE coves_unavailable RENAME TO coves")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn successful_context_verification_resets_retry_streak() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "storage-reset").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let task_id = format!("{}:storage-reset", boot.wave_id);
    sqlx::query("ALTER TABLE coves RENAME TO coves_unavailable")
        .execute(&pool)
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    sqlx::query("ALTER TABLE coves_unavailable RENAME TO coves")
        .execute(&pool)
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    let row: (i64, Option<i64>) =
        sqlx::query_as("SELECT context_verify_failures,context_stale_at_ms FROM tasks WHERE id=?1")
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (0, None));
    assert!(event_rows(&boot, "task.context_advanced").await.is_empty());
}

async fn assert_deletion_event_runs_context_sweep(event: Event, deleted_wave_id: &str, key: &str) {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let referenced = boot
        .repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: boot.cove_id.clone(),
            title: deleted_wave_id.into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let child_report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 1,
        summary: String::new(),
        body: String::new(),
        blocks: Some(vec![ReportBlock {
            id: "b_dead".into(),
            kind: "prose".into(),
            rev: 1,
            payload: json!({"markdown": "will disappear"}),
        }]),
    };
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "INSERT INTO cards \
         (id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES (?1,?2,'wave-report',-1,?3,'reportcard',0,1,1)",
    )
    .bind(format!("deleted-report-{key}"))
    .bind(referenced.id.as_str())
    .bind(serde_json::to_string(&child_report).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let root_report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 1,
        summary: String::new(),
        body: String::new(),
        blocks: Some(vec![ReportBlock {
            id: "b_1000".into(),
            kind: "task".into(),
            rev: 1,
            payload: json!({
                "key": key, "kind": "terminal", "goal": "true",
                "refs": [format!("neige://wave/{}#b_dead", referenced.id)]
            }),
        }]),
    };
    insert_report_payload(
        &boot,
        &format!("deletion-root-{key}"),
        serde_json::to_value(root_report).unwrap(),
    )
    .await;
    let task_id = format!("{}:{key}", boot.wave_id);
    seed_task(
        &boot,
        plan_task(&boot.wave_id, key, TaskKind::Terminal, &[]),
    )
    .await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let frozen: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(frozen, "[]");
    let frozen_refs: Vec<TaskContextRef> = serde_json::from_str(&frozen).unwrap();
    assert_eq!(
        frozen_refs.len(),
        2,
        "fixture must freeze the deleted child"
    );
    assert!(matches!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying
    ));
    sqlx::query("DELETE FROM waves WHERE id = ?1")
        .bind(referenced.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let state = app_state_for_context_events(&boot);

    let event = match event {
        Event::WaveDeleted { .. } => Event::WaveDeleted {
            id: referenced.id.clone(),
            cove_id: boot.cove_id.clone(),
        },
        Event::CoveDeleted { .. } => Event::CoveDeleted {
            id: boot.cove_id.clone(),
        },
        _ => unreachable!("deletion fixture only accepts wave/cove deletion"),
    };
    boot.events.emit(ActorId::Kernel, event);
    for _ in 0..500 {
        if !event_rows(&boot, "task.context_advanced").await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        event_rows(&boot, "task.context_advanced").await.len(),
        1,
        "dispatcher deletion arm must run the context sweep"
    );
    drop(state);
}

#[tokio::test]
async fn wave_deleted_event_runs_context_sweep_end_to_end() {
    assert_deletion_event_runs_context_sweep(
        Event::WaveDeleted {
            id: WaveId::from("deleted-destination"),
            cove_id: CoveId::from("deleted-destination-cove"),
        },
        "deleted-destination",
        "wave-deleted-event",
    )
    .await;
}

#[tokio::test]
async fn cove_deleted_event_runs_context_sweep_end_to_end() {
    assert_deletion_event_runs_context_sweep(
        Event::CoveDeleted {
            id: CoveId::from("deleted-destination-cove"),
        },
        "deleted-wave-under-cove",
        "cove-deleted-event",
    )
    .await;
}

#[tokio::test]
async fn event_detection_catches_deleted_or_recycled_same_id_same_rev() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "recycled").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.blocks[0].payload.goal', 'new incarnation') \
         WHERE wave_id = ?1 AND kind = 'wave-report'",
    )
    .bind(boot.wave_id.as_str())
    .execute(&pool)
    .await
    .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(event_rows(&boot, "task.context_advanced").await.len(), 1);
}

#[tokio::test]
async fn referenced_block_deletion_is_material_even_without_changed_ids() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "deleted-ref").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.blocks', json('[]')) \
         WHERE wave_id = ?1 AND kind = 'wave-report'",
    )
    .bind(boot.wave_id.as_str())
    .execute(&pool)
    .await
    .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(event_rows(&boot, "task.context_advanced").await.len(), 1);
}

#[tokio::test]
async fn referenced_block_absence_recovers_only_when_the_frozen_identity_returns() {
    let boot = boot().await;
    let (monitor, task_id, _) =
        seed_production_report_context_fixture(&boot, "absent-same-id-restores").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let (_, report_card, original_report) =
        resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
            .await
            .unwrap();
    let frozen_json: String =
        sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let frozen: Vec<TaskContextRef> = serde_json::from_str(&frozen_json).unwrap();
    let child_id = frozen
        .iter()
        .find(|reference| !reference.is_root)
        .unwrap()
        .block_id
        .clone();
    let mut deleted = original_report.clone();
    deleted
        .blocks
        .as_mut()
        .unwrap()
        .retain(|block| block.id != child_id);
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&deleted).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "referenced_block_absent".into())]
    );

    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&original_report).unwrap())
        .bind(report_card.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms,
        None,
        "the original block id and hash make the frozen closure provably equal again"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![
            ("material".into(), "referenced_block_absent".into()),
            ("restored".into(), "content_restored_to_frozen".into()),
        ]
    );
}

#[tokio::test]
async fn deleted_then_rebuilt_reference_keeps_known_identity_gap_stale() {
    // Known gap: production reconstruction mints a new block id. This
    // content-hash restore mechanism deliberately cannot equate that new
    // identity with the deleted frozen reference, even when bytes match.
    let boot = boot().await;
    let (monitor, task_id, original_body) =
        seed_production_report_context_fixture(&boot, "deleted-rebuilt-known-gap").await;
    let frozen_json: String =
        sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    let frozen: Vec<TaskContextRef> = serde_json::from_str(&frozen_json).unwrap();
    let frozen_child_id = frozen
        .iter()
        .find(|reference| !reference.is_root)
        .unwrap()
        .block_id
        .clone();

    let body_without_child = original_body.replacen("referenced original\n\n", "", 1);
    persist_context_report_body(&boot, body_without_child).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "referenced_block_absent".into())],
        "deleting the frozen block records the identity-loss rationale"
    );

    persist_context_report_body(&boot, original_body).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    let (_, _, rebuilt) = resolve_report_for_wave(boot.repo.as_ref(), boot.wave_id.as_str())
        .await
        .unwrap();
    let rebuilt_child = rebuilt
        .blocks
        .unwrap()
        .into_iter()
        .find(|block| block.kind == "prose")
        .unwrap();
    assert_ne!(
        rebuilt_child.id, frozen_child_id,
        "production rebuild mints a new block identity even for identical bytes"
    );
    assert!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT context_stale_at_ms FROM tasks WHERE id=?1",)
            .bind(&task_id)
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap()
            .is_some(),
        "known gap: content equality cannot restore a deleted block under a newly minted id"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "referenced_block_absent".into())],
        "known gap stays fail-closed and emits no misleading restored edge"
    );
}

#[tokio::test]
async fn missing_frozen_context_is_material_and_terminal_index_is_cleaned() {
    let boot = boot().await;
    let mut task = plan_task(&boot.wave_id, "missing", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let monitor =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    monitor.sweep().await.unwrap();
    assert_eq!(event_rows(&boot, "task.context_advanced").await.len(), 1);
    monitor.sweep().await.unwrap();
    assert!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms
            .is_some(),
        "a missing frozen value cannot prove closure equality"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![(
            "material".into(),
            "frozen reference set is missing or malformed".into()
        )]
    );

    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET status = 'failed' WHERE id = ?1")
        .bind(&task_id)
        .execute(&pool)
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM task_ref_index WHERE task_id = ?1")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "terminal task cannot retain reverse-index rows");
}

#[tokio::test]
async fn closure_depth_exhaustion_truncates_and_cross_cove_is_rejected() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let blocks: Vec<Value> = (0..5)
        .map(|index| {
            let markdown = if index == 4 {
                "leaf".to_string()
            } else {
                format!("[next](neige://wave/{}#b_{:04})", boot.wave_id, index + 1)
            };
            json!({
                "id": format!("b_{index:04}"),
                "kind": "prose",
                "rev": 1,
                "payload": {"markdown": markdown}
            })
        })
        .collect();
    sqlx::query(
        "INSERT INTO cards \
         (id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES ('closure-report',?1,'wave-report',-1,?2,'reportcard',0,1,1)",
    )
    .bind(boot.wave_id.as_str())
    .bind(json!({"docRev": 1, "blocks": blocks}).to_string())
    .execute(&pool)
    .await
    .unwrap();
    let monitor =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    let closure = monitor
        .resolve_closure(boot.wave_id.as_str(), "b_0000")
        .await
        .unwrap();
    assert_eq!(closure.refs.len(), 4, "depth zero through depth three");
    assert!(closure.closure_truncated);

    let foreign_cove = boot
        .repo
        .cove_create(NewCove {
            name: "foreign".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let foreign_wave = boot
        .repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: foreign_cove.id,
            title: "foreign".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO cards \
         (id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES ('foreign-report',?1,'wave-report',-1,?2,'reportcard',0,1,1)",
    )
    .bind(foreign_wave.id.as_str())
    .bind(
        json!({"docRev": 1, "blocks": [{
            "id":"b_f000","kind":"prose","rev":1,"payload":{"markdown":"foreign"}
        }]})
        .to_string(),
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE cards SET payload = ?1 WHERE id = 'closure-report'",
    )
    .bind(json!({"docRev": 2, "blocks": [{
        "id":"b_0000","kind":"prose","rev":1,
        "payload":{"markdown": format!("[foreign](neige://wave/{}#b_f000)", foreign_wave.id)}
    }]}).to_string())
    .execute(&pool)
    .await
    .unwrap();
    let error = monitor
        .resolve_closure(boot.wave_id.as_str(), "b_0000")
        .await
        .expect_err("cross-cove closure must fail closed");
    assert!(matches!(error, ResolveError::CrossCove(_)));
}

#[tokio::test]
async fn reresolve_fanout_and_sweep_node_caps_fail_closed() {
    let boot = boot().await;
    let monitor = seed_frozen_context_fixture(&boot, "fanout-00").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let frozen_json: String =
        sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
            .bind(format!("{}:fanout-00", boot.wave_id))
            .fetch_one(&pool)
            .await
            .unwrap();
    let payload: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE id = 'context-report'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut report: WaveReportPayload = serde_json::from_str(&payload).unwrap();
    for index in 1..65 {
        let key = format!("fanout-{index:02}");
        report.blocks.as_mut().unwrap().push(ReportBlock {
            id: format!("b_a{index:03}"),
            kind: "task".into(),
            rev: 1,
            payload: json!({
                "key": key, "kind": "terminal", "goal": "true",
                "refs": [format!("neige://wave/{}#b_1000", boot.wave_id)]
            }),
        });
        let task = plan_task(&boot.wave_id, &key, TaskKind::Terminal, &[]);
        seed_task(&boot, task).await;
    }
    report.doc_rev += 1;
    sqlx::query("UPDATE cards SET payload = ?1 WHERE id = 'context-report'")
        .bind(serde_json::to_string(&report).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET task_budget = 65 WHERE id = ?1")
        .bind(boot.wave_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let claimed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tasks WHERE wave_id = ?1 AND status != 'pending' ",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        claimed, 65,
        "fanout rows must all come from production claim"
    );
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        event_rows(&boot, "task.context_advanced").await.len(),
        1,
        "the first task beyond fanout 64 is material even when tuples match"
    );

    report.blocks.as_mut().unwrap().push(ReportBlock {
        id: "b_ca900".into(),
        kind: "task".into(),
        rev: 1,
        payload: json!({"key": "sweep-cap", "kind": "terminal", "goal": "true"}),
    });
    report.doc_rev += 1;
    sqlx::query("UPDATE cards SET payload = ?1 WHERE id = 'context-report'")
        .bind(serde_json::to_string(&report).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET task_budget = 66 WHERE id = ?1")
        .bind(boot.wave_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let cap_task = plan_task(&boot.wave_id, "sweep-cap", TaskKind::Terminal, &[]);
    let cap_id = cap_task.id.clone();
    seed_task(&boot, cap_task).await;
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let one: Value = serde_json::from_str::<Vec<Value>>(&frozen_json).unwrap()[0].clone();
    let oversized = vec![one; calm_server::task_context::MAX_SWEEP_NODES + 1];
    // This is the dedicated sweep-limit corruption fixture: no production
    // claim can emit a closure beyond MAX_REF_NODES, so the persisted
    // over-limit state must be injected after a real claim.
    sqlx::query("UPDATE tasks SET claim_context_json = ?1 WHERE id = ?2")
        .bind(serde_json::to_string(&oversized).unwrap())
        .bind(cap_id)
        .execute(&pool)
        .await
        .unwrap();
    monitor.sweep().await.unwrap();
    let verdicts = event_rows(&boot, "task.context_advanced")
        .await
        .into_iter()
        .map(|(_, payload)| {
            (
                payload["verdict"].as_str().unwrap().to_string(),
                payload["rationale"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        verdicts,
        vec![
            (
                "material".into(),
                "MAX_RERESOLVE_FANOUT budget exceeded".into()
            ),
            ("material".into(), "MAX_SWEEP_NODES budget exceeded".into()),
            ("restored".into(), "content_restored_to_frozen".into()),
        ],
        "both caps fail closed, while the earlier equal fanout verdict is audibly restored"
    );
}

// ---------------------------------------------------------------------------
// Review round 1 — F3: a sibling card's report can never flip another
// task's row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sibling_card_report_cannot_flip_other_tasks_row() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "owned", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    // Stamped: the scheduler recorded which card owns this task.
    task.worker_card_id = Some(boot.worker_card_id.as_str().to_string());
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // Mint a sibling worker card in the SAME wave — wave-pinning alone
    // would let it terminalize the row.
    let sibling = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("sibling worker card");
    boot.card_role_cache
        .insert(sibling.id.clone(), CardRole::Worker, boot.wave_id.clone());
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    seed_runtime_session_in_pool(
        &pool,
        sibling.id.as_str(),
        "sibling-session",
        "sibling-thread",
    )
    .await;
    let sibling_identity = ToolCallIdentity {
        card_id: sibling.id.as_str().to_string(),
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id: "sibling-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "sibling-thread".into(),
    };

    // Sibling completes "someone else's" task → guarded flip no-ops AND
    // (round-2 F3 case iv) the whole report is refused: error back to
    // the caller, NO event persisted, no lifecycle transition.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        sibling_identity.clone(),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect_err("sibling report against a row stamped to another card must be rejected");
    let row = task_row(&boot, "owned").await;
    assert_eq!(
        row.status,
        TaskStatus::Running,
        "sibling card must not complete a row stamped to another card"
    );
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str()),
        "stamp untouched"
    );
    assert!(
        event_rows(&boot, "task.completed").await.is_empty(),
        "rejected report must persist no terminal event"
    );

    // Same guard on the failure flip.
    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        sibling_identity,
        json!({ "idempotency_key": task_id, "reason": "not mine" }),
    )
    .await
    .expect_err("sibling fail report must be rejected");
    assert_eq!(task_row(&boot, "owned").await.status, TaskStatus::Running);
    assert!(event_rows(&boot, "task.failed").await.is_empty());
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "rejected reports must not run the Working → Reviewing transition"
    );

    // The stamped owner still flips normally.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect("owner complete report");
    assert_eq!(task_row(&boot, "owned").await.status, TaskStatus::Done);
    assert_eq!(event_rows(&boot, "task.completed").await.len(), 1);
}

// ---------------------------------------------------------------------------
// Review round 1 — F4: the claim tx re-checks the wave lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_aborts_when_lifecycle_leaves_schedulable_set() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_task(
        &boot,
        plan_task(&boot.wave_id, "held", TaskKind::Codex, &[]),
    )
    .await;

    // Park the pass between the (passing) pre-claim lifecycle read and
    // the claim tx, then move the wave out of the schedulable set.
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&semaphore)
        .acquire_owned()
        .await
        .expect("test holds the only permit");
    let (_runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        semaphore,
    );
    let handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let wave_id = boot.wave_id.clone();
        async move { scheduler.schedule_wave(wave_id).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    set_lifecycle(&boot, WaveLifecycle::Canceled).await;
    drop(permit);
    handle.await.expect("schedule_wave task");

    assert_eq!(
        task_row(&boot, "held").await.status,
        TaskStatus::Pending,
        "in-tx lifecycle guard must abort the claim (race-lost, silent)"
    );
    assert!(
        event_rows(&boot, "task.dispatched").await.is_empty(),
        "a lost claim persists no dispatch record"
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);
}

// ---------------------------------------------------------------------------
// Review round 1 — F5: claiming from a Planning wave promotes it along
// Planning → Dispatching → Working in the claim tx
// ---------------------------------------------------------------------------

#[tokio::test]
async fn planning_wave_promotes_to_working_on_claim() {
    let boot = boot().await; // wave is Draft (create default)
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    // Writing a ready report task block without a lifecycle arg auto-promotes
    // Draft to Planning and leaves the wave there — the F5 scenario.
    insert_report_payload(
        &boot,
        "report-planning-claim",
        serde_json::to_value(WaveReportPayload::initial()).unwrap(),
    )
    .await;
    edit_report_blocks(
        &boot,
        &[(
            "b_planning_task",
            "task",
            json!({
                "key": "p1", "kind": "codex", "goal": "do p1",
                "ready": true, "declared_by": "spec"
            }),
        )],
        0,
    )
    .await;
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Planning,
        "task block write with no lifecycle arg leaves the wave Planning"
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = task_row(&boot, "p1").await;
    assert_eq!(
        row.status,
        TaskStatus::Running,
        "Planning waves schedule (§5.2)"
    );
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "claim tx chains Planning → Dispatching → Working"
    );

    // A later worker report then drives Working → Reviewing as usual.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": row.id, "result": { "ok": true } }),
    )
    .await
    .expect("worker report");
    assert_eq!(task_row(&boot, "p1").await.status, TaskStatus::Done);
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);
}

// ---------------------------------------------------------------------------
// Review round 5 — F1: a dependent task claimed while the wave sits in
// Reviewing (the first worker's completion promoted it) rides the legal
// Reviewing → Working edge in the claim tx
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reviewing_wave_promotes_back_to_working_on_dependent_claim() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_projected_task(&boot, plan_task(&boot.wave_id, "t1", TaskKind::Codex, &[])).await;
    seed_projected_task(
        &boot,
        plan_task(&boot.wave_id, "t2", TaskKind::Codex, &["t1"]),
    )
    .await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let t1 = task_row(&boot, "t1").await;
    assert_eq!(t1.status, TaskStatus::Running);

    // First worker reports → emit tx flips t1 done AND promotes the
    // wave Working → Reviewing.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": t1.id, "result": { "ok": true } }),
    )
    .await
    .expect("t1 complete");
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);

    // t2's dep is now satisfied; in production the task.completed
    // envelope pokes the scheduler. The claim from a Reviewing wave
    // must promote it back to Working in the same tx — otherwise the
    // wave reads `Reviewing` while new work runs and the second
    // completion's Working → Reviewing transition can never fire.
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let t2 = task_row(&boot, "t2").await;
    assert_eq!(t2.status, TaskStatus::Running, "dependent task claimed");
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "claim tx must ride the legal Reviewing → Working edge"
    );

    // The second completion promotes Working → Reviewing again.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": t2.id, "result": { "ok": true } }),
    )
    .await
    .expect("t2 complete");
    assert_eq!(task_row(&boot, "t2").await.status, TaskStatus::Done);
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);
}

// ---------------------------------------------------------------------------
// Review round 1 — F6: resuming a dispatched terminal task immediately
// reconciles a recorded exit (one boot sweep, no second pass)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn boot_sweep_resolves_dispatched_terminal_with_recorded_exit_in_one_pass() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "crashed", TaskKind::Terminal, &[]);
    // Claimed before the crash; the PTY exited while the kernel was
    // down and the supervisor reconcile persisted the synthetic -1.
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    boot.repo
        .terminal_set_exit(&terminal_id, Some(-1), false)
        .await
        .expect("persist synthetic exit");

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: card_id.as_str().to_string(),
        })],
    );
    // ONE sweep: dispatched arm resumes the op → running stamp → the
    // immediate recorded-exit reconcile lands the terminal state.
    scheduler.sweep_all().await;

    let row = task_row(&boot, "crashed").await;
    assert_eq!(
        row.status,
        TaskStatus::Failed,
        "a single boot sweep must reach the terminal state"
    );
    assert_eq!(row.status_detail.as_deref(), Some("worker-reported"));
    assert_eq!(event_rows(&boot, "task.failed").await.len(), 1);
}

// ---------------------------------------------------------------------------
// Review round 1 — F7: the boot sweep's pending arm dispatches via the
// async poke path instead of blocking
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sweep_boot_dispatches_pending_without_blocking() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_projected_task(&boot, plan_task(&boot.wave_id, "bg", TaskKind::Codex, &[])).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    // Returns after the reconcile arms; pending dispatch is poked onto
    // a background task.
    scheduler.sweep_boot().await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if task_row(&boot, "bg").await.status == TaskStatus::Running {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "poked schedule pass never dispatched the pending task"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(event_rows(&boot, "task.dispatched").await.len(), 1);
}

// ---------------------------------------------------------------------------
// Review round 1 — F8: sweep dispatched-arm sub-cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sweep_marks_running_when_op_succeeded_before_crash() {
    // Crash window: the worker op ran to success but the kernel died
    // before the running stamp — the sweep must stamp, not respawn.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "stamped", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    seed_task(&boot, task.clone()).await;
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    let (op_kind, payload) = build_worker_payload(&task).expect("payload");
    let payload_hash = stable_payload_hash(&payload).expect("hash");
    let op_id = runtime
        .submit(
            op_kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash,
            },
            payload,
        )
        .await
        .expect("submit");
    let result = runtime.wait(&op_id).await.expect("wait");
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "fixture op must succeed, got {:?}",
        result.outcome
    );
    assert_eq!(
        task_row(&boot, "stamped").await.status,
        TaskStatus::Dispatched,
        "running stamp lost in the crash window"
    );

    scheduler.sweep_all().await;

    let row = task_row(&boot, "stamped").await;
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str()),
        "stamp recovered from the op result"
    );
    assert_eq!(
        operation_count(&boot, "codex-worker").await,
        1,
        "no respawn — submit deduped on the idempotency key"
    );
}

#[tokio::test]
async fn sweep_redrives_half_driven_operation() {
    // The op row exists but was never driven to a terminal phase
    // (crash right after insert, or a lease-stuck driver). The sweep's
    // `wait()` is the steady-state re-drive.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "stalled", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    seed_task(&boot, task.clone()).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );

    // Insert WITHOUT driving (bypasses `submit`'s inline drive).
    let op_repo = SqlxOperationRepo::new(boot.repo.sqlite_pool().expect("sqlite pool"));
    let (op_kind, payload) = build_worker_payload(&task).expect("payload");
    let payload_hash = stable_payload_hash(&payload).expect("hash");
    op_repo
        .insert_operation(
            op_kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash,
            },
            payload,
        )
        .await
        .expect("insert non-terminal op");
    assert_eq!(operation_count(&boot, "codex-worker").await, 1);
    assert_eq!(
        task_row(&boot, "stalled").await.status,
        TaskStatus::Dispatched
    );

    scheduler.sweep_all().await;

    let row = task_row(&boot, "stalled").await;
    assert_eq!(row.status, TaskStatus::Running, "wait() re-drove the op");
    assert_eq!(
        operation_count(&boot, "codex-worker").await,
        1,
        "re-drive, not a second op"
    );
}

#[tokio::test]
async fn sweep_fails_task_when_preexisting_op_failed() {
    // The worker op already terminated `failed` (e.g. spawn failure
    // whose task reconcile was lost to a crash) — the sweep must mark
    // the row failed('spawn-failed'), not leave it dispatched forever.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "wedged", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    seed_task(&boot, task.clone()).await;
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(FailingSpawnAdapter {
            kind: "codex-worker",
        })],
    );

    let (op_kind, payload) = build_worker_payload(&task).expect("payload");
    let payload_hash = stable_payload_hash(&payload).expect("hash");
    let op_id = runtime
        .submit(
            op_kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash,
            },
            payload,
        )
        .await
        .expect("submit");
    let result = runtime.wait(&op_id).await.expect("wait");
    assert!(
        matches!(result.outcome, OperationOutcome::Failed { .. }),
        "fixture op must fail, got {:?}",
        result.outcome
    );
    assert_eq!(
        task_row(&boot, "wedged").await.status,
        TaskStatus::Dispatched,
        "task reconcile lost in the crash window"
    );

    scheduler.sweep_all().await;

    let row = task_row(&boot, "wedged").await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("spawn-failed"));
    let failed = event_rows(&boot, "task.failed").await;
    assert_eq!(failed.len(), 1);
    assert!(failed[0].0.contains("KernelDispatcher"));
}

// ---------------------------------------------------------------------------
// Review round 2 — F1: the claim tx revalidates the ready predicate
// (deps + budget) against the CURRENT plan, not the pre-claim snapshot
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_aborts_when_dep_added_pre_claim() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let task = plan_task(&boot.wave_id, "revised", TaskKind::Codex, &[]);
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // Park the pass inside `dispatch_task` between the ready-set
    // snapshot and the claim (the semaphore wait window).
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&semaphore)
        .acquire_owned()
        .await
        .expect("test holds the only permit");
    let (_runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        semaphore,
    );
    let handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let wave_id = boot.wave_id.clone();
        async move { scheduler.schedule_wave(wave_id).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // The plan.updated shape: the spec inserts a new prerequisite task
    // and revises the still-pending row to depend on it.
    seed_task(
        &boot,
        plan_task(&boot.wave_id, "prereq", TaskKind::Codex, &[]),
    )
    .await;
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let revised =
        sqlx::query("UPDATE tasks SET depends_on_json = ?1 WHERE id = ?2 AND status = 'pending'")
            .bind(json!(["prereq"]).to_string())
            .bind(&task_id)
            .execute(&pool)
            .await
            .expect("revise pending row deps");
    assert_eq!(revised.rows_affected(), 1, "dep added while parked");

    drop(permit);
    handle.await.expect("schedule_wave task");

    assert_eq!(
        task_row(&boot, "revised").await.status,
        TaskStatus::Pending,
        "in-tx dep revalidation must abort the claim (race-lost, silent)"
    );
    assert!(
        event_rows(&boot, "task.dispatched").await.is_empty(),
        "a lost claim persists no dispatch record"
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);
}

#[tokio::test]
async fn claim_aborts_when_budget_shrunk_pre_claim() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_task(
        &boot,
        plan_task(&boot.wave_id, "held", TaskKind::Codex, &[]),
    )
    .await;

    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&semaphore)
        .acquire_owned()
        .await
        .expect("test holds the only permit");
    let (_runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        semaphore,
    );
    let handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let wave_id = boot.wave_id.clone();
        async move { scheduler.schedule_wave(wave_id).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // `PATCH /api/waves` shrinks the budget to 0 mid-window.
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                task_budget: Some(Some(0)),
                ..Default::default()
            },
        )
        .await
        .expect("shrink budget");

    drop(permit);
    handle.await.expect("schedule_wave task");

    assert_eq!(
        task_row(&boot, "held").await.status,
        TaskStatus::Pending,
        "in-tx budget revalidation must abort the claim"
    );
    assert!(event_rows(&boot, "task.dispatched").await.is_empty());
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);
}

// ---------------------------------------------------------------------------
// Review round 2 — F2 + F3 case (iv): an UNSTAMPED dispatched row only
// accepts the card that proves payload ownership of the key; rejected
// reports error and emit nothing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unstamped_dispatched_row_rejects_sibling_report() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    // Claimed but the running stamp hasn't landed yet — the
    // report-beats-stamp window round 1 left open for siblings.
    let mut task = plan_task(&boot.wave_id, "unstamped", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // Sibling worker in the SAME wave whose payload binds a DIFFERENT
    // idempotency key — it echoes this task's id in its report.
    let sibling = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({ "idempotency_key": "some-other-task" }),
        })
        .await
        .expect("sibling worker card");
    boot.card_role_cache
        .insert(sibling.id.clone(), CardRole::Worker, boot.wave_id.clone());
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    seed_runtime_session_in_pool(
        &pool,
        sibling.id.as_str(),
        "sibling-session",
        "sibling-thread",
    )
    .await;
    let sibling_identity = ToolCallIdentity {
        card_id: sibling.id.as_str().to_string(),
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id: "sibling-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "sibling-thread".into(),
    };

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        sibling_identity.clone(),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect_err("sibling without the payload binding must be rejected on an unstamped row");
    let row = task_row(&boot, "unstamped").await;
    assert_eq!(row.status, TaskStatus::Dispatched, "row untouched");
    assert_eq!(row.worker_card_id, None, "no stamp stolen");
    assert!(
        event_rows(&boot, "task.completed").await.is_empty(),
        "rejected report persists nothing"
    );

    // Same on the fail path.
    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        sibling_identity,
        json!({ "idempotency_key": task_id, "reason": "not mine" }),
    )
    .await
    .expect_err("sibling fail report must be rejected");
    assert_eq!(
        task_row(&boot, "unstamped").await.status,
        TaskStatus::Dispatched
    );
    assert!(event_rows(&boot, "task.failed").await.is_empty());
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "rejected reports must not promote Working → Reviewing"
    );

    // The card the task's worker op actually targets flips the
    // unstamped row and stamps itself — the legitimate
    // report-beats-stamp path survives (round-4 F1: the op target, not
    // the payload, is the proof).
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect("owning card's report");
    let row = task_row(&boot, "unstamped").await;
    assert_eq!(row.status, TaskStatus::Done);
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str())
    );
}

// ---------------------------------------------------------------------------
// Review round 4 — F1/F2: card payloads are mutable
// (`PATCH /api/cards/{id}`), so a payload that CLAIMS the task's key is
// not ownership — only the worker op's immutable target card is
// ---------------------------------------------------------------------------

#[tokio::test]
async fn forged_payload_sibling_report_rejected_without_op_target() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    // Dispatched + unstamped: the report-beats-running-stamp window.
    let mut task = plan_task(&boot.wave_id, "forged", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    // The real spawn's op row targets the boot worker card.
    seed_worker_op_target(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;

    // Same-wave sibling whose payload was PATCHed to claim THIS task's
    // idempotency key — the round-2 payload-comparison proof would have
    // accepted it; no worker op targets it.
    let sibling = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.wave_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({ "idempotency_key": task_id }),
        })
        .await
        .expect("forged sibling card");
    boot.card_role_cache
        .insert(sibling.id.clone(), CardRole::Worker, boot.wave_id.clone());
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    seed_runtime_session_in_pool(
        &pool,
        sibling.id.as_str(),
        "forged-session",
        "forged-thread",
    )
    .await;
    let sibling_identity = ToolCallIdentity {
        card_id: sibling.id.as_str().to_string(),
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id: "forged-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "forged-thread".into(),
    };

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        sibling_identity.clone(),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect_err("forged payload without an op target must be rejected");
    let row = task_row(&boot, "forged").await;
    assert_eq!(row.status, TaskStatus::Dispatched, "row untouched");
    assert_eq!(row.worker_card_id, None, "no stamp stolen");
    assert!(
        event_rows(&boot, "task.completed").await.is_empty(),
        "rejected forged report persists nothing"
    );

    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        sibling_identity,
        json!({ "idempotency_key": task_id, "reason": "forged" }),
    )
    .await
    .expect_err("forged fail report must be rejected");
    assert_eq!(
        task_row(&boot, "forged").await.status,
        TaskStatus::Dispatched
    );
    assert!(event_rows(&boot, "task.failed").await.is_empty());
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "rejected forged reports must not promote Working → Reviewing"
    );

    // The card the op actually targets reports fine — no payload
    // binding needed: ownership comes from the op row alone.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect("op-target card's report");
    let row = task_row(&boot, "forged").await;
    assert_eq!(row.status, TaskStatus::Done);
    assert_eq!(
        row.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str())
    );
}

#[tokio::test]
async fn forged_payload_terminal_exit_rejected_without_op_target() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "forged-term", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    // Real worker terminal + its op target.
    let (real_card_id, real_terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    seed_worker_op_target(&boot, "terminal-worker", &task_id, real_card_id.as_str()).await;
    // Forged terminal card whose payload claims the same key — round-4
    // F2: `on_terminal_exit` finds the task from this payload, but no
    // worker op targets the card, so its exit must prove nothing.
    let (_forged_card_id, forged_terminal_id) = seed_terminal_worker(&boot, &task_id).await;

    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    hook.on_terminal_exit(&forged_terminal_id, Some(0), false)
        .await;

    let row = task_row(&boot, "forged-term").await;
    assert_eq!(
        row.status,
        TaskStatus::Running,
        "forged terminal exit must not terminalize the unstamped row"
    );
    assert_eq!(row.worker_card_id, None, "no stamp stolen");
    assert!(
        event_rows(&boot, "task.completed").await.is_empty(),
        "rejected forged exit persists nothing"
    );

    // The real worker's exit still completes the task.
    hook.on_terminal_exit(&real_terminal_id, Some(0), false)
        .await;
    let row = task_row(&boot, "forged-term").await;
    assert_eq!(row.status, TaskStatus::Done);
    assert_eq!(row.worker_card_id.as_deref(), Some(real_card_id.as_str()));
    assert_eq!(event_rows(&boot, "task.completed").await.len(), 1);
}

// ---------------------------------------------------------------------------
// Review round 5 — F2: an op row under the task's idempotency key whose
// persisted payload actor is NOT KernelDispatcher (a legacy
// `calm.task.dispatch` spawn) proves nothing — its worker card cannot
// flip the plan task during the unstamped `dispatched` window
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_actor_op_does_not_prove_unstamped_ownership() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    // Dispatched + unstamped: the window the scheduler has not yet
    // classified the payload conflict as spawn-failed.
    let mut task = plan_task(&boot.wave_id, "legacy-owned", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    // A legacy `calm.task.dispatch` operation created by a spec reusing
    // the same idempotency key: kind + key + card target all match the
    // scheduler shape, but the persisted payload actor is the spec card
    // — NOT KernelDispatcher.
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target_with_payload(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
        json!({
            "actor": ActorId::AiSpec(boot.spec_card_id.clone()),
            "wave_id": boot.wave_id.as_str()
        }),
    )
    .await;

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect_err("a legacy-actor op's card must not flip the unstamped row");
    let row = task_row(&boot, "legacy-owned").await;
    assert_eq!(row.status, TaskStatus::Dispatched, "row untouched");
    assert_eq!(row.worker_card_id, None, "no stamp stolen");
    assert!(
        event_rows(&boot, "task.completed").await.is_empty(),
        "rejected report persists nothing"
    );

    // Fail path is guarded identically.
    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "reason": "not the scheduler's worker" }),
    )
    .await
    .expect_err("legacy-actor fail report must be rejected");
    assert_eq!(
        task_row(&boot, "legacy-owned").await.status,
        TaskStatus::Dispatched
    );
    assert!(event_rows(&boot, "task.failed").await.is_empty());
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "rejected reports must not promote Working → Reviewing"
    );
}

// ---------------------------------------------------------------------------
// Review round 2 — F3 case (i): legacy `calm.task.dispatch` reports
// (no tasks row for the key) keep today's emit behavior
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_report_without_task_row_still_emits() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    // No tasks row exists for this key, and the boot worker card's
    // payload carries no binding — the legacy dispatch shape.
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": "legacy-dispatch-key", "result": { "ok": true } }),
    )
    .await
    .expect("legacy report must keep succeeding");
    let completed = event_rows(&boot, "task.completed").await;
    assert_eq!(completed.len(), 1, "event persisted exactly as before");
    // ... including the Working → Reviewing first-report promotion.
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);
}

// ---------------------------------------------------------------------------
// Review round 6 — a legacy `calm.task.dispatch` key colliding with a
// still-`pending` plan row: the guarded flip could never have matched
// (`status IN ('dispatched','running')`), so the 0-row outcome carries
// no ownership signal — the legacy report must keep emitting and the
// pending row must stay untouched (no Forbidden, no stamp)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_report_with_pending_task_row_still_emits() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    // Pending plan row under the key; legacy-style worker — no
    // worker-spawn op target, no payload binding (owns_key = false).
    let task = plan_task(&boot.wave_id, "pending-collide", TaskKind::Codex, &[]);
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": { "ok": true } }),
    )
    .await
    .expect("legacy complete report against a pending row must keep succeeding");
    assert_eq!(
        event_rows(&boot, "task.completed").await.len(),
        1,
        "task.completed persisted exactly as before"
    );

    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "reason": "legacy retry" }),
    )
    .await
    .expect("legacy fail report against a pending row must keep succeeding");
    assert_eq!(
        event_rows(&boot, "task.failed").await.len(),
        1,
        "task.failed persisted exactly as before"
    );

    let row = task_row(&boot, "pending-collide").await;
    assert_eq!(row.status, TaskStatus::Pending, "plan row never flips");
    assert_eq!(row.worker_card_id, None, "no ownership stamp");
    assert_eq!(row.finished_at_ms, None, "no terminal timestamps");
}

// ---------------------------------------------------------------------------
// Review round 3 — F1: a foreign operation owning the task's idempotency
// key with a DIFFERENT payload is a PERMANENT spawn error — fail the
// task and free the wave budget instead of retrying forever
// ---------------------------------------------------------------------------

#[tokio::test]
async fn foreign_idempotency_conflict_fails_task_and_frees_budget() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let legacy = plan_task(&boot.wave_id, "legacy", TaskKind::Codex, &[]);
    let legacy_id = legacy.id.clone();
    seed_projected_task(&boot, legacy).await;
    seed_projected_task(
        &boot,
        plan_task(&boot.wave_id, "next", TaskKind::Codex, &[]),
    )
    .await;

    // A legacy/foreign operation already holds (codex-worker, task id)
    // with a payload the scheduler's deterministic payload can never
    // hash-match — every submit returns the idempotency conflict.
    let op_repo = SqlxOperationRepo::new(boot.repo.sqlite_pool().expect("sqlite pool"));
    op_repo
        .insert_operation(
            "codex-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(legacy_id.clone()),
                payload_hash: "legacy-foreign-hash".into(),
            },
            json!({ "legacy": true }),
        )
        .await
        .expect("pre-insert foreign op under the task id");

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;

    // PERMANENT classification: the same spawn-failure path as an op
    // Failed/Stuck outcome — guarded failed('spawn-failed') + kernel
    // task.failed — not the log-and-leave-for-sweep transient path.
    let row = task_row(&boot, "legacy").await;
    assert_eq!(
        row.status,
        TaskStatus::Failed,
        "idempotency payload conflict must terminalize the row"
    );
    assert_eq!(row.status_detail.as_deref(), Some("spawn-failed"));
    assert!(row.finished_at_ms.is_some());
    let failed = event_rows(&boot, "task.failed").await;
    assert_eq!(failed.len(), 1, "kernel task.failed pushed for the spec");
    assert!(failed[0].0.contains("KernelDispatcher"));
    assert_eq!(failed[0].1["idempotency_key"], json!(legacy_id));
    let reason = failed[0].1["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("already used with different payload"),
        "reason carries the conflict, got {reason:?}"
    );
    // The foreign operation row itself is untouched.
    assert_eq!(operation_count(&boot, "codex-worker").await, 1);

    // Budget freed (kernel default 1): the second pending task now
    // dispatches instead of the wave stalling behind the dead row.
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert_eq!(
        task_row(&boot, "next").await.status,
        TaskStatus::Running,
        "freed budget admits the next pending task"
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 2);
}

// ---------------------------------------------------------------------------
// Review round 3 — F2: backstop sweeps (reconcile tick / Lagged) no-op
// until the boot sweep completes (recovery → scheduler boot order)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backstop_sweep_noops_until_boot_sweep_completes() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    // Claimed pre-crash; the worker op was never inserted — exactly the
    // row an early tick would re-drive against unrecovered op state.
    let mut task = plan_task(&boot.wave_id, "early", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    seed_task(&boot, task).await;
    let (_runtime, scheduler) = build_scheduler_unbooted(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        Arc::new(tokio::sync::Semaphore::new(8)),
    );
    assert!(!scheduler.boot_sweep_completed());

    // A reconcile tick (or Lagged sweep) firing during boot must no-op.
    scheduler.sweep_all().await;
    assert_eq!(
        operation_count(&boot, "codex-worker").await,
        0,
        "gated backstop sweep must not submit operations"
    );
    assert_eq!(
        task_row(&boot, "early").await.status,
        TaskStatus::Dispatched,
        "gated backstop sweep must not move rows"
    );

    // The earlier context boot sweep opens its independent admission
    // gate before the scheduler boot funnel reconciles dispatched rows.
    scheduler.open_context_sweep_gate().await;
    scheduler.sweep_boot().await;
    assert!(scheduler.boot_sweep_completed());
    assert_eq!(operation_count(&boot, "codex-worker").await, 1);
    assert_eq!(task_row(&boot, "early").await.status, TaskStatus::Running);

    // Post-boot ticks sweep for real (and stay idempotent).
    scheduler.sweep_all().await;
    assert_eq!(operation_count(&boot, "codex-worker").await, 1);
}

// ---------------------------------------------------------------------------
// PR-C — task-verify gate runner (real /bin/sh gates on parked operations)
//
// Coverage map (brief §7 / design § → test):
//   green gate → done + TaskGateResult(passed) + promotion —
//     `green_gate_flips_verifying_to_done_and_promotes`.
//   red gate → failed('gate-red') + failing_step + log_tail —
//     `red_gate_fails_with_failing_step_and_log_tail`.
//   timeout → group killed + 'gate-timeout' —
//     `gate_timeout_group_kills_and_fails_gate_timeout`.
//   kill-prior (recorded triple) — `gate_spawn_kills_prior_recorded_group`.
//   parked-op boot liveness (dead, no outcome → per-#653 handling +
//     consumer reconcile copy) —
//     `parked_gate_dead_at_boot_fails_op_and_row_reconciles_gate_infra`.
//   §6.5 suppression predicate — `gated_self_report_predicate`.
//
// Real processes are spawned (POSIX sh, sleep) — serialized behind one
// lock like the dispatcher daemon-spawn tests (CI flake limits).
// ---------------------------------------------------------------------------

static GATE_SPAWN_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unique_gate_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "neige-gate-test-{tag}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).expect("gate dir");
    dir
}

fn gate_task(boot: &Boot, key: &str, gate_json: &str) -> Task {
    let mut task = plan_task(&boot.wave_id, key, TaskKind::Codex, &[]);
    task.status = TaskStatus::Verifying;
    task.gate_json = Some(gate_json.to_string());
    task
}

async fn wait_for_terminal_row(boot: &Boot, key: &str, timeout_secs: u64) -> Task {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        let row = task_row(boot, key).await;
        if matches!(row.status, TaskStatus::Done | TaskStatus::Failed) {
            return row;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "task {key} did not reach a terminal status in {timeout_secs}s: {row:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn wave_lifecycle(boot: &Boot) -> WaveLifecycle {
    boot.repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .lifecycle
}

#[tokio::test]
async fn green_gate_flips_verifying_to_done_and_promotes() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("green");
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [
            { "name": "hello", "cmd": "echo gate-says-hello" },
            { "name": "check", "cmd": "test -d ." }
        ]
    })
    .to_string();
    let task = gate_task(&boot, "green", &gate);
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "green", 30).await;

    assert_eq!(row.status, TaskStatus::Done, "{row:?}");
    assert_eq!(row.status_detail, None);
    assert_eq!(row.gate_attempt, 1);
    assert!(row.gate_pid.is_none(), "pid triple cleared by the flip");
    assert!(row.finished_at_ms.is_some());
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["passed"], true, "{verdict}");
    assert_eq!(verdict["exit_code"], 0);
    assert_eq!(verdict["attempt"], 1);
    assert!(
        verdict["log_tail"]
            .as_str()
            .unwrap()
            .contains("gate-says-hello"),
        "{verdict}"
    );

    // The §6.5 event landed, actor KernelDispatcher, passed=true.
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, data) = &rows[0];
    assert!(
        actor.contains("kernel-dispatcher") || actor.contains("KernelDispatcher"),
        "gate result actor must be the kernel dispatcher: {actor}"
    );
    assert_eq!(data["task_id"], task_id.as_str());
    assert_eq!(data["passed"], true);

    // §3: exactly one promotion per gated task, in the gate-result tx.
    assert_eq!(wave_lifecycle(&boot).await, WaveLifecycle::Reviewing);

    // Disk artifacts: full log with sentinels, exit file "0".
    let log = std::fs::read_to_string(dir.join(format!("{task_id}-g1.log"))).unwrap();
    assert!(log.contains("::gate-step hello"), "{log}");
    assert!(log.contains("gate-says-hello"), "{log}");
    let exit = std::fs::read_to_string(dir.join(format!("{task_id}-g1.exit"))).unwrap();
    assert_eq!(exit.trim(), "0");
    std::fs::remove_dir_all(&dir).ok();
}

async fn seed_child_parent(
    boot: &Boot,
    key: &str,
    lifecycle: WaveLifecycle,
    gate_json: Option<String>,
) -> (String, String) {
    let child = boot
        .repo
        .wave_create(NewWave {
            cove_id: boot.cove_id.clone(),
            title: format!("child {key}"),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET parent_wave_id=?1,lifecycle=?2 WHERE id=?3")
        .bind(boot.wave_id.as_str())
        .bind(lifecycle.as_db_str())
        .bind(child.id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut task = plan_task(&boot.wave_id, key, TaskKind::Codex, &[]);
    task.spawn = "sub-wave".into();
    task.status = TaskStatus::Running;
    task.gate_json = gate_json;
    let task_id = task.id.clone();
    seed_task(boot, task).await;
    sqlx::query("UPDATE tasks SET child_wave_id=?1 WHERE id=?2")
        .bind(child.id.as_str())
        .bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    (task_id, child.id.to_string())
}

async fn seed_child_task(boot: &Boot, child_id: &str, key: &str, status: TaskStatus) {
    sqlx::query(
        "INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,status,declared_by,spawn,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,?3,'codex','child work','{}',?4,'spec','in-wave',?5,?5)",
    )
    .bind(format!("{child_id}:{key}")).bind(child_id).bind(key).bind(status)
    .bind(now_ms()).execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
}

#[tokio::test]
async fn acceptance_11_sub_wave_parent_survives_two_timeout_sweeps_without_deadline() {
    let boot = boot().await;
    let (_runtime, scheduler) =
        build_scheduler_with_timeouts(&boot, vec![], Duration::from_millis(1));
    let (task_id, _) = seed_child_parent(&boot, "long-child", WaveLifecycle::Working, None).await;
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_millis(3)).await;
        scheduler.sweep_all().await;
        let row = boot.repo.task_get(&task_id).await.unwrap().unwrap();
        assert_eq!(row.status, TaskStatus::Running);
        assert_eq!(row.running_deadline_ms, None);
    }
}

#[tokio::test]
async fn acceptance_12_sub_wave_running_stamp_has_no_worker_or_deadline() {
    let boot = boot().await;
    let mut task = plan_task(&boot.wave_id, "stamp-child", TaskKind::Codex, &[]);
    task.spawn = "sub-wave".into();
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    sqlx::query("UPDATE tasks SET child_wave_id='child-stamp' WHERE id=?1")
        .bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    calm_server::db::sqlite::task_mark_sub_wave_running_tx(&mut tx, &task_id, now_ms())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let row = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(row.worker_card_id, None);
    assert_eq!(row.running_deadline_ms, None);

    for (key, status, spawn, child_wave_id, guard) in [
        (
            "stamp-failed",
            TaskStatus::Failed,
            "sub-wave",
            Some("child-failed"),
            "status='dispatched'",
        ),
        (
            "stamp-in-wave",
            TaskStatus::Dispatched,
            "in-wave",
            Some("child-in-wave"),
            "spawn='sub-wave'",
        ),
        (
            "stamp-no-child",
            TaskStatus::Dispatched,
            "sub-wave",
            None,
            "child_wave_id IS NOT NULL",
        ),
    ] {
        let mut task = plan_task(&boot.wave_id, key, TaskKind::Codex, &[]);
        task.status = status;
        task.spawn = spawn.into();
        let id = task.id.clone();
        seed_task(&boot, task).await;
        if let Some(child_wave_id) = child_wave_id {
            sqlx::query("UPDATE tasks SET child_wave_id=?1 WHERE id=?2")
                .bind(child_wave_id)
                .bind(&id)
                .execute(&boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
        }
        let pool = boot.repo.sqlite_pool().unwrap();
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        let changed =
            calm_server::db::sqlite::task_mark_sub_wave_running_tx(&mut tx, &id, now_ms())
                .await
                .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(changed, 0, "guard {guard} must reject the stamp");
        assert_eq!(
            boot.repo.task_get(&id).await.unwrap().unwrap().status,
            status,
            "guard {guard} must preserve the existing status"
        );
    }
}

#[tokio::test]
async fn acceptance_13_done_quiescent_child_routes_parent_through_gate() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    let dir = unique_gate_dir("child-close");
    let (_runtime, scheduler) =
        build_scheduler(&boot, vec![Arc::new(TaskVerifyAdapter::new(dir.clone()))]);
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps":[{"name":"ok","cmd":"true"}]
    })
    .to_string();
    let (task_id, child) =
        seed_child_parent(&boot, "gated-child", WaveLifecycle::Done, Some(gate)).await;
    scheduler
        .reconcile_child_wave_for_test(&child)
        .await
        .unwrap();
    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Verifying
    );
    assert_eq!(event_rows(&boot, "task.completed").await.len(), 1);
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "gated-child", 30).await;
    assert_eq!(row.status, TaskStatus::Done, "{row:?}");
    assert_eq!(row.gate_attempt, 1, "the parent gate actually ran");
    assert_eq!(operation_count(&boot, "task-verify").await, 1);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn acceptance_13b_and_13c_inflight_child_blocks_then_eventually_closes_parent() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let gate_without_cwd = json!({"steps":[{"name":"parent","cmd":"true"}]}).to_string();
    let (task_id, child) = seed_child_parent(
        &boot,
        "interleaved-child",
        WaveLifecycle::Done,
        Some(gate_without_cwd.clone()),
    )
    .await;
    seed_child_task(&boot, &child, "gate-still-running", TaskStatus::Verifying).await;
    sqlx::query("UPDATE tasks SET gate_json=?1 WHERE wave_id=?2")
        .bind(&gate_without_cwd)
        .bind(&child)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    scheduler
        .reconcile_child_wave_for_test(&child)
        .await
        .unwrap();
    let blocked = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(blocked.status, TaskStatus::Running);
    assert_eq!(blocked.gate_attempt, 0, "parent gate must not start early");
    assert_eq!(operation_count(&boot, "task-verify").await, 0);
    sqlx::query("UPDATE tasks SET status='done' WHERE wave_id=?1")
        .bind(&child)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    scheduler
        .reconcile_child_wave_for_test(&child)
        .await
        .unwrap();
    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Verifying
    );
}

#[tokio::test]
async fn acceptance_13d_done_child_with_pending_block_fails_with_count() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let (task_id, child) =
        seed_child_parent(&boot, "pending-child", WaveLifecycle::Done, None).await;
    seed_child_task(&boot, &child, "left", TaskStatus::Pending).await;
    scheduler
        .reconcile_child_wave_for_test(&child)
        .await
        .unwrap();
    let row = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("child-wave-incomplete"));
    assert!(
        event_rows(&boot, "task.failed").await[0].1["reason"]
            .as_str()
            .unwrap()
            .contains("1 pending")
    );
}

#[tokio::test]
async fn acceptance_14_failed_canceled_and_deleted_child_have_distinct_parent_reasons() {
    for (lifecycle, expected) in [
        (WaveLifecycle::Failed, "child-wave-failed"),
        (WaveLifecycle::Canceled, "child-wave-canceled"),
    ] {
        let boot = boot().await;
        let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
        let (task_id, child) = seed_child_parent(&boot, expected, lifecycle, None).await;
        scheduler
            .reconcile_child_wave_for_test(&child)
            .await
            .unwrap();
        assert_eq!(
            boot.repo
                .task_get(&task_id)
                .await
                .unwrap()
                .unwrap()
                .status_detail
                .as_deref(),
            Some(expected)
        );
    }
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let (task_id, child) = seed_child_parent(&boot, "deleted", WaveLifecycle::Working, None).await;
    sqlx::query("DELETE FROM waves WHERE id=?1")
        .bind(&child)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    scheduler
        .reconcile_child_wave_for_test(&child)
        .await
        .unwrap();
    assert_eq!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .status_detail
            .as_deref(),
        Some("child-wave-deleted")
    );
}

#[tokio::test]
async fn acceptance_15_lost_event_sweep_closes_child_parent() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let (task_id, _) = seed_child_parent(&boot, "sweep", WaveLifecycle::Done, None).await;
    scheduler.sweep_all().await;
    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Done
    );
}

#[tokio::test]
async fn acceptance_16_live_and_sweep_use_the_same_guarded_conclusion() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let (live_task, live_child) =
        seed_child_parent(&boot, "live", WaveLifecycle::Failed, None).await;
    let (sweep_task, _) = seed_child_parent(&boot, "sweep2", WaveLifecycle::Failed, None).await;
    scheduler.reconcile_child_wave(live_child.clone().into());
    for _ in 0..100 {
        if boot
            .repo
            .task_get(&live_task)
            .await
            .unwrap()
            .unwrap()
            .status
            == TaskStatus::Failed
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        boot.repo
            .task_get(&live_task)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Failed,
        "the live trigger must conclude before the sweep runs"
    );
    scheduler.sweep_all().await;
    let live = boot.repo.task_get(&live_task).await.unwrap().unwrap();
    let sweep = boot.repo.task_get(&sweep_task).await.unwrap().unwrap();
    assert_eq!(
        (live.status, live.status_detail),
        (sweep.status, sweep.status_detail)
    );
}

#[tokio::test]
async fn acceptance_18_success_flip_rechecks_done_after_its_snapshot() {
    for mutation in ["delete", "reopen"] {
        let boot = boot().await;
        let (task_id, child) = seed_child_parent(&boot, mutation, WaveLifecycle::Done, None).await;
        let pool = boot.repo.sqlite_pool().unwrap();
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        let observed: String = sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id=?1")
            .bind(&child)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(observed, "done", "fixture must first observe Done");
        if mutation == "delete" {
            sqlx::query("DELETE FROM waves WHERE id=?1")
                .bind(&child)
                .execute(&mut *tx)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE waves SET lifecycle='planning' WHERE id=?1")
                .bind(&child)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        let changed =
            calm_server::scheduler::guarded_child_success_flip_for_test(&mut tx, &task_id, &child)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(changed, 0, "{mutation} must lose the guarded flip");
        assert_eq!(
            boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }
}

#[tokio::test]
async fn acceptance_18_production_reconcile_keeps_the_child_guard_wired() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let (task_id, child) =
        seed_child_parent(&boot, "production-guard", WaveLifecycle::Done, None).await;

    // This drives the real reconcile_child_wave_task entry and changes the child
    // after its advisory snapshot but before the production flip call. Today both
    // happen in one BEGIN IMMEDIATE transaction, so concurrent writers cannot make
    // this guard load-bearing. If a future refactor splits the snapshot and flip
    // across transactions, the guard becomes the correctness boundary immediately;
    // this fixture also ensures removing it from the production call site fails now.
    scheduler.reopen_child_after_reconcile_snapshot_for_test();
    scheduler
        .reconcile_child_wave_for_test(&child)
        .await
        .unwrap();

    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Running,
        "the production guarded flip must reject the stale Done snapshot"
    );
}

#[tokio::test]
async fn acceptance_18_incomplete_flip_rechecks_done_after_its_snapshot() {
    for mutation in ["delete", "reopen"] {
        let boot = boot().await;
        let (task_id, child) = seed_child_parent(&boot, mutation, WaveLifecycle::Done, None).await;
        seed_child_task(&boot, &child, "left", TaskStatus::Pending).await;
        let pool = boot.repo.sqlite_pool().unwrap();
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        let observed: String = sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id=?1")
            .bind(&child)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(observed, "done", "fixture must first observe Done");
        if mutation == "delete" {
            sqlx::query("DELETE FROM waves WHERE id=?1")
                .bind(&child)
                .execute(&mut *tx)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE waves SET lifecycle='planning' WHERE id=?1")
                .bind(&child)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        let changed = calm_server::scheduler::guarded_child_incomplete_flip_for_test(
            &mut tx,
            &task_id,
            boot.wave_id.as_str(),
            &child,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(changed, 0, "{mutation} must lose the incomplete flip");
        assert_eq!(
            boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }
}

#[tokio::test]
async fn acceptance_18_terminal_flip_rechecks_all_three_outcomes_after_its_snapshot() {
    for (label, seed_lifecycle, expected_lifecycle) in [
        ("deleted", WaveLifecycle::Working, None),
        ("failed", WaveLifecycle::Failed, Some(WaveLifecycle::Failed)),
        (
            "canceled",
            WaveLifecycle::Canceled,
            Some(WaveLifecycle::Canceled),
        ),
    ] {
        let boot = boot().await;
        let (task_id, child) = seed_child_parent(&boot, label, seed_lifecycle, None).await;
        let pool = boot.repo.sqlite_pool().unwrap();
        if label == "deleted" {
            sqlx::query("DELETE FROM waves WHERE id=?1")
                .bind(&child)
                .execute(&pool)
                .await
                .unwrap();
        }

        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        let observed: Option<String> =
            sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id=?1")
                .bind(&child)
                .fetch_optional(&mut *tx)
                .await
                .unwrap();
        assert_eq!(
            observed.as_deref(),
            expected_lifecycle.map(WaveLifecycle::as_db_str),
            "fixture must first observe the selected {label} outcome"
        );

        if label == "deleted" {
            sqlx::query(
                "INSERT INTO waves(id,cove_id,title,sort,cwd,created_at,updated_at) \
                 SELECT ?1,cove_id,'replacement child',sort+0.25,cwd,?2,?2 \
                   FROM waves WHERE id=?3",
            )
            .bind(&child)
            .bind(now_ms())
            .bind(boot.wave_id.as_str())
            .execute(&mut *tx)
            .await
            .unwrap();
        } else {
            sqlx::query("UPDATE waves SET lifecycle='planning' WHERE id=?1")
                .bind(&child)
                .execute(&mut *tx)
                .await
                .unwrap();
        }

        let changed = calm_server::scheduler::guarded_child_terminal_flip_for_test(
            &mut tx,
            &task_id,
            boot.wave_id.as_str(),
            &child,
            expected_lifecycle,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(changed, 0, "{label} snapshot must lose after child flip");
        assert_eq!(
            boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Running,
            "{label} snapshot must not close the parent"
        );
    }
}

#[tokio::test]
async fn acceptance_19_child_bootstrap_is_before_running_and_exactly_once_after_redrive() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "bootstrap", TaskKind::Codex, &[]);
    task.spawn = "sub-wave".into();
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    let child_payload = build_child_wave_payload(&task).unwrap();
    seed_task(&boot, task).await;
    let minted = Arc::new(AtomicUsize::new(0));
    let child_adapter = Arc::new(ChildWaveAdapter::new(
        boot.card_role_cache.clone(),
        boot.wave_cove_cache.clone(),
    )) as Arc<dyn ProviderAdapter>;
    let block = BootstrapBlockHook {
        wait_entered: Arc::new(tokio::sync::Notify::new()),
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let bootstrap_adapter = Arc::new(BootstrapAdapter::new_blocking(
        minted.clone(),
        block.clone(),
    )) as Arc<dyn ProviderAdapter>;
    let (runtime, scheduler) = build_scheduler(&boot, vec![child_adapter, bootstrap_adapter]);

    // Crash point 1: child-wave committed, before bootstrap submit and the
    // parent running flip. Recovery must consume the durable child op.
    let child_op = runtime
        .submit(
            "child-wave",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task_id.clone()),
                payload_hash: stable_payload_hash(&child_payload).unwrap(),
            },
            child_payload,
        )
        .await
        .unwrap();
    let child_result = runtime.wait(&child_op).await.unwrap();
    assert!(matches!(
        child_result.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    assert_eq!(operation_count(&boot, "spec-harness-start").await, 0);
    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Dispatched
    );
    runtime.install_wait_entered_hook_for_test(block.wait_entered.clone());
    let sweep = {
        let scheduler = scheduler.clone();
        tokio::spawn(async move { scheduler.sweep_all().await })
    };
    tokio::time::timeout(Duration::from_secs(1), block.entered.notified())
        .await
        .expect("bootstrap adapter must reach its blocking hook");
    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Dispatched,
        "bootstrap has not completed, so running must not be visible"
    );
    block.release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), sweep)
        .await
        .expect("bootstrap sweep must not hang")
        .unwrap();
    let row = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(row.running_deadline_ms, None);
    let child_id: String = sqlx::query_scalar("SELECT child_wave_id FROM tasks WHERE id=?1")
        .bind(&task_id)
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let child_lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id=?1")
        .bind(&child_id)
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_ne!(child_lifecycle, "draft");
    assert_eq!(minted.load(Ordering::SeqCst), 1);

    // Crash point 2: bootstrap committed and running was stamped, but the
    // caller did not observe completion. A dispatched-shaped restart must
    // dedupe both operations and must not mint a replacement runtime.
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
        .bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    scheduler.sweep_all().await;
    assert_eq!(operation_count(&boot, "child-wave").await, 1);
    assert_eq!(operation_count(&boot, "spec-harness-start").await, 1);
    assert_eq!(minted.load(Ordering::SeqCst), 1);
    let idem: String = sqlx::query_scalar(
        "SELECT idempotency_key FROM operations WHERE kind='spec-harness-start'",
    )
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(idem, format!("child-wave:{child_id}:bootstrap"));

    // Crash while bootstrap is blocked after its prepare transaction. Drop
    // the first runtime, recover the durable operation with a new runtime,
    // and prove both operation rows and the harness mint remain exactly once.
    let crash_boot = self::boot().await;
    set_lifecycle(&crash_boot, WaveLifecycle::Working).await;
    let mut crash_task = plan_task(&crash_boot.wave_id, "bootstrap-crash", TaskKind::Codex, &[]);
    crash_task.spawn = "sub-wave".into();
    crash_task.status = TaskStatus::Dispatched;
    let crash_task_id = crash_task.id.clone();
    seed_task(&crash_boot, crash_task).await;
    let crash_minted = Arc::new(AtomicUsize::new(0));
    let crash_block = BootstrapBlockHook {
        wait_entered: Arc::new(tokio::sync::Notify::new()),
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let (crash_runtime, crash_scheduler) = build_scheduler(
        &crash_boot,
        vec![
            Arc::new(ChildWaveAdapter::new(
                crash_boot.card_role_cache.clone(),
                crash_boot.wave_cove_cache.clone(),
            )),
            Arc::new(BootstrapAdapter::new_blocking(
                crash_minted.clone(),
                crash_block.clone(),
            )),
        ],
    );
    crash_runtime.install_wait_entered_hook_for_test(crash_block.wait_entered.clone());
    let crashed_sweep = {
        let scheduler = crash_scheduler.clone();
        tokio::spawn(async move { scheduler.sweep_all().await })
    };
    tokio::time::timeout(Duration::from_secs(1), crash_block.entered.notified())
        .await
        .expect("crash fixture must block during bootstrap");
    assert_eq!(
        crash_boot
            .repo
            .task_get(&crash_task_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Dispatched
    );
    assert_eq!(operation_count(&crash_boot, "child-wave").await, 1);
    assert_eq!(operation_count(&crash_boot, "spec-harness-start").await, 1);
    assert_eq!(crash_minted.load(Ordering::SeqCst), 1);
    crashed_sweep.abort();
    assert!(crashed_sweep.await.unwrap_err().is_cancelled());
    drop(crash_scheduler);
    drop(crash_runtime);

    let (_recovered_runtime, recovered_scheduler) = build_scheduler(
        &crash_boot,
        vec![
            Arc::new(ChildWaveAdapter::new(
                crash_boot.card_role_cache.clone(),
                crash_boot.wave_cove_cache.clone(),
            )),
            Arc::new(BootstrapAdapter::new(crash_minted.clone())),
        ],
    );
    let recovered_sweep = {
        let scheduler = recovered_scheduler.clone();
        tokio::spawn(async move { scheduler.sweep_all().await })
    };
    assert_eq!(
        crash_boot
            .repo
            .task_get(&crash_task_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Dispatched
    );
    crash_block.release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), recovered_sweep)
        .await
        .expect("recovered bootstrap sweep must not hang")
        .unwrap();
    assert_eq!(
        crash_boot
            .repo
            .task_get(&crash_task_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(operation_count(&crash_boot, "child-wave").await, 1);
    assert_eq!(operation_count(&crash_boot, "spec-harness-start").await, 1);
    assert_eq!(crash_minted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn acceptance_13e_failed_and_stuck_at_both_operation_levels_close_once() {
    for (stage, phase, expected) in [
        ("create", "failed", "child-wave-create-failed"),
        ("create", "stuck", "child-wave-create-stuck"),
        ("bootstrap", "failed", "child-wave-bootstrap-failed"),
        ("bootstrap", "stuck", "child-wave-bootstrap-stuck"),
    ] {
        let boot = boot().await;
        set_lifecycle(&boot, WaveLifecycle::Working).await;
        let mut task = plan_task(
            &boot.wave_id,
            &format!("{stage}-{phase}"),
            TaskKind::Codex,
            &[],
        );
        task.spawn = "sub-wave".into();
        if stage == "create" {
            task.status = TaskStatus::Dispatched;
        }
        let task_id = task.id.clone();
        seed_projected_task(&boot, task).await;
        let minted = Arc::new(AtomicUsize::new(0));
        let child_adapter = Arc::new(ChildWaveAdapter::new(
            boot.card_role_cache.clone(),
            boot.wave_cove_cache.clone(),
        )) as Arc<dyn ProviderAdapter>;
        let bootstrap_adapter = Arc::new(BootstrapAdapter::new(minted)) as Arc<dyn ProviderAdapter>;
        let (_runtime, scheduler) = build_scheduler(&boot, vec![child_adapter, bootstrap_adapter]);
        if stage == "create" {
            // Drive the real child adapter first. This preserves the
            // production post-commit tx_output and child_wave_id that the
            // failure cleanup must discover from durable task state.
            scheduler.sweep_all().await;
            assert_eq!(
                boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
                TaskStatus::Running
            );
            sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
                .bind(&task_id)
                .execute(&boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
            let (detail, last_error) = if phase == "stuck" {
                (json!({"reason":"injected","since":1}).to_string(), None)
            } else {
                (
                    json!({"from_phase":"tx_committed"}).to_string(),
                    Some("injected"),
                )
            };
            sqlx::query(
                "UPDATE operations SET phase=?1,phase_detail_json=?2,last_error=?3 \
                 WHERE kind='child-wave' AND idempotency_key=?4",
            )
            .bind(phase)
            .bind(detail)
            .bind(last_error)
            .bind(&task_id)
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        } else {
            scheduler.schedule_wave(boot.wave_id.clone()).await;
            sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
                .bind(&task_id)
                .execute(&boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
            let (detail, last_error) = if phase == "stuck" {
                (json!({"reason":"injected","since":1}).to_string(), None)
            } else {
                (
                    json!({"from_phase":"pending"}).to_string(),
                    Some("injected"),
                )
            };
            sqlx::query("UPDATE operations SET phase=?1,phase_detail_json=?2,last_error=?3 WHERE kind='spec-harness-start'")
                .bind(phase).bind(detail).bind(last_error)
                .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
        }
        scheduler.sweep_all().await;
        let row = boot.repo.task_get(&task_id).await.unwrap().unwrap();
        assert_eq!(row.status, TaskStatus::Failed, "{stage}/{phase}");
        assert_eq!(
            row.status_detail.as_deref(),
            Some(expected),
            "{stage}/{phase}"
        );
        if stage == "create" {
            let child_id: String =
                sqlx::query_scalar("SELECT child_wave_id FROM tasks WHERE id=?1")
                    .bind(&task_id)
                    .fetch_one(&boot.repo.sqlite_pool().unwrap())
                    .await
                    .unwrap();
            let child_lifecycle: String =
                sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id=?1")
                    .bind(&child_id)
                    .fetch_one(&boot.repo.sqlite_pool().unwrap())
                    .await
                    .unwrap();
            assert_eq!(
                child_lifecycle, "failed",
                "post-commit {phase} must close the durable child"
            );
            boot.repo.wave_delete(&child_id).await.unwrap();
            boot.repo.wave_delete(boot.wave_id.as_str()).await.unwrap();
            assert!(boot.repo.wave_get(&child_id).await.unwrap().is_none());
            assert!(
                boot.repo
                    .wave_get(boot.wave_id.as_str())
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        scheduler.sweep_all().await;
        assert_eq!(
            event_rows(&boot, "task.failed").await.len(),
            1,
            "{stage}/{phase} restart idempotency"
        );
    }
}

#[tokio::test]
async fn acceptance_3b_claim_frozen_spawn_routes_recovery_without_report_reread() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "frozen-route", TaskKind::Codex, &[]);
    task.spawn = "sub-wave".into();
    let task_id = task.id.clone();
    seed_projected_task(&boot, task).await;
    let minted = Arc::new(AtomicUsize::new(0));
    let adapters = vec![
        Arc::new(ChildWaveAdapter::new(
            boot.card_role_cache.clone(),
            boot.wave_cove_cache.clone(),
        )) as Arc<dyn ProviderAdapter>,
        Arc::new(BootstrapAdapter::new(minted)) as Arc<dyn ProviderAdapter>,
        Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.to_string(),
        }) as Arc<dyn ProviderAdapter>,
    ];
    let (_runtime, scheduler) = build_scheduler(&boot, adapters);
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    assert_eq!(operation_count(&boot, "child-wave").await, 1, "live route");
    assert_eq!(
        operation_count(&boot, "codex-worker").await,
        0,
        "live route"
    );
    // Simulate a post-claim report whose mutable declaration now says in-wave.
    // Recovery never reads it; only the frozen tasks row is authoritative.
    sqlx::query("UPDATE cards SET payload=?1 WHERE wave_id=?2 AND kind='wave-report'")
        .bind(serde_json::to_string(&WaveReportPayload::initial()).unwrap())
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.blocks',json(?1)) WHERE wave_id=?2 AND kind='wave-report'")
        .bind(json!([{"id":"b_route","kind":"task","rev":2,"payload":{"key":"frozen-route","kind":"codex","goal":"changed","ready":true,"declared_by":"spec","spawn":"in-wave"}}]).to_string())
        .bind(boot.wave_id.as_str()).execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
        .bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    scheduler.sweep_all().await;
    assert_eq!(
        operation_count(&boot, "child-wave").await,
        1,
        "recovery route"
    );
    assert_eq!(
        operation_count(&boot, "codex-worker").await,
        0,
        "recovery route"
    );
}

#[tokio::test]
async fn acceptance_3a_claim_frozen_spawn_routes_live_after_post_claim_report_edit() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let report = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: 1,
        summary: String::new(),
        body: String::new(),
        blocks: Some(vec![ReportBlock {
            id: "b_live_route".into(),
            kind: "task".into(),
            rev: 1,
            payload: json!({
                "key":"frozen-live", "kind":"codex", "goal":"delegate",
                "ready":true, "declared_by":"spec", "spawn":"sub-wave"
            }),
        }]),
    };
    insert_report_payload(
        &boot,
        "live-route-report",
        serde_json::to_value(report).unwrap(),
    )
    .await;
    let mut task = plan_task(&boot.wave_id, "frozen-live", TaskKind::Codex, &[]);
    task.spawn = "sub-wave".into();
    seed_task(&boot, task).await;
    let adapters = vec![
        Arc::new(ChildWaveAdapter::new(
            boot.card_role_cache.clone(),
            boot.wave_cove_cache.clone(),
        )) as Arc<dyn ProviderAdapter>,
        Arc::new(BootstrapAdapter::new(Arc::new(AtomicUsize::new(0)))) as Arc<dyn ProviderAdapter>,
    ];
    let (_runtime, scheduler) = build_scheduler(&boot, adapters);
    let claimed = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    scheduler.set_post_claim_drive_test_hook(PostClaimDriveTestHook {
        claimed: claimed.clone(),
        resume: resume.clone(),
    });
    let scheduled = tokio::spawn({
        let scheduler = scheduler.clone();
        let wave = boot.wave_id.clone();
        async move { scheduler.schedule_wave(wave).await }
    });
    claimed.notified().await;
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.docRev',2, \
         '$.blocks[0].rev',2,'$.blocks[0].payload.spawn','in-wave') WHERE id='live-route-report'",
    )
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    resume.notify_one();
    scheduled.await.unwrap();
    assert_eq!(operation_count(&boot, "child-wave").await, 1);
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);
}

#[tokio::test]
async fn acceptance_3c_claim_success_uses_transaction_reread_spawn() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let task = plan_task(&boot.wave_id, "tx-reread", TaskKind::Codex, &[]);
    let task_id = task.id.clone();
    seed_projected_task(&boot, task).await;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = semaphore.clone().acquire_owned().await.unwrap();
    let adapters = vec![
        Arc::new(ChildWaveAdapter::new(
            boot.card_role_cache.clone(),
            boot.wave_cove_cache.clone(),
        )) as Arc<dyn ProviderAdapter>,
        Arc::new(BootstrapAdapter::new(Arc::new(AtomicUsize::new(0)))) as Arc<dyn ProviderAdapter>,
    ];
    let (_runtime, scheduler) = build_scheduler_with_semaphore(&boot, adapters, semaphore);
    let handle = tokio::spawn({
        let scheduler = scheduler.clone();
        let wave = boot.wave_id.clone();
        async move { scheduler.schedule_wave(wave).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    sqlx::query("UPDATE tasks SET spawn='sub-wave' WHERE id=?1 AND status='pending'")
        .bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    drop(permit);
    handle.await.unwrap();
    assert_eq!(operation_count(&boot, "child-wave").await, 1);
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);
}

#[tokio::test]
async fn acceptance_5b_stale_frozen_context_refuses_real_child_operation() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    insert_report_payload(
        &boot,
        "stale-child-report",
        serde_json::to_value(WaveReportPayload::initial()).unwrap(),
    )
    .await;
    let original = json!({
        "key":"stale-child", "kind":"codex", "goal":"frozen child contract",
        "spawn":"sub-wave", "ready":true, "declared_by":"spec"
    });
    edit_report_blocks(&boot, &[("b_stale_child", "task", original.clone())], 0).await;
    let task_id = format!("{}:stale-child", boot.wave_id);

    // Claim through production, but simulate a crash before op insertion by
    // dropping the runtime held only weakly by the scheduler.
    let (runtime, claim_only_scheduler) = build_scheduler(&boot, vec![]);
    drop(runtime);
    claim_only_scheduler
        .schedule_wave(boot.wave_id.clone())
        .await;
    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Dispatched
    );

    let edited = json!({
        "key":"stale-child", "kind":"codex", "goal":"materially edited child contract",
        "spawn":"sub-wave", "ready":true, "declared_by":"spec"
    });
    edit_report_blocks(&boot, &[("b_stale_child", "task", edited)], 1).await;
    let monitor =
        TaskContextMonitor::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(
        boot.repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .context_stale_at_ms
            .is_some()
    );

    let child_adapter = Arc::new(ChildWaveAdapter::new(
        boot.card_role_cache.clone(),
        boot.wave_cove_cache.clone(),
    )) as Arc<dyn ProviderAdapter>;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![child_adapter]);
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    scheduler.sweep_all().await;
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(before, after);
    let failed = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(failed.status, TaskStatus::Failed);
    let stale_after_operation_rejection = failed.context_stale_at_ms;
    assert!(stale_after_operation_rejection.is_some());
    let operation_phase: String = sqlx::query_scalar(
        "SELECT phase FROM operations WHERE kind='child-wave' ORDER BY created_at_ms DESC LIMIT 1",
    )
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(
        operation_phase, "failed",
        "the stale fence must reject the operation"
    );

    edit_report_blocks(&boot, &[("b_stale_child", "task", original)], 2).await;
    monitor
        .detect_wave_edit(boot.wave_id.as_str())
        .await
        .unwrap();
    monitor.sweep().await.unwrap();

    let after_revert = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(
        after_revert.status,
        TaskStatus::Failed,
        "terminal tasks never revive"
    );
    assert_eq!(
        after_revert.context_stale_at_ms, stale_after_operation_rejection,
        "restored content cannot clear the terminal task's stale verdict"
    );
    let index_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_ref_index WHERE task_id=?1")
            .bind(&task_id)
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(
        index_rows, 0,
        "terminal tasks never regain reverse-index rows"
    );
    assert_eq!(
        context_verdicts_for_task(&boot, &task_id).await,
        vec![("material".into(), "content_changed".into())],
        "an unrecoverable terminal task emits no restored verdict"
    );
}

#[tokio::test]
async fn acceptance_9_depth_exhaustion_fails_parent_without_in_wave_fallback() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "depth-fail", TaskKind::Codex, &[]);
    task.spawn = "sub-wave".into();
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
    let payload = build_child_wave_payload(&task).unwrap();
    seed_task(&boot, task).await;
    let op_repo = SqlxOperationRepo::new(boot.repo.sqlite_pool().unwrap());
    let op_id = op_repo
        .insert_operation(
            "child-wave",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task_id.clone()),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET phase='failed',last_error='sub-wave-depth-exceeded',phase_detail_json='{}' WHERE id=?1")
        .bind(op_id).execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    let child_adapter = Arc::new(ChildWaveAdapter::new(
        boot.card_role_cache.clone(),
        boot.wave_cove_cache.clone(),
    )) as Arc<dyn ProviderAdapter>;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![child_adapter]);
    scheduler.sweep_all().await;
    let row = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(
        row.status_detail.as_deref(),
        Some("sub-wave-depth-exceeded")
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 0);
}

#[tokio::test]
async fn red_gate_fails_with_failing_step_and_log_tail() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("red");
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [
            { "name": "ok", "cmd": "true" },
            { "name": "boom", "cmd": "echo failing-out; exit 7" }
        ]
    })
    .to_string();
    seed_task(&boot, gate_task(&boot, "red", &gate)).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "red", 30).await;

    assert_eq!(
        row.status,
        TaskStatus::Failed,
        "gate red is failed: {row:?}"
    );
    assert_eq!(row.status_detail.as_deref(), Some("gate-red"));
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["passed"], false);
    assert_eq!(verdict["failing_step"], "boom", "{verdict}");
    assert_eq!(verdict["exit_code"], 7);
    assert!(
        verdict["log_tail"]
            .as_str()
            .unwrap()
            .contains("failing-out"),
        "{verdict}"
    );
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1["passed"], false);
    assert_eq!(rows[0].1["failing_step"], "boom");
    // Promotion fires on ANY verdict (§3) — red included.
    assert_eq!(wave_lifecycle(&boot).await, WaveLifecycle::Reviewing);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn gate_timeout_group_kills_and_fails_gate_timeout() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("timeout");
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "timeout_secs": 1,
        "steps": [ { "name": "hang", "cmd": "sleep 600" } ]
    })
    .to_string();
    seed_task(&boot, gate_task(&boot, "hang", &gate)).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    let started = std::time::Instant::now();
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "hang", 30).await;

    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("gate-timeout"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(25),
        "live timeout enforcement, not the parked deadline backstop"
    );
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["status_detail"], "gate-timeout");
    // No exit file — the group was SIGKILLed mid-step.
    let task_id = format!("{}:hang", boot.wave_id.as_str());
    assert!(!dir.join(format!("{task_id}-g1.exit")).exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn gate_spawn_kills_prior_recorded_group() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("killprior");

    // A live `setsid` group recorded on the tasks row — the stand-in
    // for a previous attempt's orphaned gate.
    let mut cmd = tokio::process::Command::new("sleep");
    cmd.arg("600").kill_on_drop(true);
    // SAFETY: setsid() is async-signal-safe, called pre-exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut orphan = cmd.spawn().expect("spawn orphan sleeper");
    let orphan_pid = orphan.id().expect("orphan pid") as i64;
    let start_time =
        calm_server::proc_identity::read_proc_start_time(orphan_pid as i32).expect("starttime");
    let boot_id = calm_server::proc_identity::read_boot_id().expect("boot id");

    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ { "name": "ok", "cmd": "true" } ]
    })
    .to_string();
    let mut task = gate_task(&boot, "killprior", &gate);
    task.gate_pid = Some(orphan_pid);
    task.gate_pid_starttime = Some(start_time as i64);
    task.gate_pid_boot_id = Some(boot_id);
    seed_task(&boot, task).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "killprior", 30).await;
    assert_eq!(row.status, TaskStatus::Done);

    // Kill-prior reaped the recorded group before spawning the fresh
    // attempt: the sleeper died to SIGKILL well before its 600s.
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), orphan.wait())
        .await
        .expect("orphan must be dead (kill-prior)")
        .expect("wait");
    assert!(!status.success(), "killed, not exited: {status:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 review F1 — the verdict channel is the wait status, never
/// the worker-reachable exit file: a step that forges `0` into the
/// exit path and then SIGKILLs the wrapper group must still land a
/// FAILED row (signal death → gate-infra), not a green one.
#[tokio::test]
async fn forged_exit_file_and_group_kill_cannot_flip_gate_green() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("forge");
    let task_id = format!("{}:forge", boot.wave_id.as_str());
    let exit_path = dir.join(format!("{task_id}-g1.exit"));
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ {
            "name": "forge",
            "cmd": format!("printf '0\\n' > '{}'; kill -9 0", exit_path.display()),
        } ]
    })
    .to_string();
    seed_task(&boot, gate_task(&boot, "forge", &gate)).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "forge", 30).await;

    assert_eq!(
        row.status,
        TaskStatus::Failed,
        "forged exit file must not pass the gate: {row:?}"
    );
    assert_eq!(row.status_detail.as_deref(), Some("gate-infra"));
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["passed"], false, "{verdict}");
    // The forged file IS on disk — proving the observer ignored it.
    assert_eq!(
        std::fs::read_to_string(&exit_path).unwrap().trim(),
        "0",
        "forged artifact present but not consulted"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 review F2 — a step body is a free-form snippet: a top-level
/// `exit 7` must end the STEP (red, exit_code 7) and still flow
/// through `neige_gate_finish`, leaving the exit file for
/// crashed-kernel recovery.
#[tokio::test]
async fn step_exit_ends_step_and_still_writes_exit_file() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("stepexit");
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ { "name": "bail", "cmd": "exit 7" } ]
    })
    .to_string();
    seed_task(&boot, gate_task(&boot, "stepexit", &gate)).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "stepexit", 30).await;

    assert_eq!(row.status, TaskStatus::Failed, "{row:?}");
    assert_eq!(row.status_detail.as_deref(), Some("gate-red"));
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["exit_code"], 7, "{verdict}");
    assert_eq!(verdict["failing_step"], "bail");
    // The finish handler ran despite the step's `exit`: the durable
    // recovery hint exists and carries the real code.
    let task_id = format!("{}:stepexit", boot.wave_id.as_str());
    let exit = std::fs::read_to_string(dir.join(format!("{task_id}-g1.exit"))).unwrap();
    assert_eq!(exit.trim(), "7");
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 review F1+F5 — step env hygiene: `NEIGE_GATE_EXIT_PATH` is
/// unset before any step runs, the kernel's environment does not leak
/// (env_clear), and the explicit minimal set (PATH, HOME) survives.
#[tokio::test]
async fn gate_step_env_is_minimal_and_exit_path_scrubbed() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    // Sentinel for the env_clear assertion: cargo always sets this for
    // the test process, so it stands in for "arbitrary kernel env".
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "test must run under cargo for the kernel-env sentinel"
    );
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("env");
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [
            { "name": "no-exit-path", "cmd": "test -z \"$NEIGE_GATE_EXIT_PATH\"" },
            { "name": "no-kernel-env", "cmd": "test -z \"$CARGO_MANIFEST_DIR\"" },
            { "name": "minimal-set", "cmd": "test -n \"$PATH\" && test -n \"$HOME\"" }
        ]
    })
    .to_string();
    seed_task(&boot, gate_task(&boot, "env", &gate)).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "env", 30).await;
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        row.status,
        TaskStatus::Done,
        "all env-hygiene steps must pass: {verdict}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 review F6 — gates are in-flight machinery, not new claims:
/// §5.2 scopes lifecycle gating to claims, so a gated task that
/// reported while the wave is Blocked must have its gate driven by the
/// very pass the report poked — not sit `verifying` until the slow
/// reconcile tick.
#[tokio::test]
async fn blocked_wave_still_drives_verifying_gate() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Blocked).await;
    let dir = unique_gate_dir("blocked");
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ { "name": "ok", "cmd": "true" } ]
    })
    .to_string();
    seed_task(&boot, gate_task(&boot, "blocked", &gate)).await;

    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "blocked", 30).await;
    assert_eq!(row.status, TaskStatus::Done, "{row:?}");
    // Lifecycle promotion is still guarded on Working → Reviewing: a
    // Blocked wave stays Blocked (the user gets unblocked explicitly).
    assert_eq!(wave_lifecycle(&boot).await, WaveLifecycle::Blocked);
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 review F4 — a `prepare_tx` client error BEFORE the guarded
/// bump terminal-fails op `#g1` while the row stays `verifying@0`.
/// Pre-fix this looped forever (every drive deduped onto the dead op
/// and the eq-attempt reconcile guard missed); the pre-bump arm must
/// flip the row `failed('gate-infra')` and emit the gate result.
/// Repro: a verifying row whose `gate_json` is gone — prepare_tx
/// raises Conflict before `task_gate_attempt_bump_tx` (same pre-bump
/// class as "wave row gone").
#[tokio::test]
async fn pre_bump_prepare_failure_fails_row_instead_of_looping() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("prebump");

    let mut task = plan_task(&boot.wave_id, "prebump", TaskKind::Codex, &[]);
    task.status = TaskStatus::Verifying;
    task.gate_json = None; // pre-bump Conflict: "task declares no gate"
    seed_task(&boot, task).await;

    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    scheduler.schedule_wave(boot.wave_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "prebump", 30).await;

    assert_eq!(row.status, TaskStatus::Failed, "{row:?}");
    assert_eq!(row.status_detail.as_deref(), Some("gate-infra"));
    assert_eq!(row.gate_attempt, 0, "the bump never happened");
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["passed"], false);
    assert_eq!(verdict["attempt"], 1, "the verdict records the op attempt");
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1, "exactly one gate result: {rows:?}");
    // The dead op is terminal-failed and no second attempt was minted.
    let task_id = format!("{}:prebump", boot.wave_id.as_str());
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .expect("op row");
    assert!(
        matches!(op.phase.tag(), PhaseTag::Failed),
        "{:?}",
        op.phase.tag()
    );
    assert_eq!(operation_count(&boot, "task-verify").await, 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn stale_gate_before_first_attempt_fails_without_running_shell() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("context-stale");
    let marker = dir.join("must-not-exist");
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [{ "name": "forbidden", "cmd": format!("touch '{}'", marker.display()) }]
    })
    .to_string();
    let task = gate_task(&boot, "stale-gate", &gate);
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    mark_context_stale(&boot, &task_id).await;
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );

    scheduler.sweep_all().await;
    let row = wait_for_terminal_row(&boot, "stale-gate", 30).await;

    assert!(!marker.exists(), "gate shell command must not execute");
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.gate_attempt, 0);
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert!(
        verdict["log_tail"]
            .as_str()
            .unwrap()
            .contains("context-stale")
    );
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.phase.tag(), PhaseTag::Failed);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn parked_gate_dead_at_boot_fails_op_and_row_reconciles_gate_infra() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("bootdead");

    // A `verifying` row whose attempt-1 op is parked with artifacts of
    // a provably-dead process and NO exit file — the "kernel died,
    // gate died, no verdict" crash shape.
    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ { "name": "ok", "cmd": "true" } ]
    })
    .to_string();
    let mut task = gate_task(&boot, "bootdead", &gate);
    task.gate_attempt = 1;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let operation_repo = Arc::new(SqlxOperationRepo::new(pool.clone()));
    let op_id = operation_repo
        .insert_operation(
            "task-verify",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("{task_id}#g1")),
                payload_hash: "hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut claimed = operation_repo.claim_drive_batch(1).await.unwrap();
    assert_eq!(claimed.len(), 1, "exactly the crafted op");
    let _op = claimed.pop().unwrap();
    let mut output = TxOutput::new("task", Some(task_id.clone()), json!({}));
    output.data = json!({
        "task_id": task_id,
        "wave_id": boot.wave_id.as_str(),
        "cove_id": "cove-x",
        "key": "bootdead",
        "attempt": 1,
        "cwd": dir.to_str().unwrap(),
        "gate": { "steps": [ { "name": "ok", "cmd": "true" } ] }
    });
    sqlx::query(
        r#"UPDATE operations
           SET phase = 'spawn_started',
               tx_output_json = ?1,
               target_json = '{"type":"task","id":null}'
           WHERE id = ?2"#,
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&op_id)
    .execute(&pool)
    .await
    .unwrap();
    let op = operation_repo.get_operation(&op_id).await.unwrap().unwrap();
    let artifacts = calm_server::operation::SpawnArtifacts {
        pid: 999_999,
        pgid: 999_999,
        start_time: 1,
        boot_id: calm_server::proc_identity::read_boot_id().unwrap_or_else(|| "boot".into()),
        log_path: Some(dir.join("bootdead-g1.log").display().to_string()),
        extra: json!({ "exit_path": dir.join("bootdead-g1.exit").display().to_string() }),
    };
    operation_repo
        .record_spawn_artifacts(&op, &artifacts)
        .await
        .unwrap();
    operation_repo
        .set_parked(&op, now_ms() + 600_000)
        .await
        .unwrap()
        .unwrap();

    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    // Boot recovery: VerifyParked → dead, no exit file → op fails
    // parked_dead (#653 §4.2; op-only write).
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .expect("op row");
    assert!(
        matches!(op.phase.tag(), PhaseTag::Failed),
        "dead parked gate with no verdict fails at boot: {:?}",
        op.phase.tag()
    );
    assert_eq!(
        task_row(&boot, "bootdead").await.status,
        TaskStatus::Verifying,
        "boot recovery writes the op only — the row copy is the scheduler's job"
    );

    // Consumer reconcile (§6.2 / §8 arm 2): the sweep's verifying arm
    // copies the op failure to the row as gate-infra.
    scheduler.sweep_all().await;
    let row = wait_for_terminal_row(&boot, "bootdead", 30).await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("gate-infra"));
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1, "reconcile copy emits the gate result");
    assert_eq!(rows[0].1["passed"], false);
    std::fs::remove_dir_all(&dir).ok();
}

/// Craft the durable shape `spawn_side_effect` leaves behind: a parked
/// `task-verify` op `#g1` with frozen tx_output and recorded spawn
/// artifacts. Mirrors the bootdead test's inline crafting (PR #685 F8).
async fn seed_parked_gate_op(
    boot: &Boot,
    task_id: &str,
    key: &str,
    dir: &std::path::Path,
    artifacts: &calm_server::operation::SpawnArtifacts,
) -> String {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let operation_repo = Arc::new(SqlxOperationRepo::new(pool.clone()));
    let op_id = operation_repo
        .insert_operation(
            "task-verify",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("{task_id}#g1")),
                payload_hash: "hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut claimed = operation_repo.claim_drive_batch(1).await.unwrap();
    assert_eq!(claimed.len(), 1, "exactly the crafted op");
    claimed.pop();
    let mut output = TxOutput::new("task", Some(task_id.to_string()), json!({}));
    output.data = json!({
        "task_id": task_id,
        "wave_id": boot.wave_id.as_str(),
        "cove_id": "cove-x",
        "key": key,
        "attempt": 1,
        "cwd": dir.to_str().unwrap(),
        "gate": { "steps": [ { "name": "ok", "cmd": "true" } ] }
    });
    sqlx::query(
        r#"UPDATE operations
           SET phase = 'spawn_started',
               tx_output_json = ?1,
               target_json = '{"type":"task","id":null}'
           WHERE id = ?2"#,
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&op_id)
    .execute(&pool)
    .await
    .unwrap();
    let op = operation_repo.get_operation(&op_id).await.unwrap().unwrap();
    operation_repo
        .record_spawn_artifacts(&op, artifacts)
        .await
        .unwrap();
    operation_repo
        .set_parked(&op, now_ms() + 600_000)
        .await
        .unwrap()
        .unwrap();
    op_id
}

/// PR #685 review F8(a) — boot reattach to a LIVE gate: a parked op
/// whose recorded process survived the kernel restart is left parked
/// by recovery, and the spawned reattach observer lands the verdict
/// (one-tx flip + event) once the process exits.
#[tokio::test]
async fn boot_reattach_live_gate_lands_verdict_after_exit() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("reattach");

    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ { "name": "ok", "cmd": "true" } ]
    })
    .to_string();
    let mut task = gate_task(&boot, "reattach", &gate);
    task.gate_attempt = 1;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // The surviving wrapper stand-in: alive across "the restart",
    // writes its exit file green (tmp + rename, like the real wrapper)
    // and exits ~1s from now.
    let exit_path = dir.join(format!("{task_id}-g1.exit"));
    let log_path = dir.join(format!("{task_id}-g1.log"));
    std::fs::write(&log_path, "::gate-step ok\nfine\n").unwrap();
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(format!(
        "sleep 1; printf '0\\n' > '{exit}.tmp'; mv -f -- '{exit}.tmp' '{exit}'",
        exit = exit_path.display()
    ));
    // SAFETY: setsid() is async-signal-safe, called pre-exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut survivor = cmd.spawn().expect("spawn surviving gate stand-in");
    let pid = survivor.id().expect("pid") as i32;
    let artifacts = calm_server::operation::SpawnArtifacts {
        pid,
        pgid: pid,
        start_time: calm_server::proc_identity::read_proc_start_time(pid).expect("starttime"),
        boot_id: calm_server::proc_identity::read_boot_id().expect("boot id"),
        log_path: Some(log_path.display().to_string()),
        extra: json!({ "exit_path": exit_path.display().to_string() }),
    };
    seed_parked_gate_op(&boot, &task_id, "reattach", &dir, &artifacts).await;

    let (runtime, _scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    // Alive → LeaveParked: the op survives recovery unresolved.
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .expect("op row");
    assert!(
        matches!(op.phase.tag(), PhaseTag::Parked),
        "live gate stays parked at boot: {:?}",
        op.phase.tag()
    );
    // Reap the stand-in in the test so the observer's liveness poll
    // sees a dead pid, not a zombie.
    survivor.wait().await.expect("stand-in exits");
    // The reattach observer polls the identity until death, reads the
    // exit file, and lands the one-tx completion: row done + event.
    let row = wait_for_terminal_row(&boot, "reattach", 30).await;
    assert_eq!(row.status, TaskStatus::Done, "{row:?}");
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["passed"], true, "{verdict}");
    assert_eq!(verdict["exit_code"], 0);
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .expect("op row");
    assert!(
        matches!(op.phase.tag(), PhaseTag::Succeeded),
        "{:?}",
        op.phase.tag()
    );
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1["passed"], true);
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 review F8(b) — exit-file verdict recovery: a parked op
/// whose process died while the kernel was down, leaving a valid exit
/// file, recovers the REAL red verdict (gate-red, exit_code, failing
/// step) — never gate-infra.
#[tokio::test]
async fn parked_gate_dead_with_exit_file_recovers_real_verdict() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("deadexit");

    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ { "name": "boom", "cmd": "false" } ]
    })
    .to_string();
    let mut task = gate_task(&boot, "deadexit", &gate);
    task.gate_attempt = 1;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // Dead-process artifacts + the durable verdict the wrapper wrote
    // before the whole stack went down.
    let exit_path = dir.join(format!("{task_id}-g1.exit"));
    let log_path = dir.join(format!("{task_id}-g1.log"));
    std::fs::write(&log_path, "::gate-step boom\nboom-out\n").unwrap();
    std::fs::write(&exit_path, "7\n").unwrap();
    let artifacts = calm_server::operation::SpawnArtifacts {
        pid: 999_999,
        pgid: 999_999,
        start_time: 1,
        boot_id: calm_server::proc_identity::read_boot_id().unwrap_or_else(|| "boot".into()),
        log_path: Some(log_path.display().to_string()),
        extra: json!({ "exit_path": exit_path.display().to_string() }),
    };
    seed_parked_gate_op(&boot, &task_id, "deadexit", &dir, &artifacts).await;

    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    // Dead + parseable exit file → the op completes with the recorded
    // verdict (op-only write, #653 §4.2).
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .expect("op row");
    assert!(
        matches!(op.phase.tag(), PhaseTag::Succeeded),
        "dead gate WITH a verdict recovers it: {:?}",
        op.phase.tag()
    );
    assert_eq!(
        task_row(&boot, "deadexit").await.status,
        TaskStatus::Verifying,
        "boot recovery writes the op only"
    );

    // Sweep copies the REAL verdict to the row — red, not infra.
    scheduler.sweep_all().await;
    let row = wait_for_terminal_row(&boot, "deadexit", 30).await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("gate-red"));
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["exit_code"], 7, "{verdict}");
    assert_eq!(verdict["failing_step"], "boom");
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1["passed"], false);
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 fix round 2, F2 — a parked gate whose process died BEFORE
/// the deadline with no exit file must fail `gate-infra` promptly via
/// the steady-state pre-deadline probe, not sit `verifying` until
/// `parked_deadline_ms` and get misreported `gate-timeout`.
#[tokio::test]
async fn parked_gate_dead_pre_deadline_fails_gate_infra_promptly() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("predead");

    let gate = json!({
        "cwd": dir.to_str().unwrap(),
        "steps": [ { "name": "ok", "cmd": "true" } ]
    })
    .to_string();
    let mut task = gate_task(&boot, "predead", &gate);
    task.gate_attempt = 1;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // Dead-process artifacts (the "group killed, no verdict written"
    // shape), parked deadline far in the FUTURE — only the probe can
    // resolve this before then.
    let artifacts = calm_server::operation::SpawnArtifacts {
        pid: 999_999,
        pgid: 999_999,
        start_time: 1,
        boot_id: calm_server::proc_identity::read_boot_id().unwrap_or_else(|| "boot".into()),
        log_path: Some(dir.join("predead-g1.log").display().to_string()),
        extra: json!({ "exit_path": dir.join("predead-g1.exit").display().to_string() }),
    };
    seed_parked_gate_op(&boot, &task_id, "predead", &dir, &artifacts).await;

    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    // Steady-state sweep (NOT boot recovery): the pre-deadline probe
    // sees dead + no exit file → Fail → op failed `parked_dead`, then
    // the same sweep's verifying arm copies it to the row.
    let started = std::time::Instant::now();
    scheduler.sweep_all().await;
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .expect("op row");
    assert!(
        matches!(op.phase.tag(), PhaseTag::Failed),
        "pre-deadline probe fails the dead op: {:?}",
        op.phase.tag()
    );
    let row = wait_for_terminal_row(&boot, "predead", 30).await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(
        row.status_detail.as_deref(),
        Some("gate-infra"),
        "dead-no-verdict is infra, never gate-timeout: {row:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(25),
        "probe path, not the parked-deadline backstop"
    );
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1["passed"], false);
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #685 fix round 2, F1 — dropping/aborting the exit observer (the
/// in-process stand-in for a graceful kernel shutdown dropping every
/// spawned task) must NOT kill the running gate: no `kill_on_drop` on
/// the wrapper child. The group stays alive, and a boot-style recovery
/// reattaches and lands the real verdict once the gate finishes.
#[tokio::test]
async fn aborted_observer_leaves_gate_group_alive_and_reattach_lands_verdict() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let dir = unique_gate_dir("obsdrop");

    let mut task = gate_task(
        &boot,
        "obsdrop",
        &json!({
            "cwd": dir.to_str().unwrap(),
            "steps": [ { "name": "ok", "cmd": "sleep 2; echo fine" } ]
        })
        .to_string(),
    );
    task.gate_attempt = 1;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    // Drive the REAL adapter's spawn by hand so the test owns the
    // observer future the runtime would otherwise detach: craft the
    // op in `spawn_started` with the frozen tx_output, call
    // `spawn_side_effect`, park, spawn the observer — then ABORT it
    // mid-`wait()` (the drop a graceful shutdown performs).
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let operation_repo = Arc::new(SqlxOperationRepo::new(pool.clone()));
    let op_id = operation_repo
        .insert_operation(
            "task-verify",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("{task_id}#g1")),
                payload_hash: "hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut claimed = operation_repo.claim_drive_batch(1).await.unwrap();
    assert_eq!(claimed.len(), 1, "exactly the crafted op");
    claimed.pop();
    let mut output = TxOutput::new("task", Some(task_id.clone()), json!({}));
    output.data = json!({
        "task_id": task_id,
        "wave_id": boot.wave_id.as_str(),
        "cove_id": "cove-x",
        "key": "obsdrop",
        "attempt": 1,
        "cwd": dir.to_str().unwrap(),
        "gate": { "steps": [ { "name": "ok", "cmd": "sleep 2; echo fine" } ] }
    });
    sqlx::query(
        r#"UPDATE operations
           SET phase = 'spawn_started',
               tx_output_json = ?1,
               target_json = '{"type":"task","id":null}'
           WHERE id = ?2"#,
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&op_id)
    .execute(&pool)
    .await
    .unwrap();
    let op = operation_repo.get_operation(&op_id).await.unwrap().unwrap();

    let adapter = calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone());
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        Arc::new(DaemonClient {
            data_dir: std::path::PathBuf::from("/tmp/neige-scheduler-test-noop"),
            proc_supervisor_sock: Some(std::path::PathBuf::from(
                "/tmp/neige-scheduler-test-missing.sock",
            )),
        }),
        TerminalRendererRegistry::new(),
        boot.events.clone(),
        OperationCompletionBus::new(),
    );
    let outcome = adapter
        .spawn_side_effect(&output, &op, &spawn_ctx)
        .await
        .expect("gate spawn");
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = outcome
    else {
        panic!("task-verify spawn must park");
    };
    operation_repo
        .set_parked(&op, deadline_ms)
        .await
        .unwrap()
        .expect("park commits");
    let observer_task = tokio::spawn(observer);
    observer_task.abort();
    let _ = observer_task.await; // joined: the Child handle is dropped NOW

    // The regression assertion: the wrapper survived the observer drop
    // (with `kill_on_drop` it would already be SIGKILLed here).
    let op = operation_repo.get_operation(&op_id).await.unwrap().unwrap();
    let artifacts = op.spawn_artifacts.clone().expect("recorded artifacts");
    assert!(
        calm_server::proc_identity::verify_owned_pid(
            artifacts.pid,
            artifacts.start_time,
            &artifacts.boot_id
        ),
        "gate wrapper must survive an observer drop"
    );

    // The dropped Child is unreaped; reap the wrapper deterministically
    // once it exits so the reattach liveness poll sees a dead pid, not
    // a zombie (in production the restarted kernel is a NEW process and
    // init reaps the orphan).
    let wrapper_pid = artifacts.pid;
    tokio::task::spawn_blocking(move || {
        let mut status: libc::c_int = 0;
        // ECHILD (tokio's orphan reaper won) is fine — either way the
        // pid leaves /proc.
        unsafe { libc::waitpid(wrapper_pid, &mut status, 0) };
    });

    // Boot-style recovery: parked + alive → reattach observer; the
    // wrapper finishes (~2s), writes its exit file, and the observer
    // lands the green verdict via the one-tx flip.
    let (runtime, _scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(
            calm_server::operation::task_verify_adapter::TaskVerifyAdapter::new(dir.clone()),
        )],
    );
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    let op = runtime
        .find_by_kind_and_idempotency("task-verify", &format!("{task_id}#g1"))
        .await
        .unwrap()
        .expect("op row");
    assert!(
        matches!(op.phase.tag(), PhaseTag::Parked),
        "live gate stays parked through boot recovery: {:?}",
        op.phase.tag()
    );
    let row = wait_for_terminal_row(&boot, "obsdrop", 30).await;
    assert_eq!(row.status, TaskStatus::Done, "{row:?}");
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["passed"], true, "{verdict}");
    assert_eq!(verdict["exit_code"], 0);
    let rows = event_rows(&boot, "task.gate_result").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1["passed"], true);
    std::fs::remove_dir_all(&dir).ok();
}

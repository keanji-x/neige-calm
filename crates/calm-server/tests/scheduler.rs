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
//!     `duplicate_report_is_idempotent`, `gated_row_is_left_alone_on_complete`.
//!   §3 verdict isolation — `spec_verdict_never_flips_rows`.
//!   M2 live path — `terminal_hook_completes_task_on_exit`.
//!   §8 sweep arms — `sweep_reconciles_running_terminal_with_recorded_exit`,
//!     `sweep_resubmits_dispatched_task_with_missing_operation`.

use std::sync::Arc;

use async_trait::async_trait;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, task_insert_tx};
use calm_server::error::Result as CalmResult;
use calm_server::event::EventBus;
use calm_server::ids::{CardId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::emit::{TOOL_TASK_COMPLETE, TOOL_TASK_FAIL};
use calm_server::mcp_server::tools::wave_state::TOOL_TASK_VERDICT;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{
    CardRole, NewCard, NewCove, NewTerminal, NewWave, Task, TaskKind, TaskStatus, WaveLifecycle,
    WavePatch, now_ms,
};
use calm_server::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, OperationCompletionBus,
    OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome,
    SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::scheduler::{Scheduler, TerminalTaskHook};
use calm_server::state::{DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::{Value, json};

struct Boot {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    /// Same cache instance `write` wraps — kept so tests can register
    /// extra worker cards minted mid-test.
    card_role_cache: CardRoleCache,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
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
            cove_id: cove.id.clone(),
            title: "scheduler-test".into(),
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
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache);

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs_pool: repo.sqlite_pool(),
        events: events.clone(),
        write: write.clone(),
        daemon_token_hash: None,
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        repo,
        events,
        write,
        card_role_cache,
        ctx,
        registry: Arc::new(registry),
        wave_id: wave.id,
        spec_card_id: spec_card.id,
        worker_card_id: worker_card.id,
    }
}

/// Build a real `OperationRuntime` over the boot repo with the supplied
/// stub adapters, plus a `Scheduler` wired exactly like the dispatcher
/// construction site wires it (Weak runtime + shared semaphore).
fn build_scheduler(
    boot: &Boot,
    adapters: Vec<Arc<dyn ProviderAdapter>>,
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
    );
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        adapters,
        boot.events.clone(),
        completion,
        spawn_ctx,
    ));
    let scheduler = Scheduler::new(
        boot.repo.clone(),
        boot.events.clone(),
        boot.write.clone(),
        Arc::downgrade(&runtime),
        Arc::new(tokio::sync::Semaphore::new(8)),
    );
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
            TaskKind::Codex => format!("do {key}"),
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
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    }
}

async fn seed_task(boot: &Boot, task: Task) {
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            task_insert_tx(tx, &task).await?;
            Ok(())
        })
    })
    .await
    .expect("seed task row");
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

fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        wave_id: Some(boot.wave_id.as_str().to_string()),
        thread_id: "worker-thread".into(),
    }
}

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        wave_id: Some(boot.wave_id.as_str().to_string()),
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

/// Fast-worker-report fixture: the spawn side effect itself reports
/// `calm.task.complete` BEFORE the scheduler's `wait()` can return —
/// the §3 race, deterministically sequenced.
struct FastReportAdapter {
    kind: &'static str,
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
        Ok(TxOutput::new("fast-report", None, json!({ "ok": true })))
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
    seed_task(&boot, plan_task(&boot.wave_id, "t1", TaskKind::Codex, &[])).await;
    seed_task(
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

// ---------------------------------------------------------------------------
// §5.2 — budget + lifecycle gating
// ---------------------------------------------------------------------------

#[tokio::test]
async fn budget_holds_second_task_until_first_done() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_task(&boot, plan_task(&boot.wave_id, "a", TaskKind::Codex, &[])).await;
    seed_task(&boot, plan_task(&boot.wave_id, "b", TaskKind::Codex, &[])).await;
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

#[tokio::test]
async fn claim_race_two_schedulers_single_winner() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    seed_task(
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

    let w1 = boot.wave_id.clone();
    let w2 = boot.wave_id.clone();
    tokio::join!(s1.schedule_wave(w1), s2.schedule_wave(w2));

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
    seed_task(&boot, task).await;
    let handler = boot
        .registry
        .lookup(TOOL_TASK_COMPLETE)
        .expect("task complete tool");
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(FastReportAdapter {
            kind: "terminal-worker",
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
    seed_task(&boot, task).await;
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
async fn duplicate_report_is_idempotent() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "dup", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

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
}

#[tokio::test]
async fn gated_row_is_left_alone_on_complete() {
    // Defensive PR-C guard: no gated row can exist yet (PR-A rule 8),
    // but if one did, `calm.task.complete` must NOT flip it to done —
    // gated completion belongs to the verify pipeline.
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "gated", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.gate_json = Some(json!({ "steps": [{ "name": "t", "cmd": "true" }] }).to_string());
    let task_id = task.id.clone();
    seed_task(&boot, task).await;

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": {} }),
    )
    .await
    .expect("task complete");
    assert_eq!(
        task_row(&boot, "gated").await.status,
        TaskStatus::Running,
        "gated rows must not be mis-flipped to done"
    );

    // Worker failure flips regardless of gate (§3: no gate runs on
    // failure).
    call_tool(
        &boot,
        TOOL_TASK_FAIL,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "reason": "boom" }),
    )
    .await
    .expect("task fail");
    assert_eq!(task_row(&boot, "gated").await.status, TaskStatus::Failed);
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
async fn terminal_hook_nonzero_exit_fails_task() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Working).await;
    let mut task = plan_task(&boot.wave_id, "term-fail", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (_card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;

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

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

use std::sync::Arc;

use async_trait::async_trait;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, task_insert_tx};
use calm_server::error::Result as CalmResult;
use calm_server::event::EventBus;
use calm_server::ids::{ActorId, CardId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::emit::{TOOL_TASK_COMPLETE, TOOL_TASK_FAIL};
use calm_server::mcp_server::tools::plan::TOOL_PLAN_UPSERT;
use calm_server::mcp_server::tools::wave_state::TOOL_TASK_VERDICT;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{
    CardRole, NewCard, NewCove, NewTerminal, NewWave, Task, TaskKind, TaskStatus, WaveLifecycle,
    WavePatch, new_id, now_ms,
};
use calm_server::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, OperationCompletionBus,
    OperationKey, OperationOutcome, OperationRepo, OperationRuntime, PhaseTag, ProviderAdapter,
    SpawnCtx, SpawnHandle, SpawnOutcome, SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::routes::terminal_cards::stable_payload_hash;
use calm_server::scheduler::{Scheduler, TerminalTaskHook, build_worker_payload};
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
        semaphore,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
    seed_task(&boot, task).await;
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
                calm_server::db::sqlite::task_mark_running_tx(tx, &task_id, Some("late"), now_ms())
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
    seed_task(&boot, task).await;

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
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("sibling worker card");
    boot.card_role_cache
        .insert(sibling.id.clone(), CardRole::Worker, boot.wave_id.clone());
    let sibling_identity = ToolCallIdentity {
        card_id: sibling.id.as_str().to_string(),
        role: CardRole::Worker,
        wave_id: Some(boot.wave_id.as_str().to_string()),
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

    // `calm.plan.upsert` WITHOUT a lifecycle arg: draft auto-promotes
    // to Planning and stays there — the F5 scenario.
    call_tool(
        &boot,
        TOOL_PLAN_UPSERT,
        spec_identity(&boot),
        json!({
            "tasks": [{ "key": "p1", "kind": "codex", "goal": "do p1" }],
            "message": "plan ready"
        }),
    )
    .await
    .expect("plan upsert");
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Planning,
        "upsert with no lifecycle arg leaves the wave Planning"
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
    seed_task(&boot, plan_task(&boot.wave_id, "bg", TaskKind::Codex, &[])).await;
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
            kind: "codex".into(),
            sort: None,
            payload: json!({ "idempotency_key": "some-other-task" }),
        })
        .await
        .expect("sibling worker card");
    boot.card_role_cache
        .insert(sibling.id.clone(), CardRole::Worker, boot.wave_id.clone());
    let sibling_identity = ToolCallIdentity {
        card_id: sibling.id.as_str().to_string(),
        role: CardRole::Worker,
        wave_id: Some(boot.wave_id.as_str().to_string()),
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
            kind: "codex".into(),
            sort: None,
            payload: json!({ "idempotency_key": task_id }),
        })
        .await
        .expect("forged sibling card");
    boot.card_role_cache
        .insert(sibling.id.clone(), CardRole::Worker, boot.wave_id.clone());
    let sibling_identity = ToolCallIdentity {
        card_id: sibling.id.as_str().to_string(),
        role: CardRole::Worker,
        wave_id: Some(boot.wave_id.as_str().to_string()),
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
    seed_task(&boot, legacy).await;
    seed_task(
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

    // The boot funnel's sweep reconciles and opens the gate.
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

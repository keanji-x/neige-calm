//! Dispatcher integration tests.
//!
//! Coverage:
//!
//!   1. **`SubscribeFilter` over `EventBus::subscribe_filtered`** — emit
//!      three events of mixed kinds + scopes, assert the filter delivers
//!      only the requested ones (and that the receiver outlives extra
//!      lifecycle activity around it).
//!   2. Pending shared-spec thread binding.
//!   3. Scheduler trigger wiring: `track.updated` pokes the plan scheduler.
//!
//! Tests run against an in-memory `SqlxRepo` and stubbed worker spawn
//! dependencies.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::{Event, EventBus, EventScope, SubscribeFilter, SubscribeScope};
use calm_server::ids::{ActorId, AreaId, TrackId};
use calm_server::model::{
    CardRole, NewArea, NewCard, NewTerminal, NewTrack, Task, TaskKind, TaskStatus, TrackLifecycle,
    TrackPatch, new_id, now_ms,
};
use calm_server::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, OperationCompletionBus,
    OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome,
    SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::pending_codex_threads::{PendingEntry, PendingThreadStartRegistry};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_area_cache::TrackAreaCache;
use calm_types::track_report::{ReportBlock, TrackReportPayload};
use serde_json::Value;

static DISPATCHER_DAEMON_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot() -> (
    Arc<dyn Repo>,
    EventBus,
    CardRoleCache,
    TrackAreaCache,
    TrackId,
    AreaId,
) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "dispatcher-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "dispatcher-test".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    (
        repo,
        events,
        card_role_cache,
        track_area_cache,
        track.id,
        area.id,
    )
}

fn stub_daemon() -> Arc<DaemonClient> {
    Arc::new(DaemonClient {
        data_dir: PathBuf::from("/tmp/neige-dispatcher-test-noop"),
        proc_supervisor_sock: Some(PathBuf::from(
            "/tmp/neige-dispatcher-test-missing-proc-supervisor.sock",
        )),
    })
}

fn stub_codex() -> Arc<CodexClient> {
    Arc::new(CodexClient::new_stub())
}

fn stub_shared(
    repo: &Arc<dyn Repo>,
) -> Arc<calm_server::shared_codex_appserver::SharedCodexAppServer> {
    calm_server::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone())
}

fn codex_req(idem: &str, goal: &str) -> Event {
    Event::CodexWorkerRequested {
        idempotency_key: idem.into(),
        goal: goal.into(),
        context: serde_json::Value::Null,
        acceptance_criteria: None,
        agent_message: None,
    }
}

fn track_scope(track: &TrackId, area: &AreaId) -> EventScope {
    EventScope::Track {
        track: track.clone(),
        area: area.clone(),
    }
}

#[tokio::test]
async fn dispatcher_pending_thread_bind_persists_thread_id_and_broadcasts_card_updated() {
    let (repo, events, _cache, _wcc, track_id, _area_id) = boot().await;
    let planner_card = repo
        .card_create(NewCard {
            track_id: track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .expect("create planner card");
    let runtime = calm_server::db::write_in_tx_typed(repo.as_ref(), {
        let card_id = planner_card.id.to_string();
        move |tx| {
            Box::pin(async move {
                let runtime = session_start_runtime_tx(
                    tx,
                    WorkerSessionInit {
                        id: new_id(),
                        card_id,
                        kind: WorkerSessionKind::SharedPlanner,
                        agent_provider: Some(AgentProvider::Codex),
                        status: WorkerSessionState::TurnPending,
                        terminal_run_id: None,
                        thread_id: None,
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        spawn_op_id: None,
                        now_ms: now_ms(),
                    },
                )
                .await?;
                Ok(runtime)
            })
        }
    })
    .await
    .expect("seed shared-spec runtime");
    let terminal = repo
        .terminal_create(NewTerminal {
            card_id: planner_card.id.clone(),
            program: "codex".into(),
            cwd: "/workspace".into(),
            env: serde_json::json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("seed live terminal");
    let pending = PendingThreadStartRegistry::new(repo.clone(), events.clone());
    pending
        .register(
            PendingEntry::new(
                planner_card.id.to_string(),
                Some(track_id.to_string()),
                terminal.id.to_string(),
                runtime.id.clone(),
            )
            .with_role(calm_server::model::CardRole::Planner),
        )
        .await
        .expect("register pending thread");
    let mut rx = events.subscribe();

    assert_eq!(
        pending
            .on_thread_started("thread-tui-created")
            .await
            .expect("bind pending thread"),
        Some(planner_card.id.to_string())
    );

    let updated = repo
        .card_get(planner_card.id.as_str())
        .await
        .expect("card_get")
        .expect("planner card still exists");
    assert!(updated.payload.get("codex_thread_id").is_none());
    let runtime = repo
        .session_projection_active_for_card(&planner_card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(runtime.thread_id.as_deref(), Some("thread-tui-created"));

    let envelope = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let envelope = rx.recv().await.expect("event bus open");
            if matches!(envelope.event, Event::CardUpdated(_)) {
                break envelope;
            }
        }
    })
    .await
    .expect("CardUpdated broadcast");
    match envelope.event {
        Event::CardUpdated(card) => {
            assert_eq!(card.id, planner_card.id);
            assert!(card.payload.get("codex_thread_id").is_none());
        }
        other => panic!("expected CardUpdated, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatcher_pending_thread_missing_runtime_is_orphaned_and_clears_pending() {
    let (repo, events, _cache, _wcc, track_id, _area_id) = boot().await;
    let planner_card = repo
        .card_create(NewCard {
            track_id: track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!(["corrupt-payload-shape"]),
        })
        .await
        .expect("create malformed planner card");
    let terminal = repo
        .terminal_create(NewTerminal {
            card_id: planner_card.id.clone(),
            program: "codex".into(),
            cwd: "/workspace".into(),
            env: serde_json::json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("seed live terminal");
    let pending = PendingThreadStartRegistry::new(repo.clone(), events.clone());
    pending
        .register(
            PendingEntry::new(
                planner_card.id.to_string(),
                Some(track_id.to_string()),
                terminal.id.to_string(),
                "missing-runtime".to_string(),
            )
            .with_role(calm_server::model::CardRole::Planner),
        )
        .await
        .expect("register pending thread");
    let mut rx = events.subscribe();

    assert_eq!(
        pending
            .on_thread_started("thread-that-cannot-persist")
            .await
            .expect("missing runtime is consumed as an orphan"),
        None
    );
    assert_eq!(pending.pending_count().await, 0);

    let unchanged = repo
        .card_get(planner_card.id.as_str())
        .await
        .expect("card_get")
        .expect("planner card still exists");
    assert_eq!(
        unchanged.payload,
        serde_json::json!(["corrupt-payload-shape"])
    );
    let envelope = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("stale pending clear broadcasts CardUpdated")
        .expect("event bus open");
    assert!(matches!(envelope.event, Event::CardUpdated(_)));
}

/// Poll a predicate every 20ms until it returns Some(...) or the
/// timeout elapses. Returns the latest predicate result.
async fn wait_for<T, F, Fut>(timeout: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return Some(v);
        }
        if start.elapsed() > timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// 1. SubscribeFilter integration.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_filtered_delivers_only_matching_kinds() {
    let events = EventBus::new();
    let mut rx = events.subscribe_filtered();
    let filter = SubscribeFilter {
        scope: SubscribeScope::Any,
        include_descendants: true,
        kinds: Some(vec!["codex.worker_requested".into()]),
    };

    // Emit three events: matching kind, non-matching kind, matching kind again.
    events.emit(ActorId::User, codex_req("k1", "g1"));
    events.emit(
        ActorId::User,
        Event::TaskFailed {
            idempotency_key: "k2".into(),
            reason: "x".into(),
            details: None,
            agent_message: None,
        },
    );
    events.emit(ActorId::User, codex_req("k3", "g3"));

    let mut matched = Vec::new();
    // Drain everything the channel has + a small grace window.
    for _ in 0..6 {
        if let Ok(Ok(env)) = tokio::time::timeout(Duration::from_millis(80), rx.recv()).await {
            if filter.matches(&env) {
                matched.push(env);
            }
        } else {
            break;
        }
    }
    assert_eq!(
        matched.len(),
        2,
        "exactly two codex.worker_requested events should match, got {}",
        matched.len()
    );
    for env in &matched {
        assert_eq!(env.event.kind_tag(), "codex.worker_requested");
    }
}

/// `subscribe_filtered` returns the raw bus receiver — verifying that
/// the receiver lives long enough across `recv()` calls and behaves
/// like the bare `subscribe()` API. The in-module `event::filter_tests`
/// pin the scope-match predicate exhaustively.
#[tokio::test]
async fn subscribe_filtered_returns_live_receiver() {
    let events = EventBus::new();
    let mut rx = events.subscribe_filtered();
    events.emit(ActorId::User, codex_req("k", "g"));
    let env = tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .expect("recv within 200ms")
        .expect("recv ok");
    assert_eq!(env.event.kind_tag(), "codex.worker_requested");
}

#[tokio::test]
async fn subscribe_filtered_skips_lagged_without_panic() {
    // Provoke a Lagged frame by oversubscribing the bus capacity
    // (BUS_CAPACITY = 1024). We don't have a public knob to shrink
    // capacity, so 1100 emits exceeds the channel's queue against a
    // receiver that hasn't drained.
    let events = EventBus::new();
    let mut rx = events.subscribe_filtered();
    for i in 0..1100u32 {
        events.emit(ActorId::User, codex_req(&format!("k{i}"), "g"));
    }

    // Drain until either RecvError::Lagged surfaces or we exhaust.
    let mut saw_lag = false;
    let mut saw_ok = 0usize;
    for _ in 0..1200 {
        match tokio::time::timeout(Duration::from_millis(20), rx.recv()).await {
            Ok(Ok(_)) => saw_ok += 1,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                saw_lag = true;
                // Continue draining — the channel is still alive after
                // a lag and should yield events that came after the
                // dropped frames.
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => break, // Timeout — nothing more pending.
        }
    }
    assert!(
        saw_lag,
        "expected at least one Lagged frame after 1100 emits"
    );
    assert!(saw_ok > 0, "expected to see ok recvs after Lagged");
}

/// A dispatcher lag is a lost-edge recovery path. The context verdict must
/// land before the scheduler can resume the same dispatched row; otherwise
/// the worker crosses the admission point and §5.3.3 deliberately leaves it
/// alone afterward.
#[tokio::test(flavor = "current_thread")]
async fn lagged_context_sweep_precedes_scheduler_resume() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, track_id, _area_id) = boot().await;
    repo.track_update(
        track_id.as_str(),
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Working),
            ..Default::default()
        },
    )
    .await
    .expect("set working lifecycle");

    let pool = repo.sqlite_pool().expect("dispatcher test uses sqlite");
    let report = TrackReportPayload {
        schema_version: TrackReportPayload::SCHEMA_VERSION,
        doc_rev: 1,
        summary: String::new(),
        body: "# Task\n".into(),
        blocks: Some(vec![ReportBlock {
            id: "b_1a66ed".into(),
            kind: "task".into(),
            rev: 1,
            payload: serde_json::json!({
                "key": "lagged-stale", "kind": "terminal", "command": "false"
            }),
        }]),
    };
    sqlx::query(
        "INSERT INTO cards \
         (id,track_id,kind,sort,payload,role,deletable,created_at,updated_at) \
         VALUES ('lagged-context-report',?1,'track-report',-1,?2,'reportcard',0,1,1)",
    )
    .bind(track_id.as_str())
    .bind(serde_json::to_string(&report).unwrap())
    .execute(&pool)
    .await
    .expect("seed referenced report block");

    let task_id = format!("{}:lagged-stale", track_id.as_str());
    let now = now_ms();
    let task = Task {
        id: task_id.clone(),
        track_id: track_id.as_str().to_string(),
        key: "lagged-stale".into(),
        kind: TaskKind::Terminal,
        goal: "must not spawn".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
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
    };
    calm_server::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::support::task::insert_task_tx(tx, &task).await?;
            Ok(())
        })
    })
    .await
    .expect("seed pending task");

    let spawned = Arc::new(AtomicUsize::new(0));
    let operation_repo = Arc::new(SqlxOperationRepo::new(pool.clone()));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let completion = OperationCompletionBus::new();
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        stub_daemon(),
        TerminalRendererRegistry::new(),
        events.clone(),
        completion.clone(),
    );
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![Arc::new(CountingSpawnAdapter {
            spawned: spawned.clone(),
        })],
        events.clone(),
        completion,
        spawn_ctx,
    ));
    let dispatcher = Dispatcher::spawn_with_operation_runtime(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache, wcc),
        stub_codex(),
        stub_daemon(),
        None,
        stub_shared(&repo),
        runtime,
        4,
    );
    dispatcher.scheduler().mark_boot_sweep_complete();
    dispatcher.scheduler().open_context_sweep_gate().await;
    dispatcher
        .scheduler()
        .schedule_track(track_id.clone())
        .await;
    let frozen_context: String =
        sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .expect("production claim context");
    assert_ne!(frozen_context, "[]");
    spawned.store(0, Ordering::SeqCst);
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.blocks[0].payload.goal', 'changed') \
         WHERE id = 'lagged-context-report'",
    )
    .execute(&pool)
    .await
    .expect("change referenced block without emitting its event");

    // No await after subscription creation: on a current-thread runtime the
    // dispatcher cannot drain until all 1,100 sends have forced one Lagged.
    for index in 0..1100 {
        events.emit(
            ActorId::Kernel,
            Event::TaskContextAdvanced {
                track_id: Default::default(),
                task_key: String::new(),
                task_id: format!("noise-{index}"),
                changed_refs: Vec::new(),
                verdict: "material".into(),
                rationale: String::new(),
            },
        );
    }

    let stale = wait_for(Duration::from_secs(5), || {
        let repo = repo.clone();
        let task_id = task_id.clone();
        async move {
            repo.task_get(&task_id)
                .await
                .ok()
                .flatten()
                .and_then(|task| task.context_stale_at_ms)
        }
    })
    .await;
    assert!(
        stale.is_some(),
        "Lagged recovery must persist material verdict"
    );
    tokio::task::yield_now().await;
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        0,
        "scheduler must not start a worker before the Lagged context sweep"
    );
}

// ---------------------------------------------------------------------------
// Issue #644 round-2 review F4 — `track.updated` is a scheduler trigger.
// ---------------------------------------------------------------------------

const CARD_SPAWN_ADAPTER_PHASES: &[PhaseTag] = &[];

/// Successful worker-spawn stub (mirror of the scheduler suite's):
/// `prepare_tx` returns a card-shaped result — the scheduler reads
/// `result["id"]` for the running stamp — and the spawn is a no-op.
struct CardSpawnAdapter {
    kind: &'static str,
    card_id: String,
}

struct CountingSpawnAdapter {
    spawned: Arc<AtomicUsize>,
}

#[async_trait]
impl ProviderAdapter for CountingSpawnAdapter {
    fn kind(&self) -> &'static str {
        "terminal-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CARD_SPAWN_ADAPTER_PHASES
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
        calm_server::operation::refuse_if_context_stale(
            tx,
            input.get("idempotency_key").and_then(Value::as_str),
        )
        .await?;
        Ok(TxOutput::new("card", None, serde_json::json!({})))
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
        self.spawned.fetch_add(1, Ordering::SeqCst);
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        _reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Err(CalmError::Internal(
            "counting spawn fixture unexpected plan_compensation".into(),
        ))
    }

    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(CalmError::Internal(
            "counting spawn fixture unexpected compensate_step".into(),
        ))
    }
}

#[async_trait]
impl ProviderAdapter for CardSpawnAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CARD_SPAWN_ADAPTER_PHASES
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
            serde_json::json!({ "id": self.card_id }),
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
        Err(CalmError::Internal(
            "card-spawn test fixture unexpected plan_compensation".into(),
        ))
    }

    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(CalmError::Internal(
            "card-spawn test fixture unexpected compensate_step".into(),
        ))
    }
}

/// Round-2 review F4: a Working track held at `task_budget = 0` with a
/// pending plan task must dispatch when `PATCH /api/tracks` raises the
/// budget — that PATCH emits ONLY `track.updated` (no lifecycle event,
/// no plan.updated), so the dispatcher's subscriber must treat
/// `track.updated` as a scheduler poke instead of waiting for the
/// periodic reconcile tick (300s default — far beyond this test).
#[tokio::test]
async fn track_updated_budget_raise_pokes_scheduler() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, track_id, area_id) = boot().await;
    repo.track_update(
        track_id.as_str(),
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Working),
            task_budget: Some(Some(0)),
            ..Default::default()
        },
    )
    .await
    .expect("hold track at budget 0");
    let worker_card = repo
        .card_create(NewCard {
            track_id: track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("worker card for the spawn stub");
    cache.insert(worker_card.id.clone(), CardRole::Worker, track_id.clone());

    let task_id = format!("{}:budget-held", track_id.as_str());
    let now = now_ms();
    let task = calm_server::model::Task {
        id: task_id.clone(),
        track_id: track_id.as_str().to_string(),
        key: "budget-held".into(),
        kind: calm_server::model::TaskKind::Codex,
        goal: "do budget-held".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status: calm_server::model::TaskStatus::Pending,
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
    };
    crate::support::task::project_task(&repo.sqlite_pool().unwrap(), &task)
        .await
        .expect("project pending task block");

    let operation_repo = Arc::new(SqlxOperationRepo::new(
        repo.sqlite_pool()
            .expect("dispatcher test uses sqlite repo"),
    ));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let completion = OperationCompletionBus::new();
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        stub_daemon(),
        terminal_renderer,
        events.clone(),
        completion.clone(),
    );
    let operation_runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: worker_card.id.to_string(),
        })],
        events.clone(),
        completion,
        spawn_ctx,
    ));
    let _dispatcher = Dispatcher::spawn_with_operation_runtime(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        stub_codex(),
        stub_daemon(),
        None,
        stub_shared(&repo),
        operation_runtime,
        4,
    );

    // A track.updated while the budget is still 0 pokes the scheduler
    // but the §5.2 budget gate holds the task.
    let track = repo.track_get(track_id.as_str()).await.unwrap().unwrap();
    repo.log_pure_event(
        ActorId::User,
        track_scope(&track_id, &area_id),
        None,
        &events,
        &cache,
        &wcc,
        Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(track, None)),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        repo.task_get(&task_id).await.unwrap().unwrap().status,
        calm_server::model::TaskStatus::Pending,
        "budget 0 must keep holding the task"
    );

    // The budget-raise PATCH shape: row update + ONLY a track.updated
    // event (mirror of routes/tracks.rs `update_track` with no lifecycle
    // change).
    repo.track_update(
        track_id.as_str(),
        TrackPatch {
            task_budget: Some(Some(1)),
            ..Default::default()
        },
    )
    .await
    .expect("raise budget");
    let track = repo.track_get(track_id.as_str()).await.unwrap().unwrap();
    repo.log_pure_event(
        ActorId::User,
        track_scope(&track_id, &area_id),
        None,
        &events,
        &cache,
        &wcc,
        Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(track, None)),
    )
    .await
    .unwrap();

    let status = wait_for(Duration::from_secs(5), || {
        let repo = repo.clone();
        let task_id = task_id.clone();
        async move {
            let row = repo.task_get(&task_id).await.unwrap()?;
            (row.status != calm_server::model::TaskStatus::Pending).then_some(row.status)
        }
    })
    .await
    .expect("track.updated must poke the scheduler — task stayed pending until the tick");
    assert!(
        matches!(
            status,
            calm_server::model::TaskStatus::Dispatched | calm_server::model::TaskStatus::Running
        ),
        "raised budget must dispatch the held task; got {status:?}"
    );
}

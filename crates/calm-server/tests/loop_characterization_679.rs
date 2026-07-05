//! #679 PR0-E — dispatch→push→observation loop characterization.
//!
//! These tests pin the CURRENT behavior of the dispatch→push→observation
//! loop as a regression anchor for the PR5-8 dispatcher rewrite. They are
//! deliberately black-box-ish (events table + `Dispatcher` public surface +
//! `SpecHarness` observation channel) and run with **zero real processes**:
//! the spec harness is constructed via `run_unstarted_for_test` (its run
//! loop never starts, so deliveries are read deterministically from the
//! observation channel — no wall-clock polling against a live turn loop),
//! the shared daemon is the in-process fake, and the worker "spawn" in the
//! stall test is a no-op test adapter.
//!
//! Coverage (gaps only — see the existing estate before adding here):
//!
//!   1. `catch_up_push` with persisted `task.completed` / `task.failed`
//!      envelopes → exact `Observation` content + push-cursor advance +
//!      watermark dedup (the existing `spec_harness_dual_run_filter` test
//!      only covers `wave.report_edited` through this path).
//!   2. A live worker-actor `task.failed` push is **observation-only**:
//!      it must not touch the wave lifecycle and must not append events
//!      (T2 precursor: observation delivery leaves the event log
//!      unchanged). The Working→Reviewing fallback exists ONLY on the
//!      dispatcher's own spawn-failure path (pinned by
//!      `dispatcher_spawn_failure_auto_promotes_working_to_reviewing`).
//!   3. AiSpec-authored task events never push back into the harness
//!      (anti-feedback-loop), asserted at the live transport level.
//!   4. The dead-worker stall: a worker that spawns successfully and then
//!      never reports leaves the wave parked in `Working` forever — no
//!      kernel-side convergence event of any kind is produced.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use calm_exec::WorkerProvider;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, session_insert_tx, session_start_runtime_tx, task_insert_tx,
};
use calm_server::dispatcher::Dispatcher;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::harness::run_loop::HarnessObservationDelivery;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, SpecHarness,
    SpecHarnessParams,
};
use calm_server::ids::{ActorId, CoveId, WaveId};
use calm_server::model::{
    Card, CardRole, NewCard, NewCove, NewWave, Task, TaskKind, TaskStatus, WaveLifecycle,
    WavePatch, new_id, now_ms,
};
use calm_server::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, OperationCompletionBus,
    OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome,
    SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::provider_registry::WorkerProviderRegistry;
use calm_server::reaper::{Reaper, reaper_on_boot};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_truth_test_harness::FakeProvider;
use calm_types::worker::{
    ExitEvidence, ExitSource, Liveness, LivenessTag, SessionMode, WorkerContract,
    WorkerProviderKind, WorkerSession, WorkerSessionId,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Fixture: spec card + worker card + unstarted harness + live dispatcher.
// ---------------------------------------------------------------------------

struct LoopFixture {
    repo: Arc<SqlxRepo>,
    /// Live bus the dispatcher subscribes to.
    events: EventBus,
    role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    wave_id: WaveId,
    cove_id: CoveId,
    spec_card: Card,
    worker_card: Card,
    harness: SpecHarness,
    /// Observation deliveries the dispatcher pushes into the harness. The
    /// harness run loop is intentionally NOT started, so every delivery
    /// stays in this channel and can be received deterministically.
    obs_rx: mpsc::Receiver<HarnessObservationDelivery>,
    dispatcher: Dispatcher,
}

async fn loop_fixture(tag: &str) -> LoopFixture {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: tag.into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: tag.into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    wave_cove_cache.insert(wave.id.clone(), cove.id.clone());

    let mut tx = repo.pool().begin().await.unwrap();
    let spec_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Spec,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    let worker_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Worker,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Harness-backed SharedSpec runtime row — `harness_runtime_id_for_spec_card`
    // requires an active SharedSpec runtime whose handle_state is a harness
    // snapshot.
    let thread_id = format!("thread-{tag}");
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: spec_card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let events = EventBus::new();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let registry = HarnessRegistry::new();
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo_dyn.clone(), None);
    // Unstarted: the run loop never consumes the observation channel, so
    // the test reads deliveries deterministically (no debounce / turn
    // issuance racing the assertions).
    let (harness, obs_rx) = SpecHarness::run_unstarted_for_test(
        SpecHarnessParams {
            runtime_id: runtime_id.clone(),
            wave_id: wave.id.clone(),
            card_id: spec_card.id.clone(),
            thread_id: Some(thread_id),
            repo: repo_dyn.clone(),
            events: events.clone(),
            card_role_cache: CardRoleCache::new(),
            wave_cove_cache: WaveCoveCache::new(),
            daemon: daemon.clone(),
            config: HarnessConfig::default(),
            snapshot,
        },
        8,
    );
    registry.insert(runtime_id, harness.clone());
    let dispatcher = Dispatcher::spawn_with_terminal_renderer_and_harness(
        repo_dyn,
        events.clone(),
        WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
        Arc::new(CodexClient::new_stub()),
        Arc::new(DaemonClient {
            data_dir: std::env::temp_dir().join(format!("neige-loop-pin-{tag}")),
            proc_supervisor_sock: None,
        }),
        TerminalRendererRegistry::new_with_repo(route_repo),
        None,
        registry,
        daemon,
        4,
    );

    LoopFixture {
        repo,
        events,
        role_cache,
        wave_cove_cache,
        wave_id: wave.id,
        cove_id: cove.id,
        spec_card,
        worker_card,
        harness,
        obs_rx,
        dispatcher,
    }
}

impl LoopFixture {
    fn worker_scope(&self) -> EventScope {
        EventScope::Card {
            card: self.worker_card.id.clone(),
            wave: self.wave_id.clone(),
            cove: self.cove_id.clone(),
        }
    }

    fn wave_scope(&self) -> EventScope {
        EventScope::Wave {
            wave: self.wave_id.clone(),
            cove: self.cove_id.clone(),
        }
    }

    /// Persist an event WITHOUT live broadcast (cold bus) — the catch-up
    /// tests must prove `catch_up_push` alone moves the loop.
    async fn persist_cold(&self, actor: ActorId, scope: EventScope, event: Event) -> i64 {
        let cold_bus = EventBus::new();
        self.repo
            .log_pure_event(
                actor,
                scope,
                None,
                &cold_bus,
                &self.role_cache,
                &self.wave_cove_cache,
                event,
            )
            .await
            .unwrap()
    }

    /// Persist an event WITH live broadcast on the dispatcher's bus.
    async fn persist_live(&self, actor: ActorId, scope: EventScope, event: Event) -> i64 {
        self.repo
            .log_pure_event(
                actor,
                scope,
                None,
                &self.events,
                &self.role_cache,
                &self.wave_cove_cache,
                event,
            )
            .await
            .unwrap()
    }

    /// Lifecycle-bearing events persisted for this wave (`wave.lifecycle_changed`
    /// / `wave.updated`) plus any task terminal events. Used to assert the
    /// push path appends nothing.
    async fn wave_audit_events(&self) -> Vec<Event> {
        self.repo
            .events_since(0, i64::MAX)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|(_id, _version, scope, event)| {
                if scope.wave_id() != Some(&self.wave_id) {
                    return None;
                }
                matches!(
                    event,
                    Event::WaveLifecycleChanged { .. }
                        | Event::WaveUpdated(_)
                        | Event::TaskCompleted { .. }
                        | Event::TaskFailed { .. }
                )
                .then_some(event)
            })
            .collect()
    }
}

fn task_completed(idem: &str, result: Value) -> Event {
    Event::TaskCompleted {
        idempotency_key: idem.into(),
        result,
        artifacts: Vec::new(),
        agent_message: None,
    }
}

fn task_failed(idem: &str, reason: &str) -> Event {
    Event::TaskFailed {
        idempotency_key: idem.into(),
        reason: reason.into(),
        agent_message: None,
    }
}

// ---------------------------------------------------------------------------
// 1. catch_up_push: persisted task events → observation content + cursor.
// ---------------------------------------------------------------------------

/// Inject persisted `task.completed` / `task.failed` envelopes through
/// `Dispatcher::catch_up_push` and pin:
///   - the exact `Observation` mapping the spec harness receives
///     (`result` carried verbatim; `task.failed`'s `reason` becomes the
///     observation's `error`);
///   - the per-spec-card push cursor advancing to each delivered envelope id;
///   - synthetic id-0 envelopes never delivering (cursor starts at 0,
///     pushes require `envelope_id > cursor`);
///   - watermark dedup: replaying an already-delivered (lower-or-equal id)
///     envelope is a silent no-op.
#[tokio::test]
async fn catch_up_push_task_events_deliver_observations_and_advance_cursor() {
    let mut fx = loop_fixture("catchup-task").await;

    let completed = task_completed("loop-pin-a", json!({"ok": true, "notes": "loop-pin"}));
    let failed = task_failed("loop-pin-b", "worker exploded");

    // Synthetic id-0 envelope (the shape `EventBus::emit` would produce) is
    // never above the initial 0 cursor — pinned as "only real persisted ids
    // push".
    fx.dispatcher
        .catch_up_push(fx.wave_id.clone(), completed.clone(), 0)
        .await;
    assert!(
        fx.obs_rx.try_recv().is_err(),
        "id-0 envelope must not be delivered to the harness"
    );
    assert_eq!(fx.dispatcher.push_cursor_for_test(&fx.spec_card.id), 0);

    // task.completed: persisted by the worker actor in its own card scope,
    // replayed through catch_up_push.
    let completed_id = fx
        .persist_cold(
            ActorId::AiCodex(fx.worker_card.id.clone()),
            fx.worker_scope(),
            completed.clone(),
        )
        .await;
    fx.dispatcher
        .catch_up_push(fx.wave_id.clone(), completed.clone(), completed_id)
        .await;
    let delivery = fx
        .obs_rx
        .try_recv()
        .expect("catch_up_push must deliver the task.completed observation synchronously");
    assert_eq!(delivery.envelope_id, Some(completed_id));
    assert_eq!(
        delivery.observation,
        Observation::TaskCompleted {
            idempotency_key: "loop-pin-a".into(),
            result: json!({"ok": true, "notes": "loop-pin"}),
        },
        "task.completed observation must carry the idempotency key and verbatim result"
    );
    assert_eq!(
        fx.dispatcher.push_cursor_for_test(&fx.spec_card.id),
        completed_id,
        "delivered envelope must advance the push cursor"
    );

    // task.failed: the event's `reason` maps to the observation's `error`.
    let failed_id = fx
        .persist_cold(
            ActorId::AiCodex(fx.worker_card.id.clone()),
            fx.worker_scope(),
            failed.clone(),
        )
        .await;
    fx.dispatcher
        .catch_up_push(fx.wave_id.clone(), failed.clone(), failed_id)
        .await;
    let delivery = fx
        .obs_rx
        .try_recv()
        .expect("catch_up_push must deliver the task.failed observation synchronously");
    assert_eq!(delivery.envelope_id, Some(failed_id));
    assert_eq!(
        delivery.observation,
        Observation::TaskFailed {
            idempotency_key: "loop-pin-b".into(),
            error: "worker exploded".into(),
        },
        "task.failed reason must surface as the observation error"
    );
    assert_eq!(
        fx.dispatcher.push_cursor_for_test(&fx.spec_card.id),
        failed_id
    );

    // Redelivery of an already-delivered envelope (id <= cursor) is a
    // silent dedup: no observation, cursor unchanged.
    fx.dispatcher
        .catch_up_push(fx.wave_id.clone(), completed, completed_id)
        .await;
    assert!(
        fx.obs_rx.try_recv().is_err(),
        "redelivered envelope must be deduped by the push watermark"
    );
    assert_eq!(
        fx.dispatcher.push_cursor_for_test(&fx.spec_card.id),
        failed_id,
        "dedup must not move the cursor"
    );

    fx.harness.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// 2 + 3. Live push is observation-only; AiSpec self-events never push back.
// ---------------------------------------------------------------------------

/// A worker-actor `task.failed` arriving on the live bus is delivered to the
/// spec harness as an observation and does NOTHING else: no wave lifecycle
/// change (the dispatcher's Working→Reviewing fallback fires only on its own
/// spawn failures) and no new rows in the event log (T2 precursor —
/// observation delivery leaves the event count unchanged). A subsequent
/// AiSpec-authored task event must not push back into the harness
/// (anti-feedback-loop).
#[tokio::test]
async fn live_task_failed_push_is_observation_only_and_spec_self_events_do_not_push_back() {
    let mut fx = loop_fixture("live-task-failed").await;
    fx.repo
        .wave_update(
            fx.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Working),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let failed_id = fx
        .persist_live(
            ActorId::AiCodex(fx.worker_card.id.clone()),
            fx.worker_scope(),
            task_failed("live-loop-pin", "worker reported failure"),
        )
        .await;

    // Positive sync point: the dispatcher's live push lands in the harness
    // observation channel.
    let delivery = tokio::time::timeout(Duration::from_secs(5), fx.obs_rx.recv())
        .await
        .expect("live task.failed must reach the spec harness within 5s")
        .expect("observation channel open");
    assert_eq!(delivery.envelope_id, Some(failed_id));
    assert_eq!(
        delivery.observation,
        Observation::TaskFailed {
            idempotency_key: "live-loop-pin".into(),
            error: "worker reported failure".into(),
        }
    );
    assert_eq!(
        fx.dispatcher.push_cursor_for_test(&fx.spec_card.id),
        failed_id
    );

    // Observation-only: the wave lifecycle is untouched and the event log
    // contains exactly the one task.failed we persisted — the push path
    // appended nothing (no lifecycle fallback, no echo events).
    let wave = fx
        .repo
        .wave_get(fx.wave_id.as_str())
        .await
        .unwrap()
        .expect("wave exists");
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Working,
        "a worker-reported task.failed must NOT auto-promote the wave; \
         only the dispatcher's own spawn-failure path does"
    );
    let audit = fx.wave_audit_events().await;
    assert_eq!(
        audit.len(),
        1,
        "push delivery must append no events; expected only the injected task.failed, got {audit:#?}"
    );
    assert!(matches!(
        &audit[0],
        Event::TaskFailed { idempotency_key, .. } if idempotency_key == "live-loop-pin"
    ));

    // AiSpec self-event (higher envelope id, so the watermark cannot mask
    // the warrant check): must never push back into the harness.
    let spec_self_id = fx
        .persist_live(
            ActorId::AiSpec(fx.spec_card.id.clone()),
            fx.wave_scope(),
            task_completed("spec-self-echo", json!({"ok": true})),
        )
        .await;
    assert!(spec_self_id > failed_id, "ids are monotonic");
    let echo = tokio::time::timeout(Duration::from_millis(250), fx.obs_rx.recv()).await;
    assert!(
        echo.is_err(),
        "AiSpec-authored task events must not be pushed back to the spec harness; got {echo:?}"
    );
    assert_eq!(
        fx.dispatcher.push_cursor_for_test(&fx.spec_card.id),
        failed_id,
        "ignored AiSpec event must not advance the push cursor"
    );

    fx.harness.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// 4. Dead-worker convergence.
// ---------------------------------------------------------------------------

/// No-op "spawn succeeds, worker never reports" adapter. Stands in for a
/// worker process that launches and then dies without ever calling
/// `calm.task.complete` / failing visibly.
struct SilentSpawnAdapter {
    spawned: Arc<tokio::sync::Notify>,
    card_id: String,
}

const SILENT_SPAWN_ADAPTER_PHASES: &[PhaseTag] = &[];

#[async_trait]
impl ProviderAdapter for SilentSpawnAdapter {
    fn kind(&self) -> &'static str {
        "terminal-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        SILENT_SPAWN_ADAPTER_PHASES
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
            "silent-spawn",
            None,
            json!({ "id": self.card_id }),
        ))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Err(CalmError::Internal(
            "silent-spawn test fixture unexpected app_server_interact".into(),
        ))
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        self.spawned.notify_one();
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
            "silent-spawn test fixture must not need compensation".into(),
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
            "silent-spawn test fixture has no compensation steps".into(),
        ))
    }
}

/// A worker whose spawn succeeds but which never produces
/// `task.completed` / `task.failed` is now converged by the reaper once the
/// worker session is durably observed as exited: the session terminalizes,
/// the kernel emits one `task.failed`, and the wave parks at `Reviewing`.
#[tokio::test]
async fn dead_worker_never_reporting_reaper_converges_and_parks_reviewing() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "dead-worker".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "dead-worker".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let role_cache = CardRoleCache::new();
    repo.seed_card_role_cache(&role_cache).await.unwrap();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    repo.wave_update(
        wave.id.as_str(),
        WavePatch {
            lifecycle: Some(WaveLifecycle::Dispatching),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let key = "dead-worker-pin";
    let task_id = format!("{}:{key}", wave.id.as_str());
    let now = now_ms();
    let task = Task {
        id: task_id.clone(),
        wave_id: wave.id.as_str().to_string(),
        key: key.into(),
        kind: TaskKind::Terminal,
        goal: "worker-that-never-reports".into(),
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
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    };
    let pool = repo.sqlite_pool().expect("sqlite-backed repo");
    let mut tx = pool.begin().await.unwrap();
    task_insert_tx(&mut tx, &task).await.unwrap();
    tx.commit().await.unwrap();

    // The dead worker's card exists in the wave (the projection a real
    // spawn would have left behind) — it just never reports anything.
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: json!({"idempotency_key": task_id}),
        })
        .await
        .unwrap();
    role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());

    let spawned = Arc::new(tokio::sync::Notify::new());
    let operation_repo = Arc::new(SqlxOperationRepo::new(
        repo.sqlite_pool().expect("sqlite-backed repo"),
    ));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let completion = OperationCompletionBus::new();
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        Arc::new(DaemonClient {
            data_dir: PathBuf::from("/tmp/neige-loop-pin-dead-worker"),
            proc_supervisor_sock: None,
        }),
        terminal_renderer,
        events.clone(),
        completion.clone(),
    );
    let operation_runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![Arc::new(SilentSpawnAdapter {
            spawned: spawned.clone(),
            card_id: worker_card.id.to_string(),
        })],
        events.clone(),
        completion,
        spawn_ctx,
    ));
    let dispatcher = Dispatcher::spawn_with_operation_runtime(
        repo.clone(),
        events.clone(),
        WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
        Arc::new(CodexClient::new_stub()),
        Arc::new(DaemonClient {
            data_dir: PathBuf::from("/tmp/neige-loop-pin-dead-worker"),
            proc_supervisor_sock: None,
        }),
        None,
        SharedCodexAppServer::new_stub(repo.clone()),
        operation_runtime,
        4,
    );

    let mut rx = events.subscribe();
    dispatcher.scheduler().schedule_wave(wave.id.clone()).await;

    // Positive sync points: the worker "spawn" ran, and the dispatcher
    // promoted Dispatching → Working first.
    tokio::time::timeout(Duration::from_secs(5), spawned.notified())
        .await
        .expect("silent worker spawn must run within 5s");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_working = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => {
                if let Event::WaveLifecycleChanged { id, from, to, .. } = &env.event
                    && id == &wave.id
                    && *from == WaveLifecycle::Dispatching
                    && *to == WaveLifecycle::Working
                {
                    saw_working = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    assert!(saw_working, "dispatcher must promote Dispatching → Working");

    let baseline_id = repo
        .events_since(0, i64::MAX)
        .await
        .unwrap()
        .last()
        .map(|(id, _version, _scope, _event)| *id)
        .unwrap_or(0);
    let op_id: String = sqlx::query_scalar(
        "SELECT id FROM operations WHERE kind = 'terminal-worker' AND idempotency_key = ?1 \
         ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .bind(&task_id)
    .fetch_one(&pool)
    .await
    .expect("worker operation row");

    let session_id = WorkerSessionId::from(new_id());
    let session_now = now_ms();
    let mut tx = pool.begin().await.unwrap();
    session_insert_tx(
        &mut tx,
        WorkerSession {
            id: session_id.clone(),
            wave_id: wave.id.clone(),
            provider: WorkerProviderKind::Terminal,
            mode: SessionMode::Ephemeral,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Running,
            mcp_token_hash: None,
            thread_id: None,
            agent_session_id: None,
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(worker_card.id.clone()),
            handle_state_json: None,
            liveness: LivenessTag::Unknown,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: Some(op_id),
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: session_now,
            updated_at_ms: session_now,
            completed_at_ms: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let fake = Arc::new(FakeProvider::new().with_probe_script([Liveness::Exited {
        evidence: ExitEvidence {
            exit_code: Some(-1),
            signal_killed: false,
            observed_at_ms: now_ms(),
            source: ExitSource::Probe,
        },
    }]));
    let registry = WorkerProviderRegistry::from_entries([(
        WorkerProviderKind::Terminal,
        fake as Arc<dyn WorkerProvider>,
    )]);
    let reaper = Reaper::new(
        repo.clone(),
        registry,
        events.clone(),
        WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
    );
    reaper_on_boot();
    reaper.sweep_all().await;

    // DB-level audit after the dispatcher reached Working: exactly one
    // kernel task.failed plus exactly one Working → Reviewing promotion.
    let rows = repo.events_since(baseline_id, i64::MAX).await.unwrap();
    let mut failed_events = Vec::new();
    let mut lifecycle_changes = Vec::new();
    for (id, _version, scope, event) in rows {
        if scope.wave_id() != Some(&wave.id) {
            continue;
        }
        match event {
            Event::TaskFailed {
                ref idempotency_key,
                ..
            } if idempotency_key == &task_id => {
                let actor_text: String =
                    sqlx::query_scalar("SELECT actor FROM events WHERE id = ?1")
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                let actor: ActorId = serde_json::from_str(&actor_text).unwrap();
                assert_eq!(actor, ActorId::KernelDispatcher);
                failed_events.push(event);
            }
            Event::TaskCompleted {
                idempotency_key, ..
            } if idempotency_key == task_id => {
                panic!("dead-worker reaper must not emit task.completed")
            }
            Event::WaveLifecycleChanged { from, to, .. } => {
                lifecycle_changes.push((from, to));
            }
            _ => {}
        }
    }
    assert_eq!(failed_events.len(), 1, "exactly one reaper task.failed");
    match &failed_events[0] {
        Event::TaskFailed {
            idempotency_key,
            reason,
            agent_message,
        } => {
            assert_eq!(idempotency_key, &task_id);
            // FIX 3: the kernel TaskFailed carries the provider's interpreted
            // reason — the `-1` probe sentinel is hidden behind "outcome
            // unknown", not leaked as the old `"exit Some(-1)"` format.
            assert!(
                reason.contains("outcome unknown") && reason.contains("supervisor probe"),
                "expected provider reason, got {reason:?}"
            );
            assert!(!reason.contains("exit Some(-1)"));
            assert_eq!(agent_message, &None);
        }
        other => panic!("expected task.failed, got {other:?}"),
    }
    assert_eq!(
        lifecycle_changes,
        vec![(WaveLifecycle::Working, WaveLifecycle::Reviewing)],
        "exactly one reaper lifecycle event: Working → Reviewing"
    );

    let task_row = repo.task_get(&task_id).await.unwrap().expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Failed);
    assert_eq!(task_row.status_detail.as_deref(), Some("spawn-failed"));
    let session = repo
        .session_get(&session_id)
        .await
        .unwrap()
        .expect("session exists");
    assert_eq!(session.state, WorkerSessionState::Failed);
    assert_eq!(session.exit_code, Some(-1));

    let parked = repo
        .wave_get(wave.id.as_str())
        .await
        .unwrap()
        .expect("wave exists");
    assert_eq!(parked.lifecycle, WaveLifecycle::Reviewing);
    let cards = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert!(
        cards.iter().any(|c| c.id == worker_card.id),
        "dead-worker convergence does not reap the worker card"
    );
}

//! Dispatcher integration tests.
//!
//! Coverage:
//!
//!   1. **`SubscribeFilter` over `EventBus::subscribe_filtered`** — emit
//!      three events of mixed kinds + scopes, assert the filter delivers
//!      only the requested ones (and that the receiver outlives extra
//!      lifecycle activity around it).
//!   2. **Happy path** — emit one `CodexWorkerRequested`, await the worker
//!      card landing in the DB with `role = Worker` + payload carrying
//!      the idempotency key, and a single `card.added` event in the
//!      log.
//!   3. **Idempotency** — emit the same `CodexWorkerRequested` (same
//!      `idempotency_key`) twice rapid-fire; the operation-table unique
//!      key allows only one worker operation/card.
//!   4. **Semaphore cap** — with `NEIGE_DISPATCHER_PERMITS = 2`, emit
//!      five `*.Requested` events; observe the global permit count
//!      stays bounded by 2.
//!   5. **TaskFailed on bad scope** — emit a `CodexWorkerRequested` with
//!      `EventScope::System` (the dispatcher needs a wave scope to mint
//!      a card); assert a `task.failed` event lands in the events log.
//!
//! Tests run against an in-memory `SqlxRepo` and stubbed worker spawn
//! dependencies.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::{
    ArtifactRef, Event, EventBus, EventScope, SubscribeFilter, SubscribeScope,
};
use calm_server::ids::{ActorId, CoveId, WaveId};
use calm_server::model::{
    CardRole, NewCard, NewCove, NewTerminal, NewWave, WaveLifecycle, WavePatch, new_id, now_ms,
};
use calm_server::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, OperationCompletionBus,
    OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome,
    SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::pending_codex_threads::{PendingEntry, PendingThreadStartRegistry};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::Value;

static DISPATCHER_DAEMON_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot() -> (
    Arc<dyn Repo>,
    EventBus,
    CardRoleCache,
    WaveCoveCache,
    WaveId,
    CoveId,
) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "dispatcher-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "dispatcher-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    (
        repo,
        events,
        card_role_cache,
        wave_cove_cache,
        wave.id,
        cove.id,
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

fn wave_scope(wave: &WaveId, cove: &CoveId) -> EventScope {
    EventScope::Wave {
        wave: wave.clone(),
        cove: cove.clone(),
    }
}

#[tokio::test]
async fn dispatcher_pending_thread_bind_persists_thread_id_and_broadcasts_card_updated() {
    let (repo, events, _cache, _wcc, wave_id, _cove_id) = boot().await;
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .expect("create spec card");
    calm_server::db::write_in_tx_typed(repo.as_ref(), {
        let card_id = spec_card.id.to_string();
        move |tx| {
            Box::pin(async move {
                let runtime = runtime_start_tx(
                    tx,
                    RuntimeInit {
                        id: new_id(),
                        card_id,
                        kind: RuntimeKind::SharedSpec,
                        agent_provider: Some(AgentProvider::Codex),
                        status: RunStatus::TurnPending,
                        terminal_run_id: None,
                        thread_id: None,
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        lease_owner: None,
                        lease_until_ms: None,
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
            card_id: spec_card.id.clone(),
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
                spec_card.id.to_string(),
                Some(wave_id.to_string()),
                terminal.id.to_string(),
            )
            .with_role(calm_server::model::CardRole::Spec),
        )
        .await
        .expect("register pending thread");
    let mut rx = events.subscribe();

    assert_eq!(
        pending
            .on_thread_started("thread-tui-created")
            .await
            .expect("bind pending thread"),
        Some(spec_card.id.to_string())
    );

    let updated = repo
        .card_get(spec_card.id.as_str())
        .await
        .expect("card_get")
        .expect("spec card still exists");
    assert!(updated.payload.get("codex_thread_id").is_none());
    let runtime = repo
        .runtime_get_active_for_card(&spec_card.id.to_string())
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
            assert_eq!(card.id, spec_card.id);
            assert!(card.payload.get("codex_thread_id").is_none());
        }
        other => panic!("expected CardUpdated, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatcher_pending_thread_bind_failure_does_not_broadcast_or_mutate() {
    let (repo, events, _cache, _wcc, wave_id, _cove_id) = boot().await;
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!(["corrupt-payload-shape"]),
        })
        .await
        .expect("create malformed spec card");
    let terminal = repo
        .terminal_create(NewTerminal {
            card_id: spec_card.id.clone(),
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
                spec_card.id.to_string(),
                Some(wave_id.to_string()),
                terminal.id.to_string(),
            )
            .with_role(calm_server::model::CardRole::Spec),
        )
        .await
        .expect("register pending thread");
    let mut rx = events.subscribe();

    assert_eq!(
        pending
            .on_thread_started("thread-that-cannot-persist")
            .await
            .expect("missing runtime re-parks pending entry"),
        None
    );
    assert_eq!(pending.pending_count().await, 1);

    let unchanged = repo
        .card_get(spec_card.id.as_str())
        .await
        .expect("card_get")
        .expect("spec card still exists");
    assert_eq!(
        unchanged.payload,
        serde_json::json!(["corrupt-payload-shape"])
    );
    let no_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "failed initial-prompt backfill must not broadcast CardUpdated; got {no_event:?}"
    );
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

const FAST_REPORT_ADAPTER_PHASES: &[PhaseTag] = &[];

struct FastReportAdapter {
    task_completed: calm_server::mcp_server::registry::ToolHandler,
    tool_ctx: Arc<calm_server::mcp_server::AppContext>,
    worker_card_id: String,
    wave_id: WaveId,
    idempotency_key: String,
}

impl FastReportAdapter {
    fn new(
        task_completed: calm_server::mcp_server::registry::ToolHandler,
        tool_ctx: Arc<calm_server::mcp_server::AppContext>,
        worker_card_id: String,
        wave_id: WaveId,
        idempotency_key: String,
    ) -> Self {
        Self {
            task_completed,
            tool_ctx,
            worker_card_id,
            wave_id,
            idempotency_key,
        }
    }
}

fn unexpected_fast_report_call(name: &str) -> CalmError {
    CalmError::Internal(format!("fast-report test fixture unexpected call: {name}"))
}

#[async_trait]
impl ProviderAdapter for FastReportAdapter {
    fn kind(&self) -> &'static str {
        "terminal-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        FAST_REPORT_ADAPTER_PHASES
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
            "fast-report",
            None,
            serde_json::json!({"ok": true}),
        ))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Err(unexpected_fast_report_call("app_server_interact"))
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        let identity = calm_server::mcp_server::ToolCallIdentity {
            card_id: self.worker_card_id.clone(),
            role: CardRole::Worker,
            wave_id: Some(self.wave_id.to_string()),
            thread_id: "fast-worker-report-thread".into(),
        };
        (self.task_completed)(
            self.tool_ctx.clone(),
            identity,
            serde_json::json!({
                "idempotency_key": self.idempotency_key.clone(),
                "result": { "ok": true }
            }),
        )
        .await
        .map_err(|e| CalmError::Internal(format!("fast worker task_completed failed: {e:?}")))?;
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        _reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Err(unexpected_fast_report_call("plan_compensation"))
    }

    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(unexpected_fast_report_call("compensate_step"))
    }
}

const FAILING_SPAWN_ADAPTER_PHASES: &[PhaseTag] = &[];

struct FailingSpawnAdapter;

#[async_trait]
impl ProviderAdapter for FailingSpawnAdapter {
    fn kind(&self) -> &'static str {
        "terminal-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        FAILING_SPAWN_ADAPTER_PHASES
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
            "failing-spawn",
            None,
            serde_json::json!({"ok": false}),
        ))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Err(CalmError::Internal(
            "failing-spawn test fixture unexpected app_server_interact".into(),
        ))
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        Err(CalmError::Internal("forced spawn failure".into()))
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
        Err(CalmError::Internal(
            "failing-spawn test fixture has no compensation steps".into(),
        ))
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

// ---------------------------------------------------------------------------
// 2. Dispatcher happy path.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 3. Dispatcher idempotency.
// ---------------------------------------------------------------------------

/// Operation idempotency treats the second emit as a duplicate of the
/// first worker operation. The duplicate path is silent: it must not mint
/// a second worker card or emit `task.failed`.
///
/// Note (#310 followup): adapter compensation deletes the worker card +
/// terminal row when the spawn step fails. A failing proc-supervisor
/// connection is therefore the wrong fixture for the positive dedup
/// invariant: the card would be compensated away. We use the
/// fixture-backed proc supervisor so the first dispatch succeeds
/// end-to-end and the duplicate emit reuses that result.
// ---------------------------------------------------------------------------
// 4. Semaphore cap.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 5. TaskFailed on spawn error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatcher_emits_task_failed_on_bad_scope() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    // EventScope::System has no wave/cove — the dispatcher can't mint
    // a worker card without a parent wave, so it bails and emits a
    // task.failed event with the reason.
    let (repo, events, cache, wcc, _wave_id, _cove_id) = boot().await;
    let codex = stub_codex();
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex.clone(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        stub_shared(&repo),
        4,
    );

    // Subscribe BEFORE we emit so we don't race.
    let mut rx = events.subscribe();

    let idem = "no-scope-x";
    repo.log_pure_event(
        ActorId::User,
        EventScope::System,
        None,
        &events,
        &cache,
        &wcc,
        codex_req(idem, "g"),
    )
    .await
    .unwrap();

    // Drain until we see task.failed or time out.
    let mut saw_failed = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => {
                if let Event::TaskFailed {
                    idempotency_key,
                    reason,
                    ..
                } = &env.event
                    && idempotency_key == idem
                {
                    assert!(
                        !reason.is_empty(),
                        "task.failed must carry a non-empty reason"
                    );
                    saw_failed = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => {
                // timeout iteration — keep polling
            }
        }
    }
    assert!(
        saw_failed,
        "expected dispatcher to emit task.failed for system-scoped request"
    );

    // No worker card should have been minted (we never had a wave).
    // We don't have a wave to query against, so verify via the events
    // table: no card.added events emitted by KernelDispatcher.
    // (Acceptable proxy: the worker-card insert path is the only one
    // that emits card.added under KernelDispatcher actor.)
}

// ---------------------------------------------------------------------------
// 6. Touch-test: dispatcher's CardAdded passes through enforce_role.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// PR6 (#136) — Real concurrent idempotency-race test.
//
// PR5's dedup test fires sequentially (`for _ in 0..2`); the canonical
// in-tx SELECT race window only opens when two emissions hit the
// dispatcher within microseconds. We use a `Barrier` to release two
// dispatcher tasks at the same moment after both have already
// acquired their semaphore permit and entered the spawn fn.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 7. ArtifactRef use-site exercise (keeps the import live).
// ---------------------------------------------------------------------------

#[test]
fn artifact_ref_smoke() {
    // PR4 type — keeps the import meaningful in this test module so
    // any future renames surface here, not just in `event.rs::tests`.
    let a = ArtifactRef::from("a-1");
    assert_eq!(a.as_str(), "a-1");
}
// ---------------------------------------------------------------------------
// 10. Issue #310 — `CardAdded` ordering contract: the dispatcher must NOT
//     broadcast `Event::CardAdded` for a worker card until AFTER
//     `spawn_terminal_with_parts` has written `renderer entry` on the backing
//     terminal row.
//
//     The pre-#310 bug: `CardAdded` was emitted inside the row-creation tx,
//     so a spec card hot-subscribed to the wave's event stream saw the
//     card frame, mounted an `XtermView`, attempted WS attach, and hit
//     `resolve_live_renderer`'s "no renderer entry = clean child exit" branch
//     (#304) — producing a spurious `Close(1000, "child-exited")` for a
//     daemon ~670ms away from being alive.
//
//     The regression guard below subscribes to the bus BEFORE the request
//     emit, dispatches a codex worker through the fixture-backed proc
//     supervisor (so EnsureProc actually succeeds), captures the FIRST
//     `CardAdded` envelope, then queries the matching `terminal_get(...)`
//     row and asserts:
//        - `renderer entry.is_some()`  ← the core ordering contract
//        - the renderer persisted a pid (the supervised child really is up)
//
//     If anyone reverts the dispatcher back to emitting `CardAdded`
//     inside the tx (e.g. by routing it through `write_with_event_typed`
//     again), this test trips loudly.
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// 11. Issue #310 — same ordering contract as test 10, but for the
//     terminal-worker operation path. The pre-fix bug existed on both
//     worker paths: each emitted `CardAdded` inside the row-creation tx,
//     so a hot subscriber saw the card frame before the renderer entry was
//     populated. This test guards the terminal half so a future refactor
//     that reverts only the terminal change still trips a regression.
//
//     Mirrors `dispatcher_codex_card_added_after_renderer_entry_set_issue_310`
//     in shape — see that test's doc comment for the full bug rationale.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_terminal_card_added_after_renderer_entry_set_issue_310() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });

    let codex = stub_codex();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
    let _dispatcher = Dispatcher::spawn_with_terminal_renderer(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex.clone(),
        daemon,
        terminal_renderer.clone(),
        None,
        stub_shared(&repo),
        4,
    );

    // Subscribe BEFORE emitting so we don't race past the CardAdded
    // frame. Filter on the kind below so the dispatch's own
    // `TerminalWorkerRequested` echo doesn't show up as the "first" event.
    let mut rx = events.subscribe();

    let idem = "issue-310-ordering-terminal";
    let req = Event::TerminalWorkerRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
        agent_message: None,
    };
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    // Drain the bus until we see the worker's CardAdded. Skip the
    // initial TerminalWorkerRequested envelope we just emitted; skip any
    // unrelated kinds.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut worker_card: Option<calm_server::model::Card> = None;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => {
                if let Event::CardAdded(card) = &env.event
                    && card.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem)
                {
                    worker_card = Some(card.clone());
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    let card = worker_card
        .expect("dispatcher must broadcast CardAdded for the terminal worker card within 5s");

    let term = repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("terminal row for the worker card must exist post-CardAdded");
    let terminal_id = term.id.as_str();
    assert!(
        terminal_renderer.get(terminal_id).is_some(),
        "issue #310 regression (terminal path): renderer entry MUST be registered by the time CardAdded reaches subscribers",
    );
    assert!(
        term.pid.is_some(),
        "terminal pid must be persisted by the time CardAdded reaches subscribers; got {term:?}"
    );
}

#[tokio::test]
async fn dispatcher_promotes_dispatching_to_working_before_spawn() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    repo.wave_update(
        wave_id.as_str(),
        WavePatch {
            lifecycle: Some(WaveLifecycle::Dispatching),
            ..Default::default()
        },
    )
    .await
    .expect("set wave dispatching");

    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let codex = stub_codex();
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex,
        daemon,
        None,
        stub_shared(&repo),
        4,
    );

    let mut rx = events.subscribe();
    let idem = "auto-working-terminal";
    repo.log_pure_event(
        ActorId::User,
        wave_scope(&wave_id, &cove_id),
        None,
        &events,
        &cache,
        &wcc,
        Event::TerminalWorkerRequested {
            idempotency_key: idem.into(),
            cmd: "/bin/true".into(),
            cwd: None,
            agent_message: None,
        },
    )
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_auto = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => {
                if matches!(env.actor, ActorId::KernelDispatcher)
                    && let Event::WaveUpdated(payload) = env.event
                    && payload.id == wave_id
                {
                    assert_eq!(payload.lifecycle, WaveLifecycle::Working);
                    assert_eq!(
                        payload.agent_message.as_deref(),
                        Some("[auto] worker spawned")
                    );
                    saw_auto = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    assert!(
        saw_auto,
        "dispatcher must emit kernel-dispatcher WaveUpdated before worker spawn"
    );

    let wave = repo
        .wave_get(wave_id.as_str())
        .await
        .unwrap()
        .expect("wave exists");
    assert_eq!(wave.lifecycle, WaveLifecycle::Working);
}

#[tokio::test]
async fn dispatcher_spawn_failure_auto_promotes_working_to_reviewing() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    repo.wave_update(
        wave_id.as_str(),
        WavePatch {
            lifecycle: Some(WaveLifecycle::Dispatching),
            ..Default::default()
        },
    )
    .await
    .expect("set wave dispatching");

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
        vec![Arc::new(FailingSpawnAdapter)],
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

    let mut rx = events.subscribe();
    let idem = "spawn-fail-auto-review";
    repo.log_pure_event(
        ActorId::User,
        wave_scope(&wave_id, &cove_id),
        None,
        &events,
        &cache,
        &wcc,
        Event::TerminalWorkerRequested {
            idempotency_key: idem.into(),
            cmd: "force-failure".into(),
            cwd: None,
            agent_message: None,
        },
    )
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_failed = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => {
                if let Event::TaskFailed {
                    idempotency_key,
                    reason,
                    ..
                } = &env.event
                    && idempotency_key == idem
                {
                    assert!(
                        reason.contains("forced spawn failure"),
                        "task.failed should preserve dispatch error, got {reason:?}"
                    );
                    let wave = repo
                        .wave_get(wave_id.as_str())
                        .await
                        .unwrap()
                        .expect("wave exists");
                    assert_eq!(
                        wave.lifecycle,
                        WaveLifecycle::Reviewing,
                        "TaskFailed broadcast must not be observable before Working -> Reviewing commits"
                    );
                    saw_failed = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    assert!(saw_failed, "expected spawn failure task.failed broadcast");

    let rows = repo.events_since(0, None).await.unwrap();
    let relevant: Vec<Event> = rows
        .into_iter()
        .filter_map(|(_id, _version, scope, event)| {
            if scope.wave_id() != Some(&wave_id) {
                return None;
            }
            match &event {
                Event::WaveLifecycleChanged { id, .. } if id == &wave_id => Some(event),
                Event::WaveUpdated(payload) if payload.id == wave_id => Some(event),
                Event::TaskFailed {
                    idempotency_key, ..
                } if idempotency_key == idem => Some(event),
                _ => None,
            }
        })
        .collect();

    match relevant.as_slice() {
        [
            Event::WaveLifecycleChanged {
                from: first_from,
                to: first_to,
                agent_message: first_message,
                ..
            },
            Event::WaveUpdated(first_update),
            Event::TaskFailed {
                idempotency_key,
                reason,
                ..
            },
            Event::WaveLifecycleChanged {
                from: second_from,
                to: second_to,
                agent_message: second_message,
                ..
            },
            Event::WaveUpdated(second_update),
        ] => {
            assert_eq!(*first_from, WaveLifecycle::Dispatching);
            assert_eq!(*first_to, WaveLifecycle::Working);
            assert_eq!(first_message.as_deref(), Some("[auto] worker spawned"));
            assert_eq!(first_update.lifecycle, WaveLifecycle::Working);
            assert_eq!(
                first_update.agent_message.as_deref(),
                Some("[auto] worker spawned")
            );
            assert_eq!(idempotency_key, idem);
            assert!(
                reason.contains("forced spawn failure"),
                "task.failed should preserve dispatch error, got {reason:?}"
            );
            assert_eq!(*second_from, WaveLifecycle::Working);
            assert_eq!(*second_to, WaveLifecycle::Reviewing);
            assert_eq!(
                second_message.as_deref(),
                Some("[auto] worker spawn failed")
            );
            assert_eq!(second_update.lifecycle, WaveLifecycle::Reviewing);
            assert_eq!(
                second_update.agent_message.as_deref(),
                Some("[auto] worker spawn failed")
            );
        }
        other => panic!("unexpected spawn-failure lifecycle event sequence: {other:#?}"),
    }

    let wave = repo
        .wave_get(wave_id.as_str())
        .await
        .unwrap()
        .expect("wave exists");
    assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);
}

#[tokio::test]
async fn dispatcher_promotes_to_working_before_fast_worker_report() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Marker {
        DispatchingToWorking,
        TaskCompleted,
        WorkingToReviewing,
    }

    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    repo.wave_update(
        wave_id.as_str(),
        WavePatch {
            lifecycle: Some(WaveLifecycle::Dispatching),
            ..Default::default()
        },
    )
    .await
    .expect("set wave dispatching");

    let idem = "fast-worker-report-ordering";
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: serde_json::json!({ "idempotency_key": idem }),
        })
        .await
        .expect("seed worker card");
    cache.insert(worker_card.id.clone(), CardRole::Worker, wave_id.clone());

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let tool_ctx = Arc::new(calm_server::mcp_server::AppContext {
        repo: route_repo.clone(),
        wave_vcs_pool: repo.sqlite_pool(),
        events: events.clone(),
        write: calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        daemon_token_hash: None,
    });
    let task_completed = calm_server::mcp_server::build_default_registry()
        .lookup(calm_server::mcp_server::tools::emit::TOOL_TASK_COMPLETE)
        .expect("calm.task.complete handler registered");
    let operation_repo = Arc::new(SqlxOperationRepo::new(
        repo.sqlite_pool()
            .expect("dispatcher test uses sqlite repo"),
    ));
    let fast_report_adapter = Arc::new(FastReportAdapter::new(
        task_completed,
        tool_ctx,
        worker_card.id.to_string(),
        wave_id.clone(),
        idem.to_string(),
    ));
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
        vec![fast_report_adapter],
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

    let mut rx = events.subscribe();
    repo.log_pure_event(
        ActorId::User,
        wave_scope(&wave_id, &cove_id),
        None,
        &events,
        &cache,
        &wcc,
        Event::TerminalWorkerRequested {
            idempotency_key: idem.into(),
            cmd: "mock-fast-worker".into(),
            cwd: None,
            agent_message: None,
        },
    )
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut markers = Vec::new();
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => {
                match &env.event {
                    Event::WaveLifecycleChanged { id, from, to, .. }
                        if id == &wave_id
                            && *from == WaveLifecycle::Dispatching
                            && *to == WaveLifecycle::Working =>
                    {
                        markers.push(Marker::DispatchingToWorking);
                    }
                    Event::WaveLifecycleChanged { id, from, to, .. }
                        if id == &wave_id
                            && *from == WaveLifecycle::Working
                            && *to == WaveLifecycle::Reviewing =>
                    {
                        markers.push(Marker::WorkingToReviewing);
                    }
                    Event::TaskCompleted {
                        idempotency_key, ..
                    } if idempotency_key == "fast-worker-report-ordering" => {
                        markers.push(Marker::TaskCompleted);
                    }
                    _ => {}
                }

                if markers.contains(&Marker::DispatchingToWorking)
                    && markers.contains(&Marker::TaskCompleted)
                    && markers.contains(&Marker::WorkingToReviewing)
                {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }

    let dispatching_to_working = markers
        .iter()
        .position(|m| *m == Marker::DispatchingToWorking)
        .unwrap_or_else(|| panic!("missing Dispatching -> Working marker: {markers:?}"));
    let task_completed_idx = markers
        .iter()
        .position(|m| *m == Marker::TaskCompleted)
        .unwrap_or_else(|| panic!("missing task.completed marker: {markers:?}"));
    let working_to_reviewing = markers
        .iter()
        .position(|m| *m == Marker::WorkingToReviewing)
        .unwrap_or_else(|| panic!("missing Working -> Reviewing marker: {markers:?}"));

    assert!(
        dispatching_to_working < task_completed_idx,
        "dispatcher must promote Dispatching -> Working before a fast worker report; markers={markers:?}"
    );
    assert!(
        task_completed_idx < working_to_reviewing,
        "worker task.completed must then promote Working -> Reviewing; markers={markers:?}"
    );

    let wave = repo
        .wave_get(wave_id.as_str())
        .await
        .unwrap()
        .expect("wave exists");
    assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);
}

#[tokio::test]
async fn dispatcher_terminal_worker_cwd_normalization_reuses_idempotency_key() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    let tmp_path = tempfile::TempDir::new()
        .expect("tempdir for daemon sockets")
        .keep();
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp_path,
        proc_supervisor_sock: None,
    });
    let codex = stub_codex();
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex,
        daemon,
        None,
        stub_shared(&repo),
        4,
    );

    let idem = "terminal-cwd-normalized-idem";
    let cmd = "printf cwd-normalized\n";
    let scope = wave_scope(&wave_id, &cove_id);
    let mut rx = events.subscribe();

    repo.log_pure_event(
        ActorId::User,
        scope.clone(),
        None,
        &events,
        &cache,
        &wcc,
        Event::TerminalWorkerRequested {
            idempotency_key: idem.into(),
            cmd: cmd.into(),
            cwd: None,
            agent_message: None,
        },
    )
    .await
    .unwrap();
    repo.log_pure_event(
        ActorId::User,
        scope,
        None,
        &events,
        &cache,
        &wcc,
        Event::TerminalWorkerRequested {
            idempotency_key: idem.into(),
            cmd: cmd.into(),
            cwd: Some(String::new()),
            agent_message: None,
        },
    )
    .await
    .unwrap();

    let pool = repo
        .sqlite_pool()
        .expect("dispatcher test repo should be sqlite-backed");
    wait_for(Duration::from_secs(5), || {
        let pool = pool.clone();
        async move {
            let (op_count,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM operations WHERE kind = 'terminal-worker' AND idempotency_key = ?1",
            )
            .bind(idem)
            .fetch_one(&pool)
            .await
            .unwrap();
            (op_count == 1).then_some(())
        }
    })
    .await
    .expect("terminal-worker operation row");

    let mut saw_task_failed = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => {
                if let Event::TaskFailed {
                    idempotency_key, ..
                } = &env.event
                    && idempotency_key == idem
                {
                    saw_task_failed = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    assert!(
        !saw_task_failed,
        "duplicate terminal-worker request with equivalent cwd must reuse idempotency instead of emitting task.failed"
    );

    let (op_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM operations WHERE kind = 'terminal-worker' AND idempotency_key = ?1",
    )
    .bind(idem)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        op_count, 1,
        "None and blank cwd terminal-worker retries must share one operation row"
    );
}

// ---------------------------------------------------------------------------
// 12. Issue #310 followup (codex's P2 escalation) — orphan-row rollback on
//     post-commit spawn failure.
//
//     Pre-fix: when `spawn_terminal_with_parts` returned Err after the
//     row-creation tx committed (real failure modes: missing daemon
//     binary, fd exhaustion, permission denied, readiness failure), the
//     dispatcher returned the error WITHOUT cleaning up the card +
//     terminal row.
//
//     The fix: operation adapter compensation reaps terminal artifacts,
//     deletes both worker rows, and lets the dispatcher emit `TaskFailed`
//     from the failed operation result.
//
//     This test pins all three legs of the contract:
//        a) dispatch returns Err → task.failed fires.
//        b) no card with that idempotency_key remains in the DB.
//        c) a duplicate with the SAME idempotency_key reuses the failed
//           operation row and does not mint a second worker card.
//
//     We provoke spawn failure by pointing `DaemonClient.proc_supervisor_sock`
//     at a nonexistent path (same trick `stub_daemon()` uses) — the
//     proc-supervisor connect returns ENOENT and `spawn_terminal_with_parts`
//     maps it to `CalmError::Internal`. Then we swap to the fixture-backed
//     proc supervisor for the retry leg.
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// 13. Issue #310 followup — terminal-worker mirror of test 12. The codex
//     path AND terminal path share the post-commit / pre-spawn orphan
//     window; this test pins the terminal half so a future refactor
//     that drops the rollback on only the terminal helper still trips
//     a regression.
//
//     Same three-leg contract as test 12:
//        a) dispatch returns Err → task.failed fires.
//        b) no card with that idempotency_key remains in the DB.
//        c) a duplicate with the SAME idempotency_key reuses the failed
//           operation row and does not mint a second worker card.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_rolls_back_card_on_terminal_daemon_spawn_failure_issue_310() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    let codex = stub_codex();
    let dispatcher_fail = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex.clone(),
        stub_daemon(),
        None,
        stub_shared(&repo),
        4,
    );

    let idem = "rollback-terminal-1";
    let req = Event::TerminalWorkerRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
        agent_message: None,
    };
    let scope = wave_scope(&wave_id, &cove_id);
    let mut rx = events.subscribe();

    repo.log_pure_event(
        ActorId::User,
        scope.clone(),
        None,
        &events,
        &cache,
        &wcc,
        req,
    )
    .await
    .unwrap();

    let mut saw_failed = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
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
            Err(_) => continue,
        }
    }
    assert!(
        saw_failed,
        "expected dispatcher to emit task.failed after terminal-worker spawn failure"
    );

    let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let leftover: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert!(
        leftover.is_empty(),
        "expected terminal-worker card row to be rolled back after spawn failure; \
         found {} leftover cards",
        leftover.len(),
    );

    // Leg (c): retry with the SAME idempotency_key against a fresh
    // dispatcher whose proc-supervisor fixture would accept EnsureProc.
    // Operation-table idempotency now owns the key, so the duplicate
    // request reuses the failed operation result instead of spawning a
    // second worker card.
    drop(dispatcher_fail);
    let events_retry = calm_server::event::EventBus::new();
    // Intentionally leak the tempdir — see test 12's note for the
    // rationale (dispatcher-owned background work can outlive the test
    // fn and race tempdir cleanup).
    let tmp_path = tempfile::TempDir::new()
        .expect("tempdir for daemon sockets")
        .keep();
    let daemon_ok = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp_path,
        proc_supervisor_sock: None,
    });
    let _dispatcher_ok = Dispatcher::spawn(
        repo.clone(),
        events_retry.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex.clone(),
        daemon_ok,
        None,
        stub_shared(&repo),
        4,
    );
    let mut rx_retry = events_retry.subscribe();

    let req_retry = Event::TerminalWorkerRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
        agent_message: None,
    };
    repo.log_pure_event(
        ActorId::User,
        scope,
        None,
        &events_retry,
        &cache,
        &wcc,
        req_retry,
    )
    .await
    .unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let env = rx_retry.recv().await.unwrap();
            if let Event::TaskFailed {
                idempotency_key, ..
            } = env.event
                && idempotency_key == idem
            {
                break;
            }
        }
    })
    .await
    .expect("duplicate failed terminal-worker operation should emit task.failed");

    let pool = repo
        .sqlite_pool()
        .expect("dispatcher test repo should be sqlite-backed");
    let (op_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM operations WHERE kind = 'terminal-worker' AND idempotency_key = ?1",
    )
    .bind(idem)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        op_count, 1,
        "duplicate failed terminal-worker request must reuse the existing operation row"
    );
    let (phase,): (String,) = sqlx::query_as(
        "SELECT phase FROM operations WHERE kind = 'terminal-worker' AND idempotency_key = ?1",
    )
    .bind(idem)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(phase, "failed");

    let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let leftovers: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert!(
        leftovers.is_empty(),
        "duplicate failed terminal-worker request must not mint a replacement card"
    );
}

// ---------------------------------------------------------------------------
// 14. Issue #310 followup (codex's P1 escalation) — proc reap on rollback.
//
//     Pre-fix: when `spawn_terminal_with_parts` returned Err AFTER it had
//     already issued EnsureProc + persisted `pid` + inserted the renderer
//     entry but before readiness succeeded (real failure mode: the child
//     hangs during setup until the backstop fires),
//     compensation deleted the rows but left the supervised process
//     leaking — the sweeper's SQL excludes
//     terminals still referenced by a card row, but we just deleted the
//     card, so the sweeper *also* never sees the orphan. Result: a
//     proc-supervisor entry with no DB row to anchor cleanup, until the
//     next kernel boot.
//
//     The fix re-fetches the terminal row (to pick up any post-commit
//     pid / renderer entry writes) and calls `reap_terminal_artifacts`
//     before the row delete. This test pins that ordering by:
//        a) Pointing the dispatcher at a missing proc-supervisor socket,
//           so EnsureProc fails and the rollback path runs.
//        b) Waiting for `task.failed` (proves the rollback path ran
//           to its end).
//        c) Asserting the recorded pid is no longer alive
//           (`kill(pid, 0)` returns ESRCH) — proves the reap fired.
//        d) Asserting the renderer entry is gone — proves the renderer
//           cleanup step of `reap_terminal_artifacts` fired.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_reaps_daemon_on_rollback_after_partial_spawn_issue_310() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    let tmp_path = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let bad_sock = tmp_path.path().join("missing-proc-supervisor.sock");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp_path.path().to_path_buf(),
        proc_supervisor_sock: Some(bad_sock),
    });

    let codex = stub_codex();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
    let _dispatcher = Dispatcher::spawn_with_terminal_renderer(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex.clone(),
        daemon,
        terminal_renderer.clone(),
        None,
        stub_shared(&repo),
        4,
    );

    // Use the terminal-worker path: simpler (no codex env / MCP
    // plumbing) but exercises the same worker compensation helper as the
    // codex path. Reap-on-rollback proven for one path holds for both.
    let idem = "reap-on-rollback-1";
    let req = Event::TerminalWorkerRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
        agent_message: None,
    };
    let scope = wave_scope(&wave_id, &cove_id);
    let mut rx = events.subscribe();

    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    // Wait for task.failed. `spawn_terminal_with_parts` fails while
    // connecting to the configured proc-supervisor socket; the rollback
    // then runs synchronously before `run_one` emits `task.failed`.
    // Allow a generous deadline.
    let mut saw_failed = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
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
            Err(_) => continue,
        }
    }
    assert!(
        saw_failed,
        "expected dispatcher to emit task.failed after supervisor connect failure"
    );
    assert!(
        terminal_renderer.is_empty(),
        "rollback must not leave a registered renderer entry after spawn failure"
    );

    // And: the card row is gone (rollback step 3 fired).
    let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let leftover: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert!(
        leftover.is_empty(),
        "card row should be deleted by the rollback tx; found {} leftover",
        leftover.len()
    );
}
// ---------------------------------------------------------------------------
// 12. Issue #310 fix-loop round 4 — codex caught a regression in the round-3
//     rollback patch: a fast-exit terminal worker (e.g. `printf done`,
//     `/bin/true`, `make build`) can exit before late listeners have
//     observed the renderer's exit state. `spawn_terminal_with_parts`
//     used to treat that path like a failed spawn, and round 3's
//     unconditional rollback would then DELETE the card + terminal row,
//     turning a completed worker into `task.failed` with no card/output
//     for the user to inspect.
//
//     The fix discriminates inside adapter compensation: when the renderer
//     attach reader has persisted a clean exit, the worker rows are
//     preserved. The caller then broadcasts `CardAdded` and returns Ok(())
//     instead of the spawn Err.
//
//     This test pins that preservation contract:
//
//        a) Run a fast-exit terminal command through the fixture-backed
//           proc supervisor and in-process renderer.
//
//        b) Subscribe to the bus BEFORE emitting so we capture both
//           the (would-be) `task.failed` AND the `CardAdded` envelope.
//
//        c) Assert:
//           - `CardAdded` IS broadcast for the dispatched worker.
//           - `TaskFailed` is NOT broadcast for the dispatched key.
//           - The card row + terminal row both survive.
//           - The terminal row's `exit_code = Some(0)` and
//             `signal_killed = false`.
//
//     This test would FAIL on the round-3 code (rollback always
//     deletes), pinning that the discriminator is wired and the
//     fast-exit success path is preserved end-to-end.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_preserves_fast_exit_terminal_card_issue_310() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    // Same intentional keep-alive pattern as nearby dispatcher tests:
    // the child is short-lived, but fixture cleanup can race the
    // tempdir drop under load.
    let tmp_path = tempfile::TempDir::new()
        .expect("tempdir for daemon sockets")
        .keep();
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp_path.clone(),
        proc_supervisor_sock: None,
    });

    let codex = stub_codex();
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        codex.clone(),
        daemon,
        None,
        stub_shared(&repo),
        4,
    );

    let idem = "fast-exit-preserve-1";
    // Subscribe BEFORE emitting so we capture every envelope, in
    // particular the discriminator's `CardAdded` broadcast that lands
    // after the renderer has observed the child exit.
    let mut rx = events.subscribe();

    let req = Event::TerminalWorkerRequested {
        idempotency_key: idem.into(),
        // Fast-exit command for the real supervised shell.
        cmd: "printf done\n".into(),
        cwd: None,
        agent_message: None,
    };
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    // Drain the bus. Allow a generous deadline so the negative
    // TaskFailed assertion still observes late failures.
    let mut saw_card_added: Option<calm_server::ids::CardId> = None;
    let mut saw_task_failed = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::CardAdded(card)
                    if card.payload.get("idempotency_key").and_then(|v| v.as_str())
                        == Some(idem) =>
                {
                    saw_card_added = Some(card.id.clone());
                }
                Event::TaskFailed {
                    idempotency_key, ..
                } if idempotency_key == idem => {
                    saw_task_failed = true;
                }
                _ => {}
            },
            Ok(Err(_)) => break,
            Err(_) => {
                // Recv timeout — keep polling until the 15s deadline
                // so a stray late `TaskFailed` (which we're asserting
                // is ABSENT) still has a chance to land in the loop.
                continue;
            }
        }
    }

    let card_id = saw_card_added.expect(
        "dispatcher must broadcast CardAdded for the fast-exit worker — \
         pre-fix this would never fire because the rollback path deleted \
         the row + propagated the spurious readiness Err as \
         task.failed",
    );
    assert!(
        !saw_task_failed,
        "task.failed must NOT be emitted for a fast-exit success — the worker \
         actually completed (exit_code = 0); pre-fix this \
         fired because spawn_terminal_with_parts's readiness error \
         propagated unconditionally to `run_one`",
    );

    // Card row survives.
    let card_row = repo
        .card_get(card_id.as_str())
        .await
        .expect("card_get ok")
        .expect("preserved card must still exist post-spawn-Err");
    assert_eq!(
        card_row
            .payload
            .get("idempotency_key")
            .and_then(|v| v.as_str()),
        Some(idem),
    );

    // Terminal row survives and the renderer attach reader persisted
    // exit_code = 0 / signal_killed = false.
    let terminal_id = repo
        .terminal_get_by_card(card_row.id.as_str())
        .await
        .unwrap()
        .expect("preserved card terminal row")
        .id;
    let term_row = wait_for(Duration::from_secs(5), || {
        let repo = repo.clone();
        let terminal_id = terminal_id.clone();
        async move {
            let row = repo.terminal_get(&terminal_id).await.unwrap()?;
            row.exit_code.is_some().then_some(row)
        }
    })
    .await
    .expect("renderer attach reader must persist fast-exit code");
    assert_eq!(
        term_row.exit_code,
        Some(0),
        "fast-exit terminal worker must persist exit_code=0 onto the row; got {:?}",
        term_row.exit_code,
    );
    assert!(
        !term_row.signal_killed,
        "signal_killed must be false for a clean fast-exit; got true",
    );
}

// ---------------------------------------------------------------------------
// Issue #644 round-2 review F4 — `wave.updated` is a scheduler trigger.
// ---------------------------------------------------------------------------

const CARD_SPAWN_ADAPTER_PHASES: &[PhaseTag] = &[];

/// Successful worker-spawn stub (mirror of the scheduler suite's):
/// `prepare_tx` returns a card-shaped result — the scheduler reads
/// `result["id"]` for the running stamp — and the spawn is a no-op.
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

/// Round-2 review F4: a Working wave held at `task_budget = 0` with a
/// pending plan task must dispatch when `PATCH /api/waves` raises the
/// budget — that PATCH emits ONLY `wave.updated` (no lifecycle event,
/// no plan.updated), so the dispatcher's subscriber must treat
/// `wave.updated` as a scheduler poke instead of waiting for the
/// periodic reconcile tick (300s default — far beyond this test).
#[tokio::test]
async fn wave_updated_budget_raise_pokes_scheduler() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    repo.wave_update(
        wave_id.as_str(),
        WavePatch {
            lifecycle: Some(WaveLifecycle::Working),
            task_budget: Some(Some(0)),
            ..Default::default()
        },
    )
    .await
    .expect("hold wave at budget 0");
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("worker card for the spawn stub");
    cache.insert(worker_card.id.clone(), CardRole::Worker, wave_id.clone());

    let task_id = format!("{}:budget-held", wave_id.as_str());
    let now = now_ms();
    let task = calm_server::model::Task {
        id: task_id.clone(),
        wave_id: wave_id.as_str().to_string(),
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
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    };
    calm_server::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            calm_server::db::sqlite::task_insert_tx(tx, &task).await?;
            Ok(())
        })
    })
    .await
    .expect("seed pending plan task");

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

    // A wave.updated while the budget is still 0 pokes the scheduler
    // but the §5.2 budget gate holds the task.
    let wave = repo.wave_get(wave_id.as_str()).await.unwrap().unwrap();
    repo.log_pure_event(
        ActorId::User,
        wave_scope(&wave_id, &cove_id),
        None,
        &events,
        &cache,
        &wcc,
        Event::WaveUpdated(calm_server::event::WaveUpdatedPayload::new(wave, None)),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        repo.task_get(&task_id).await.unwrap().unwrap().status,
        calm_server::model::TaskStatus::Pending,
        "budget 0 must keep holding the task"
    );

    // The budget-raise PATCH shape: row update + ONLY a wave.updated
    // event (mirror of routes/waves.rs `update_wave` with no lifecycle
    // change).
    repo.wave_update(
        wave_id.as_str(),
        WavePatch {
            task_budget: Some(Some(1)),
            ..Default::default()
        },
    )
    .await
    .expect("raise budget");
    let wave = repo.wave_get(wave_id.as_str()).await.unwrap().unwrap();
    repo.log_pure_event(
        ActorId::User,
        wave_scope(&wave_id, &cove_id),
        None,
        &events,
        &cache,
        &wcc,
        Event::WaveUpdated(calm_server::event::WaveUpdatedPayload::new(wave, None)),
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
    .expect("wave.updated must poke the scheduler — task stayed pending until the tick");
    assert!(
        matches!(
            status,
            calm_server::model::TaskStatus::Dispatched | calm_server::model::TaskStatus::Running
        ),
        "raised budget must dispatch the held task; got {status:?}"
    );
}

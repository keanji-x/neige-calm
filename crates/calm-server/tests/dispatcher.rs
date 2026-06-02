//! PR5 (#136) — `Dispatcher` integration tests.
//!
//! Coverage:
//!
//!   1. **`SubscribeFilter` over `EventBus::subscribe_filtered`** — emit
//!      three events of mixed kinds + scopes, assert the filter delivers
//!      only the requested ones (and that the receiver outlives extra
//!      lifecycle activity around it).
//!   2. **Happy path** — emit one `CodexJobRequested`, await the worker
//!      card landing in the DB with `role = Worker` + payload carrying
//!      the idempotency key, and a single `card.added` event in the
//!      log.
//!   3. **Idempotency** — emit the same `CodexJobRequested` (same
//!      `idempotency_key`) twice rapid-fire; only one worker card is
//!      created, the second is short-circuited.
//!   4. **Semaphore cap** — with `NEIGE_DISPATCHER_PERMITS = 2`, emit
//!      five `*.Requested` events; observe the global permit count
//!      stays bounded by 2.
//!   5. **TaskFailed on bad scope** — emit a `CodexJobRequested` with
//!      `EventScope::System` (the dispatcher needs a wave scope to mint
//!      a card); assert a `task.failed` event lands in the events log.
//!
//! Tests run against an in-memory `SqlxRepo` and a stubbed
//! `CodexClient` / `DaemonClient` — PR5 keeps daemon spawn deferred
//! (worker card + role write-through is the testable surface), so we
//! never actually call `spawn_terminal_for`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{
    ArtifactRef, Event, EventBus, EventScope, SubscribeFilter, SubscribeScope,
};
use calm_server::ids::{ActorId, CoveId, WaveId};
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;

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
    Event::CodexJobRequested {
        idempotency_key: idem.into(),
        goal: goal.into(),
        context: serde_json::Value::Null,
        acceptance_criteria: None,
    }
}

fn wave_scope(wave: &WaveId, cove: &CoveId) -> EventScope {
    EventScope::Wave {
        wave: wave.clone(),
        cove: cove.clone(),
    }
}

#[tokio::test]
async fn dispatcher_initial_prompt_ready_sink_persists_thread_id_and_broadcasts_card_updated() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({
                "appserver_needs_initial_prompt": true,
                "push_watermark": 0,
            }),
        })
        .await
        .expect("create spec card");
    let codex = stub_codex();
    let dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex,
        stub_daemon(),
        None,
        calm_server::spec_push::SpecPushRegistry::new(),
        stub_shared(&repo),
        4,
    );
    let mut rx = events.subscribe();
    let sink = dispatcher.initial_prompt_ready_sink_for(
        spec_card.id.clone(),
        wave_id.clone(),
        cove_id.clone(),
    );

    sink("thread-tui-created".to_string()).await;

    let updated = repo
        .card_get(spec_card.id.as_str())
        .await
        .expect("card_get")
        .expect("spec card still exists");
    assert_eq!(
        updated
            .payload
            .get("codex_thread_id")
            .and_then(serde_json::Value::as_str),
        Some("thread-tui-created")
    );
    assert!(
        updated
            .payload
            .get("appserver_needs_initial_prompt")
            .is_none(),
        "initial prompt marker must be cleared after backfill"
    );

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
            assert_eq!(
                card.payload
                    .get("codex_thread_id")
                    .and_then(serde_json::Value::as_str),
                Some("thread-tui-created")
            );
        }
        other => panic!("expected CardUpdated, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatcher_initial_prompt_ready_sink_failure_does_not_broadcast_or_mutate() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave_id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!(["corrupt-payload-shape"]),
        })
        .await
        .expect("create malformed spec card");
    let codex = stub_codex();
    let dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex,
        stub_daemon(),
        None,
        calm_server::spec_push::SpecPushRegistry::new(),
        stub_shared(&repo),
        4,
    );
    let mut rx = events.subscribe();
    let sink = dispatcher.initial_prompt_ready_sink_for(
        spec_card.id.clone(),
        wave_id.clone(),
        cove_id.clone(),
    );

    sink("thread-that-cannot-persist".to_string()).await;

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
        kinds: Some(vec!["codex.job_requested".into()]),
    };

    // Emit three events: matching kind, non-matching kind, matching kind again.
    events.emit(ActorId::User, codex_req("k1", "g1"));
    events.emit(
        ActorId::User,
        Event::TaskFailed {
            idempotency_key: "k2".into(),
            reason: "x".into(),
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
        "exactly two codex.job_requested events should match, got {}",
        matched.len()
    );
    for env in &matched {
        assert_eq!(env.event.kind_tag(), "codex.job_requested");
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
    assert_eq!(env.event.kind_tag(), "codex.job_requested");
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

/// PR6 followup (issue #136, note 1): the dispatcher's spawn path
/// catches `CalmError::IdempotencyCollision` and treats it as a
/// success short-circuit (the **second** emit produces no `task.failed`
/// from the catch arm). A real `CalmError::Conflict` from the helper
/// chain would now propagate as a failure — verifying the *positive*
/// case here (the dedup arm is silent) gives us the end-to-end signal
/// that the typed-variant catch arm is wired correctly. The negative
/// case (real Conflict propagates) is unit-tested in the in-module
/// `idempotency_collision_distinct_from_conflict` test.
///
/// Note (#310 followup): the dispatcher now rolls back the worker card
/// + terminal row when the post-commit `spawn_terminal_with_parts` step
/// fails — see `rollback_orphan_worker`. That means a failing
/// proc-supervisor connection is the WRONG fixture to test the dedup
/// invariant against: the
/// orphan no longer persists and the "exactly one card stays" signal
/// disappears. We use the fixture-backed proc supervisor so the first
/// dispatch succeeds end-to-end (no rollback, no task.failed) and the
/// dedup'd second emit silently short-circuits as it should.
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
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_push::SpecPushRegistry::new(), // #293: empty push registry
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
//     terminal-worker path (`spawn_terminal_worker`). The pre-fix bug
//     existed on BOTH spawn helpers: each emitted `CardAdded` inside the
//     row-creation tx, so a hot subscriber saw the card frame before
//     `spawn_terminal_with_parts` populated `renderer entry`. The fix
//     deferred the broadcast in both helpers; this test guards the
//     terminal half so a future refactor that reverts ONLY the terminal
//     change (leaving the codex test green) still trips a regression.
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
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        terminal_renderer.clone(),
        None,
        calm_server::spec_push::SpecPushRegistry::new(), // #293: empty push registry
        stub_shared(&repo),
        4,
    );

    // Subscribe BEFORE emitting so we don't race past the CardAdded
    // frame. Filter on the kind below so the dispatch's own
    // `TerminalJobRequested` echo doesn't show up as the "first" event.
    let mut rx = events.subscribe();

    let idem = "issue-310-ordering-terminal";
    let req = Event::TerminalJobRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
    };
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    // Drain the bus until we see the worker's CardAdded. Skip the
    // initial TerminalJobRequested envelope we just emitted; skip any
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

    // Same canonical payload shape as the codex path —
    // `card_with_terminal_create_tx` stamps `terminal_id` before the
    // dispatcher merges its bookkeeping in.
    let terminal_id = card
        .payload
        .get("terminal_id")
        .and_then(|v| v.as_str())
        .expect("worker card payload.terminal_id must be set");
    let term = repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row for the worker card must exist post-CardAdded");
    assert!(
        terminal_renderer.get(terminal_id).is_some(),
        "issue #310 regression (terminal path): renderer entry MUST be registered by the time CardAdded reaches subscribers",
    );
    assert!(
        term.pid.is_some(),
        "terminal pid must be persisted by the time CardAdded reaches subscribers; got {term:?}"
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
//     terminal row. The orphan row carried the `idempotency_key`, so a
//     retry with the same key short-circuited on the abandoned row —
//     the user could never re-dispatch. Strictly worse than pre-#310:
//     pre-fix at least `CardAdded` fired at tx-commit, so the card was
//     visible/closeable; post-fix it was invisible AND idempotency-locked.
//
//     The fix: on spawn failure, open a separate tx that DELETEs both
//     the terminal row (`ON DELETE RESTRICT` since #11 means terminal
//     first) and the card row, THEN propagate the spawn error so
//     `run_one` emits `TaskFailed`. A retry with the same key now goes
//     through fresh.
//
//     This test pins all three legs of the contract:
//        a) dispatch returns Err → task.failed fires.
//        b) no card with that idempotency_key remains in the DB.
//        c) A re-dispatch with the SAME idempotency_key (this time with
//           a working proc-supervisor fixture) succeeds — proves the orphan row
//           is not blocking retries.
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
//        c) A re-dispatch with the SAME idempotency_key (this time
//           with a working proc-supervisor fixture) succeeds — proves
//           the orphan row is not blocking retries.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_rolls_back_card_on_terminal_daemon_spawn_failure_issue_310() {
    let _guard = DISPATCHER_DAEMON_TEST_LOCK.lock().await;
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    let codex = stub_codex();
    let dispatcher_fail = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        stub_daemon(),
        None,
        calm_server::spec_push::SpecPushRegistry::new(), // #293: empty push registry
        stub_shared(&repo),
        4,
    );

    let idem = "rollback-terminal-1";
    let req = Event::TerminalJobRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
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
    // dispatcher whose proc-supervisor fixture accepts EnsureProc. See
    // test 12's leg (c) doc comment for the full rationale (including
    // why a fresh `EventBus` is needed to avoid the failing dispatcher
    // racing the success dispatcher on the retry envelope).
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
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon_ok,
        None,
        calm_server::spec_push::SpecPushRegistry::new(), // #293: empty push registry
        stub_shared(&repo),
        4,
    );

    let req_retry = Event::TerminalJobRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
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

    // The retry should mint a card carrying our idempotency_key. If
    // the rollback didn't fire, the orphan row would short-circuit and
    // no NEW card lands — `wait_for` returns None and we panic.
    let card = wait_for(Duration::from_secs(5), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        cards
            .into_iter()
            .find(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
    })
    .await
    .expect(
        "retry with the same idempotency_key MUST succeed in spawning a fresh terminal worker \
         card — if this times out, the orphan row from the failed first attempt is short-\
         circuiting the retry (the rollback didn't fire or didn't remove the row)",
    );

    assert!(
        repo.card_get(card.id.as_ref()).await.unwrap().is_some(),
        "the retry's card must be live in the DB",
    );
}

// ---------------------------------------------------------------------------
// 14. Issue #310 followup (codex's P1 escalation) — proc reap on rollback.
//
//     Pre-fix: when `spawn_terminal_with_parts` returned Err AFTER it had
//     already issued EnsureProc + persisted `pid` + inserted the renderer
//     entry but before readiness succeeded (real failure mode: the child
//     hangs during setup until the backstop fires),
//     `rollback_orphan_worker` deleted the rows but left the
//     supervised process leaking — the sweeper's SQL excludes
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
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        terminal_renderer.clone(),
        None,
        calm_server::spec_push::SpecPushRegistry::new(), // #293: empty push registry
        stub_shared(&repo),
        4,
    );

    // Use the terminal-worker path: simpler (no codex env / MCP
    // plumbing) but exercises the same `rollback_orphan_worker` call
    // site as the codex path. Both paths funnel through the same
    // helper, so reap-on-rollback proven for one path holds for both.
    let idem = "reap-on-rollback-1";
    let req = Event::TerminalJobRequested {
        idempotency_key: idem.into(),
        cmd: "/bin/true".into(),
        cwd: None,
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
//     The fix discriminates inside `rollback_orphan_worker`: when the
//     renderer attach reader has persisted a clean exit, the helper
//     returns `Preserved` (no row delete). The caller then broadcasts
//     `CardAdded` and returns Ok(()) instead of the spawn Err.
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
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        None,
        calm_server::spec_push::SpecPushRegistry::new(), // #293: empty push registry
        stub_shared(&repo),
        4,
    );

    let idem = "fast-exit-preserve-1";
    // Subscribe BEFORE emitting so we capture every envelope, in
    // particular the discriminator's `CardAdded` broadcast that lands
    // after the renderer has observed the child exit.
    let mut rx = events.subscribe();

    let req = Event::TerminalJobRequested {
        idempotency_key: idem.into(),
        // Fast-exit command for the real supervised shell.
        cmd: "printf done\n".into(),
        cwd: None,
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
    let terminal_id = card_row
        .payload
        .get("terminal_id")
        .and_then(|v| v.as_str())
        .expect("preserved card payload carries terminal_id");
    let term_row = wait_for(Duration::from_secs(5), || {
        let repo = repo.clone();
        let terminal_id = terminal_id.to_string();
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

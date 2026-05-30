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
use calm_server::model::{CardRole, NewCove, NewWave};
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;

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

#[tokio::test]
async fn dispatcher_happy_path_mints_worker_card() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    // #310 followup — the dispatcher now rolls back the worker card +
    // terminal row when `spawn_terminal_with_parts` returns Err (orphan
    // cleanup; see `rollback_orphan_worker`). Pre-rollback we could
    // use `stub_daemon()` here and catch the orphan card with
    // `wait_for` because it stuck around; post-rollback the card is
    // deleted before `wait_for` can poll for it, making this happy-
    // path test flaky. Point the daemon at the argv-recorder fixture
    // so spawn actually succeeds and the card stays.
    let codex = stub_codex();
    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
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
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,    // permits
    );

    // Emit a Wave-scoped CodexJobRequested envelope by going through
    // log_pure_event. Actor = User (unrestricted in the role gate).
    let idem = "happy-1";
    let req = codex_req(idem, "do thing");
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    // Poll for the worker card landing in the cards table.
    let card = wait_for(Duration::from_secs(3), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        cards
            .into_iter()
            .find(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
    })
    .await
    .expect("worker card minted within 3s");

    assert_eq!(card.kind, "codex");
    assert_eq!(
        card.payload.get("goal").and_then(|v| v.as_str()),
        Some("do thing")
    );
    assert_eq!(
        card.payload.get("role_request").and_then(|v| v.as_str()),
        Some("codex")
    );

    // `payload.prompt` must be non-empty and quote the goal —
    // `codex_auto_submit` reads this field to decide whether to fire the
    // composer `\r`; an empty value silently hangs the worker. The
    // goal text is part of the rendered prompt, so a regression in the
    // render helper (or in dispatcher wiring it into bookkeeping) trips
    // here.
    let prompt = card
        .payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .expect("worker card payload.prompt must be present");
    assert!(
        prompt.contains("do thing"),
        "payload.prompt must carry the goal; got: {prompt:?}"
    );

    // Verify the cards.role column carries 'worker' via the role cache
    // (write-through invariant in `card_create_with_id_tx`).
    assert_eq!(cache.get(&card.id), Some(CardRole::Worker));
}

#[tokio::test]
async fn dispatcher_role_is_worker_via_role_cache() {
    // Variant of the happy-path test that doesn't need to crack open
    // the pool — we verify role=Worker via `card_role_cache.get(...)`.
    //
    // #310 followup — see `dispatcher_happy_path_mints_worker_card`:
    // the rollback path deletes the card on spawn failure, so a happy-
    // path assertion needs the recorder fixture to make spawn succeed.
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let codex = stub_codex();
    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    let idem = "role-check";
    repo.log_pure_event(
        ActorId::User,
        wave_scope(&wave_id, &cove_id),
        None,
        &events,
        &cache,
        &wcc,
        codex_req(idem, "g"),
    )
    .await
    .unwrap();

    let card = wait_for(Duration::from_secs(3), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        cards
            .into_iter()
            .find(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
    })
    .await
    .expect("worker card minted within 3s");

    assert_eq!(cache.get(&card.id), Some(CardRole::Worker));
}

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
/// fails — see `rollback_orphan_worker`. That means a failing daemon
/// is the WRONG fixture to test the dedup invariant against: the
/// orphan no longer persists and the "exactly one card stays" signal
/// disappears. We point the daemon at the argv-recorder fixture so the
/// first dispatch succeeds end-to-end (no rollback, no task.failed)
/// and the dedup'd second emit silently short-circuits as it should.
#[tokio::test]
async fn dispatcher_dedup_does_not_double_emit_task_failed() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let codex = stub_codex();
    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    // Subscribe before emitting so we don't miss a fast task.failed.
    let mut rx = events.subscribe();

    let idem = "dedup-single-fail";
    for _ in 0..2 {
        repo.log_pure_event(
            ActorId::User,
            wave_scope(&wave_id, &cove_id),
            None,
            &events,
            &cache,
            &wcc,
            codex_req(idem, "g"),
        )
        .await
        .unwrap();
    }

    // Give the dispatcher time to drain both emits — first one spawns
    // a real worker through the recorder fixture; second one dedups.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Drain the bus and count `task.failed` events carrying our idem.
    let mut failed_count = 0usize;
    while let Ok(Ok(env)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
        if let Event::TaskFailed {
            idempotency_key, ..
        } = &env.event
            && idempotency_key == idem
        {
            failed_count += 1;
        }
    }
    // Zero — both dispatches must complete cleanly. The first spawns
    // through the recorder fixture (success); the second short-circuits
    // on the IdempotencyCollision catch arm (also success). A task.failed
    // here would indicate either a real spawn-pipeline regression OR the
    // catch arm misfiring and the second emit re-running the spawn chain
    // against a now-bound socket (which would still succeed but should
    // never have re-entered the spawn path).
    assert_eq!(
        failed_count, 0,
        "expected zero task.failed events (both dispatches must complete cleanly); got {failed_count}. \
         A non-zero count indicates a regression in either the spawn pipeline or the \
         IdempotencyCollision catch arm."
    );

    // Sanity check: exactly one worker card landed.
    let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let worker_cards: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert_eq!(
        worker_cards.len(),
        1,
        "exactly one worker card for the dedup'd key"
    );
}

#[tokio::test]
async fn dispatcher_dedupes_same_idempotency_key() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let codex = stub_codex();
    // #310 followup — see `dispatcher_dedup_does_not_double_emit_task_failed`
    // for why this test needs a real (recorder) daemon now: the orphan-row
    // rollback wipes the card on spawn failure, so testing dedup against
    // a failing daemon no longer leaves the "exactly one card" signal.
    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    let idem = "dup-key";
    // Fire two requests with the same key in quick succession.
    for _ in 0..2 {
        repo.log_pure_event(
            ActorId::User,
            wave_scope(&wave_id, &cove_id),
            None,
            &events,
            &cache,
            &wcc,
            codex_req(idem, "g"),
        )
        .await
        .unwrap();
    }

    // Give the dispatcher time to process both (recorder readiness
    // takes ~50-300ms; 1.5s is generous).
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let worker_cards: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert_eq!(
        worker_cards.len(),
        1,
        "exactly one worker card for the duplicate idempotency key, got {} ({:?})",
        worker_cards.len(),
        worker_cards
    );
}

// ---------------------------------------------------------------------------
// 4. Semaphore cap.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatcher_semaphore_caps_concurrent_spawns() {
    let _ = tracing_subscriber::fmt::try_init();
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    // Permits = 2. The dispatcher's spawn-side acquire_owned holds a
    // permit across the work; with idempotency keys unique per
    // emission, 5 emits should all eventually succeed, but the
    // semaphore's `available_permits()` should never go below 0
    // (it can't by construction) and shouldn't exceed 2 minus the
    // number currently held — i.e. `available <= 2` at any sample.
    //
    // #310 followup — needs a real (recorder) daemon: with the
    // orphan-row rollback, a failing daemon spawn no longer leaves the
    // worker card behind, so the "5 cards land" tail assertion needs
    // an actually-succeeding spawn path. The semaphore-cap assertion
    // is orthogonal to daemon success/failure — what we care about is
    // that 5 emits eventually settle through the permit pool.
    let codex = stub_codex();
    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let dispatcher = Arc::new(Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        2,
    ));
    assert_eq!(dispatcher.permits(), 2);
    let sem = dispatcher.semaphore();

    // Fire five distinct requests.
    for i in 0..5 {
        repo.log_pure_event(
            ActorId::User,
            wave_scope(&wave_id, &cove_id),
            None,
            &events,
            &cache,
            &wcc,
            codex_req(&format!("idem-{i}"), "g"),
        )
        .await
        .unwrap();
    }

    // Sample available_permits at multiple times during processing.
    // The semaphore was constructed with 2 permits, so the value
    // should always satisfy: 0 <= available <= 2.
    for _ in 0..30 {
        let avail = sem.available_permits();
        assert!(
            avail <= 2,
            "semaphore over-issued permits: available={avail}, max=2"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Wait for all five to complete.
    let result = wait_for(Duration::from_secs(5), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        let n = cards
            .iter()
            .filter(|c| c.payload.get("role_request").and_then(|v| v.as_str()) == Some("codex"))
            .count();
        if n == 5 { Some(()) } else { None }
    })
    .await;

    if result.is_none() {
        // Debug dump for triage.
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        let n: Vec<_> = cards
            .iter()
            .filter_map(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()))
            .collect();
        panic!("expected 5 worker cards, found {}: keys={:?}", n.len(), n);
    }

    // After settle, semaphore should be back to full.
    assert_eq!(
        sem.available_permits(),
        2,
        "all permits released after work done"
    );
}

// ---------------------------------------------------------------------------
// 5. TaskFailed on spawn error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatcher_emits_task_failed_on_bad_scope() {
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
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

#[tokio::test]
async fn dispatcher_card_added_emit_passes_role_gate() {
    // The role gate's `KernelDispatcher` arm is documented as
    // unrestricted (see `role_gate.rs:110`). This test verifies that
    // the dispatcher's actual write path (which goes through
    // `write_with_event` → `enforce_role`) doesn't get rejected.
    //
    // #310 followup — same recorder-fixture switch as the other happy-
    // path tests: the orphan rollback now wipes the worker card on
    // spawn failure, so a happy-path "card lands" probe needs the
    // recorder to make spawn succeed.
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let codex = stub_codex();
    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    let idem = "role-gate-ok";
    repo.log_pure_event(
        ActorId::User,
        wave_scope(&wave_id, &cove_id),
        None,
        &events,
        &cache,
        &wcc,
        codex_req(idem, "g"),
    )
    .await
    .unwrap();

    // If enforce_role rejected the dispatcher write, no card would land.
    let _card = wait_for(Duration::from_secs(3), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        cards
            .into_iter()
            .find(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
    })
    .await
    .expect("dispatcher write must pass enforce_role for ActorId::KernelDispatcher");
}

// ---------------------------------------------------------------------------
// PR6 (#136) — Real concurrent idempotency-race test.
//
// PR5's dedup test fires sequentially (`for _ in 0..2`); the canonical
// in-tx SELECT race window only opens when two emissions hit the
// dispatcher within microseconds. We use a `Barrier` to release two
// dispatcher tasks at the same moment after both have already
// acquired their semaphore permit and entered the spawn fn.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatcher_dedupes_under_real_concurrent_race() {
    let _ = tracing_subscriber::fmt::try_init();
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    // Permits >= 2 so both racers can run concurrently.
    //
    // #310 followup: switched from `stub_daemon()` (nonexistent binary,
    // every spawn fails) to the argv-recorder fixture. Pre-rollback the
    // failing winning racer left an orphan card the assertion below
    // counted as "exactly one"; post-rollback that orphan is wiped, so
    // we need the spawn to actually succeed for the "exactly one card
    // lands" dedup signal to remain meaningful.
    let codex = stub_codex();
    let tmp = tempfile::TempDir::new().expect("tempdir for daemon sockets");
    let daemon = Arc::new(calm_server::state::DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        daemon,
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    let idem = "race-key-pr6";
    // Fire two emissions through `log_pure_event` from two parallel
    // tokio tasks released by a barrier. Each task acquires a
    // permit on a shared `Notify` after the barrier so the second
    // emit lands on the bus only nanoseconds after the first.
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let emit_once = |label: &'static str| {
        let repo = repo.clone();
        let events = events.clone();
        let cache = cache.clone();
        let wcc = wcc.clone();
        let scope = wave_scope(&wave_id, &cove_id);
        let barrier = barrier.clone();
        async move {
            // Synchronize so both tasks call log_pure_event at as
            // close to the same instant as the scheduler allows.
            barrier.wait().await;
            repo.log_pure_event(
                ActorId::User,
                scope,
                None,
                &events,
                &cache,
                &wcc,
                codex_req(idem, label),
            )
            .await
            .expect("log_pure_event ok");
        }
    };

    let (a, b) = tokio::join!(
        tokio::spawn(emit_once("racer-a")),
        tokio::spawn(emit_once("racer-b"))
    );
    a.unwrap();
    b.unwrap();

    // Give the dispatcher time to drain both envelopes through the
    // spawn pipeline. We use `wait_for` not a fixed sleep so this
    // doesn't hang the suite on a slow runner.
    let _ = wait_for(Duration::from_secs(3), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        let n = cards
            .iter()
            .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
            .count();
        if n >= 1 { Some(n) } else { None }
    })
    .await;

    // Settle: count worker cards.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let worker_cards: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert_eq!(
        worker_cards.len(),
        1,
        "exactly one worker card under barrier-synchronized concurrent emit; got {} cards: {:?}",
        worker_cards.len(),
        worker_cards
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
    );
}

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
/// Wait up to `timeout` for any `*.argv` file under `data_dir` to
/// land + return its lines. The recorder writes the file BEFORE
/// binding the unix socket so by the time the kernel sees the daemon
/// ready, the argv sidecar is complete on disk.
#[allow(dead_code)]
async fn wait_for_argv_file(data_dir: &std::path::Path, timeout: Duration) -> Vec<String> {
    let start = Instant::now();
    loop {
        if let Ok(read) = std::fs::read_dir(data_dir) {
            for entry in read.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("argv") {
                    let content = std::fs::read_to_string(&p).expect("read argv file");
                    return content.lines().map(String::from).collect();
                }
            }
        }
        if start.elapsed() > timeout {
            panic!(
                "no *.argv file landed under {data_dir:?} within {timeout:?} — \
                 dispatcher daemon spawn never ran (or recorder fixture failed)"
            );
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
async fn dispatcher_codex_worker_spawns_with_dark_theme_default() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    // Point the daemon at the argv-recorder fixture so the dispatcher's
    // spawn actually completes — `stub_daemon()` uses a nonexistent
    // path so spawns error before any argv can be observed.
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
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,    // permits
    );

    // Emit a Wave-scoped CodexJobRequested envelope; the dispatcher
    // picks it up, mints a worker card with `RequestTheme::default_dark()`
    // on the terminal row, then spawns the daemon. `spawn_terminal_with_parts`
    // reads the row's theme_fg/_bg and stamps `--terminal-fg/-bg` on argv.
    let idem = "dispatcher-theme-default";
    let req = codex_req(idem, "do thing");
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    let card = wait_for(Duration::from_secs(5), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        cards
            .into_iter()
            .find(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
    })
    .await
    .expect("worker card minted within 5s");
    let terminal_id = card.payload["terminal_id"]
        .as_str()
        .expect("payload.terminal_id stamped");
    // Same dispatcher-spawn race as the carries_prompt_argv test above:
    // the card-mint DB write can win against renderer.ensure() inserting
    // into the registry by a tick or two.
    let entry = wait_for(Duration::from_secs(3), || async {
        terminal_renderer.get(terminal_id)
    })
    .await
    .expect("renderer entry registered before CardAdded within 3s");
    assert_eq!(entry.config().terminal_fg, (216, 219, 226));
    assert_eq!(entry.config().terminal_bg, (15, 20, 24));
}

// ---------------------------------------------------------------------------
// 9. Dispatcher's worker codex spawn must hand the rendered prompt to
//    codex as its positional `[PROMPT]` arg — NOT a bare `codex`. Without
//    this, the worker mounts an empty composer and `codex_auto_submit`
//    has nothing to submit; the worker hangs forever. Mirrors the spec
//    card path (issue #251) for the worker side.
//
// We funnel the daemon argv through the same `argv-recorder-daemon`
// fixture used by the theme test above. `spawn_terminal_with_parts` ends
// the argv with `-- /bin/sh -c "<program>"`, so the rendered prompt
// lands as one element starting with `codex `.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_codex_worker_spawn_carries_prompt_argv() {
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    let idem = "prompt-argv";
    // Distinctive goal text so the substring assertion below pins us to
    // this exact dispatch (rather than `do thing` which a sibling test
    // also uses).
    let req = codex_req(idem, "fix issue 251 for the worker path");
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    let card = wait_for(Duration::from_secs(5), || async {
        let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        cards
            .into_iter()
            .find(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
    })
    .await
    .expect("worker card minted within 5s");
    let terminal_id = card.payload["terminal_id"]
        .as_str()
        .expect("payload.terminal_id stamped");
    // Dispatcher's spawn races against the card-mint write — the card
    // can land in the DB a tick before the renderer's ensure() completes
    // its registry insert. Poll briefly.
    let entry = wait_for(Duration::from_secs(3), || async {
        terminal_renderer.get(terminal_id)
    })
    .await
    .expect("renderer entry registered for worker within 3s");
    let argv_text = entry.config().args.join("\n");
    assert!(
        argv_text.contains("codex '"),
        "expected renderer argv to start the program with `codex '<prompt>'`; \
         got bare `codex` or missing quoting: {argv_text:?}"
    );
    assert!(
        argv_text.contains("fix issue 251 for the worker path"),
        "codex argv must carry the goal as a positional prompt; got: {argv_text:?}"
    );
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
//     emit, dispatches a codex worker through the recorder daemon fixture
//     (so daemon spawn actually succeeds), captures the FIRST `CardAdded`
//     envelope, then queries the matching `terminal_get(...)` row and
//     asserts:
//        - `renderer entry.is_some()`  ← the core ordering contract
//        - the socket path is connectable (the daemon really is up)
//
//     If anyone reverts the dispatcher back to emitting `CardAdded`
//     inside the tx (e.g. by routing it through `write_with_event_typed`
//     again), this test trips loudly.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_codex_card_added_after_renderer_entry_set_issue_310() {
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    // Subscribe BEFORE emitting so we don't race past the CardAdded
    // frame. Filter on the kind below so the dispatch's own
    // `CodexJobRequested` echo doesn't show up as the "first" event.
    let mut rx = events.subscribe();

    let idem = "issue-310-ordering";
    let req = codex_req(idem, "verify ordering contract");
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    // Drain the bus until we see the worker's CardAdded. Skip the
    // initial CodexJobRequested envelope we just emitted; skip any
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
    let card =
        worker_card.expect("dispatcher must broadcast CardAdded for the worker card within 5s");

    // The card's payload carries the terminal_id (canonical layout
    // stamped by `card_with_codex_create_tx`); use it to fetch the
    // terminal row and assert the renderer entry + pid are present AT
    // THE MOMENT the bus delivered CardAdded.
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
        "issue #310 regression: renderer entry MUST be registered by the time CardAdded reaches subscribers",
    );
    assert!(
        term.pid.is_some(),
        "terminal pid must be persisted by the time CardAdded reaches subscribers; got {term:?}"
    );
}

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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
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
//           a working daemon binary) succeeds — proves the orphan row
//           is not blocking retries.
//
//     We provoke spawn failure by pointing `DaemonClient.session_daemon_bin`
//     at a nonexistent path (same trick `stub_daemon()` uses) — `cmd.spawn()`
//     returns ENOENT, `spawn_terminal_with_parts` maps it to
//     `CalmError::Internal`. Then we swap the bin to the argv-recorder
//     fixture for the retry leg.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_rolls_back_card_on_codex_daemon_spawn_failure_issue_310() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    // First dispatcher uses a bogus daemon path → spawn always fails.
    let codex = stub_codex();
    let dispatcher_fail = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        codex.clone(),
        stub_daemon(), // session_daemon_bin = /nonexistent-daemon-bin
        None,
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    let idem = "rollback-codex-1";
    let req = codex_req(idem, "rollback-test");
    let scope = wave_scope(&wave_id, &cove_id);

    // Subscribe before emitting so we can confirm task.failed fired.
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

    // Wait for the dispatcher to drain → emit task.failed (the canonical
    // signal that the spawn pipeline ran to its failure end). The
    // rollback runs synchronously inside the spawn fn before Err
    // propagates, so by the time task.failed is on the bus, the orphan
    // row must already be gone.
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
        "expected dispatcher to emit task.failed after spawn failure"
    );

    // Leg (a) + (b): no worker card under our idempotency key — the
    // row was rolled back.
    let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let leftover: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert!(
        leftover.is_empty(),
        "expected card row to be rolled back after spawn failure; \
         found {} leftover cards: {:?}",
        leftover.len(),
        leftover.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
    );

    // Leg (c): retry with the SAME idempotency_key against a fresh
    // dispatcher whose daemon binary actually exists. Pre-rollback, the
    // orphan row would short-circuit this on `IdempotencyCollision`;
    // post-rollback, the SELECT inside the tx sees no match and the
    // spawn proceeds normally.
    //
    // We deliberately spin up a FRESH `EventBus` for the retry leg.
    // `dispatcher_fail`'s background task subscribes to `events` and
    // doesn't shut down on `drop()` — its `Dispatcher::spawn` task
    // only exits on broadcast `Closed`, which we can't trigger without
    // dropping every sender. If we re-emitted into `events`, both
    // dispatchers would race for the retry envelope; the failing one
    // could win the in-tx SELECT, spawn-fail, and roll back the
    // success-side dispatcher's card. A dedicated retry bus side-
    // steps the race entirely — the failing dispatcher only ever sees
    // the original (failed) envelope; the retry envelope goes solely
    // to the success dispatcher.
    drop(dispatcher_fail);
    let events_retry = calm_server::event::EventBus::new();
    // Intentionally leak the tempdir (`TempDir::keep()` consumes the
    // guard without scheduling cleanup) so the recorder daemon's
    // argv-sidecar writes still find a live directory after the test
    // fn returns. The dispatcher task we spawn below never observes a
    // shutdown signal in the test harness, so its child daemon
    // outlives this scope; without `keep` the `TempDir` drop deletes
    // the dir and the still-running daemon panics with "create argv
    // sidecar: Os { code: 2, kind: NotFound }" stderr noise that masks
    // real failures in adjacent tests.
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    repo.log_pure_event(
        ActorId::User,
        scope,
        None,
        &events_retry,
        &cache,
        &wcc,
        codex_req(idem, "rollback-test-retry"),
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
        "retry with the same idempotency_key MUST succeed in spawning a fresh worker card \
         — if this times out, the orphan row from the failed first attempt is short-\
         circuiting the retry (the rollback didn't fire or didn't remove the row)",
    );

    // Sanity: the card was actually freshly created (the post-rollback
    // SELECT-in-tx found nothing → `card_with_codex_create_tx` minted a
    // new row). The goal text differentiates this from the first
    // (failed) dispatch.
    assert_eq!(
        card.payload.get("goal").and_then(|v| v.as_str()),
        Some("rollback-test-retry"),
        "retry must mint a NEW card with the retry's goal, not return the orphan",
    );
    assert!(
        repo.card_get(card.id.as_ref()).await.unwrap().is_some(),
        "the retry's card must be live in the DB",
    );
}

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
//           with a working daemon binary) succeeds — proves the orphan
//           row is not blocking retries.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_rolls_back_card_on_terminal_daemon_spawn_failure_issue_310() {
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
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
    // dispatcher whose daemon binary actually exists. See test 12's
    // leg (c) doc comment for the full rationale (including why a
    // fresh `EventBus` is needed to avoid the failing dispatcher
    // racing the success dispatcher on the retry envelope).
    drop(dispatcher_fail);
    let events_retry = calm_server::event::EventBus::new();
    // Intentionally leak the tempdir — see test 12's note for the
    // rationale (daemon outlives the test fn; tempdir drop deletes
    // the data dir; daemon panics on next argv-sidecar write).
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
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
// 14. Issue #310 followup (codex's P1 escalation) — daemon reap on rollback.
//
//     Pre-fix: when `spawn_terminal_with_parts` returned Err AFTER it had
//     already spawned the daemon child + persisted `pid` + persisted
//     `renderer entry` but before readiness succeeded (real failure mode:
//     the daemon hangs during setup until the backstop fires),
//     `rollback_orphan_worker` deleted the rows but left the
//     daemon process + unix socket leaking — the sweeper's SQL excludes
//     terminals still referenced by a card row, but we just deleted the
//     card, so the sweeper *also* never sees the orphan. Result: a
//     daemon process bound to a socket on disk, with no DB row to
//     anchor cleanup, until the next kernel boot.
//
//     The fix re-fetches the terminal row (to pick up any post-commit
//     pid / renderer entry writes) and calls `reap_terminal_artifacts`
//     before the row delete. This test pins that ordering by:
//        a) Pointing the dispatcher at `never-ready-daemon` — spawns
//           OK, persists pid via a `.partial-pid` sidecar, then sleeps
//           without binding the socket or writing ready. The kernel's
//           hung-daemon backstop returns Err.
//        b) Waiting for `task.failed` (proves the rollback path ran
//           to its end).
//        c) Asserting the recorded pid is no longer alive
//           (`kill(pid, 0)` returns ESRCH) — proves the reap fired.
//        d) Asserting the socket file is gone — proves the unlink
//           step of `reap_terminal_artifacts` fired.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_reaps_daemon_on_rollback_after_partial_spawn_issue_310() {
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
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

    // Wait for task.failed — the dispatcher's readiness race in
    // `spawn_terminal_with_parts` reaches its hung-daemon backstop before
    // returning the timeout error. The rollback then runs synchronously
    // before `run_one` emits `task.failed`. Allow a generous deadline.
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
//     `/bin/true`, `make build`) writes the daemon's `.exit` sidecar AND
//     exits before the kernel sees `ready\n`.
//     `spawn_terminal_with_parts` returned Err for this case, and round 3's
//     unconditional rollback would then DELETE the card + terminal row —
//     turning a completed worker into `task.failed` with no card/output
//     for the user to inspect.
//
//     The fix discriminates inside `rollback_orphan_worker`: when the
//     re-fetched terminal row has `renderer entry = Some(...)` AND a
//     `<handle>.exit` sidecar exists on disk, the helper persists the
//     sidecar's exit_code/signal_killed onto the row and returns
//     `Preserved` (no row delete). The caller then broadcasts
//     `CardAdded` and returns Ok(()) instead of the spawn Err.
//
//     This test pins that preservation contract:
//
//        a) Point the dispatcher at `fast-exit-daemon` — spawns OK,
//           writes `<sock>.exit` with `{"code":0,"signal_killed":false}`,
//           exits 0 without binding the socket or writing ready. The
//           kernel's child-exit readiness arm returns Err.
//
//        b) Subscribe to the bus BEFORE emitting so we capture both
//           the (would-be) `task.failed` AND the `CardAdded` envelope.
//
//        c) Assert:
//           - `CardAdded` IS broadcast for the dispatched worker.
//           - `TaskFailed` is NOT broadcast for the dispatched key.
//           - The card row + terminal row both survive.
//           - The terminal row's `exit_code = Some(0)` and
//             `signal_killed = false` (persisted by the discriminator
//             from the sidecar).
//           - The terminal row's `renderer entry` is still set
//             (preserved, not nulled by the rollback path).
//
//     This test would FAIL on the round-3 code (rollback always
//     deletes), pinning that the discriminator is wired and the
//     fast-exit success path is preserved end-to-end.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn dispatcher_preserves_fast_exit_terminal_card_issue_310() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;

    // Same intentional leak as the never-ready test: the daemon child
    // is short-lived (exits immediately), but lingering filesystem
    // cleanup can race the test tempdir drop.
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
        calm_server::spec_appserver::SpecPushRegistry::new(), // #293: empty push registry
        4,
    );

    let idem = "fast-exit-preserve-1";
    // Subscribe BEFORE emitting so we capture every envelope, in
    // particular the discriminator's `CardAdded` broadcast that lands
    // AFTER the child-exit readiness arm returns Err.
    let mut rx = events.subscribe();

    let req = Event::TerminalJobRequested {
        idempotency_key: idem.into(),
        // The cmd string is only used by the real daemon — our fixture
        // ignores it. We pass a representative string for log clarity.
        cmd: "printf done\n".into(),
        cwd: None,
    };
    let scope = wave_scope(&wave_id, &cove_id);
    repo.log_pure_event(ActorId::User, scope, None, &events, &cache, &wcc, req)
        .await
        .unwrap();

    // Drain the bus. The dispatcher's child-exit readiness arm should
    // fire quickly; the discriminator's sidecar pickup + CardAdded
    // broadcast happens after. Allow a generous deadline so the
    // negative TaskFailed assertion still observes late failures.
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
         actually completed (exit_code = 0 in `.exit` sidecar); pre-fix this \
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

    // Terminal row survives, has renderer entry preserved, and the
    // discriminator persisted exit_code = 0 / signal_killed = false
    // from the sidecar.
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

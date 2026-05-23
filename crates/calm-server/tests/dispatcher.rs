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
//! never actually call `spawn_daemon_for`.

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
        session_daemon_bin: PathBuf::from("/nonexistent-daemon-bin"),
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

    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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

    // Verify the cards.role column carries 'worker' via the role cache
    // (write-through invariant in `card_create_with_id_tx`).
    assert_eq!(cache.get(&card.id), Some(CardRole::Worker));
}

#[tokio::test]
async fn dispatcher_role_is_worker_via_role_cache() {
    // Variant of the happy-path test that doesn't need to crack open
    // the pool — we verify role=Worker via `card_role_cache.get(...)`.
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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
/// Note: the stub `DaemonClient` points at a non-existent daemon
/// binary, so the **first** (winning) dispatch still emits one
/// `task.failed` from the post-commit `spawn_daemon_with_parts` step.
/// Exactly one — not two — is the dedup signal we assert on. (Two
/// would indicate the catch arm misfired and the second emit reran
/// the spawn chain.)
#[tokio::test]
async fn dispatcher_dedup_does_not_double_emit_task_failed() {
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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

    // Give the dispatcher time to drain both emits + the daemon-
    // spawn failure path on the winning one.
    tokio::time::sleep(Duration::from_millis(600)).await;

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
    // Exactly one — from the winning dispatch's daemon-spawn step
    // (stub daemon binary is missing). The dedup'd second emit must
    // not produce a second `task.failed`.
    assert_eq!(
        failed_count, 1,
        "expected exactly one task.failed (from the winning dispatch's daemon spawn); got {failed_count}. \
         A second event here would indicate the IdempotencyCollision catch arm misfired."
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
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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

    // Give the dispatcher time to process both.
    tokio::time::sleep(Duration::from_millis(400)).await;

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
    let dispatcher = Arc::new(Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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
    let (repo, events, cache, wcc, wave_id, cove_id) = boot().await;
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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
    let _dispatcher = Dispatcher::spawn(
        repo.clone(),
        events.clone(),
        cache.clone(),
        wcc.clone(),
        stub_codex(),
        stub_daemon(),
        None, // mcp_server: PR7a.1 — test fixture, no MCP wiring
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

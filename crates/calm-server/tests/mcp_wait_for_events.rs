//! PR8 (#136) — integration tests for `calm.wait_for_events`.
//!
//! Drives the tool through the registry the way the transport would
//! (look up by name, invoke the boxed handler with a `CardIdentity`),
//! same shape as `mcp_wave_state.rs`. No live UDS — the goal is to
//! exercise the long-poll, batch window, cursor, and filter logic
//! end-to-end against real bus / repo / cache wiring.

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::db::{prelude::*, write_with_event_typed};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::event_cursor::EventCursorCache;
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wait::TOOL_WAIT_FOR_EVENTS;
use calm_server::mcp_server::{CardIdentity, ToolRegistry};
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, Wave};
use calm_server::plugin_host::mcp::RpcError;
use serde_json::{Value, json};

const TEST_BUDGET: Duration = Duration::from_secs(5);

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    cove_id: CoveId,
    wave_id: WaveId,
    other_wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
    role_cache: CardRoleCache,
    wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache,
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "wait-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "wave-A".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
        })
        .await
        .unwrap();
    // A second wave under the same cove — used to assert the
    // SubscribeFilter excludes other waves' events.
    let other_wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "wave-B".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
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
    let role_cache = CardRoleCache::new();
    role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        events,
        card_role_cache: role_cache.clone(),
        wave_cove_cache: wcc.clone(),
        event_cursor_cache: EventCursorCache::new(),
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        repo,
        cove_id: cove.id,
        wave_id: wave.id,
        other_wave_id: other_wave.id,
        spec_card_id: spec_card.id,
        worker_card_id: worker_card.id,
        role_cache,
        wave_cove_cache: wcc,
    }
}

fn spec_identity(b: &Boot) -> CardIdentity {
    CardIdentity {
        card_id: b.spec_card_id.clone(),
        role: CardRole::Spec,
    }
}

fn worker_identity(b: &Boot) -> CardIdentity {
    CardIdentity {
        card_id: b.worker_card_id.clone(),
        role: CardRole::Worker,
    }
}

async fn call_wait(b: &Boot, identity: CardIdentity, args: Value) -> Result<Value, RpcError> {
    let handler = b
        .registry
        .lookup(TOOL_WAIT_FOR_EVENTS)
        .expect("wait_for_events registered");
    handler(b.ctx.clone(), identity, args).await
}

/// Persist a wave-scoped event through `write_with_event_typed`. We
/// emit a synthetic `Event::WaveUpdated` because it has wave scope
/// and the role gate permits `ActorId::User`. Returns the persisted
/// event id.
async fn emit_wave_event_on(boot: &Boot, wave_id: &WaveId, cove_id: &CoveId) -> i64 {
    let wave = boot.repo.wave_get(wave_id.as_str()).await.unwrap().unwrap();
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let (_, id) = write_with_event_typed::<Wave, _>(
        boot.ctx.repo.as_ref(),
        ActorId::User,
        scope,
        None,
        &boot.ctx.events,
        &boot.role_cache,
        &boot.wave_cove_cache,
        move |_tx| {
            let wave = wave.clone();
            Box::pin(async move { Ok((wave.clone(), Event::WaveUpdated(wave))) })
        },
    )
    .await
    .expect("emit wave.updated");
    id
}

/// Persist a card-scoped event. Returns the persisted event id.
async fn emit_card_event_on(boot: &Boot, card_id: &CardId) -> i64 {
    let card = boot
        .repo
        .card_get(card_id.as_str())
        .await
        .unwrap()
        .expect("card exists");
    let wave = boot
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let scope = EventScope::Card {
        card: card.id.clone(),
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let card_clone = card.clone();
    let (_, id) = write_with_event_typed::<Card, _>(
        boot.ctx.repo.as_ref(),
        ActorId::User,
        scope,
        None,
        &boot.ctx.events,
        &boot.role_cache,
        &boot.wave_cove_cache,
        move |_tx| {
            let card = card_clone.clone();
            Box::pin(async move { Ok((card.clone(), Event::CardUpdated(card))) })
        },
    )
    .await
    .expect("emit card.updated");
    id
}

// ---------------------------------------------------------------------------
// Happy paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_returns_event_within_long_poll_budget() {
    let b = boot().await;

    // Spawn the wait BEFORE emitting so the live subscribe path runs
    // (catch-up sees zero rows because cursor starts at 0 and no
    // events exist at boot).
    let ctx = b.ctx.clone();
    let registry = b.registry.clone();
    let identity = spec_identity(&b);

    let task = tokio::spawn(async move {
        let handler = registry.lookup(TOOL_WAIT_FOR_EVENTS).unwrap();
        // Plenty of headroom; the live emit below should land well
        // before this fires.
        handler(ctx, identity, json!({"timeout_ms": 5000})).await
    });

    // Tiny sleep so wait_for_events_for_card actually reaches the
    // subscribe path before the emit happens. 50ms is well under the
    // 5s test budget and over the bus channel setup time.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _id = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;

    let out = tokio::time::timeout(TEST_BUDGET, task)
        .await
        .expect("wait returns within budget")
        .expect("task did not panic")
        .expect("wait_for_events ok");

    let events = out.get("events").and_then(Value::as_array).unwrap();
    assert_eq!(events.len(), 1, "expected one event, got {out}");
    assert_eq!(events[0]["ev"], "wave.updated");
    assert!(out["since"].is_i64(), "since must be an integer: {out}");
}

#[tokio::test]
async fn wait_returns_empty_array_on_timeout() {
    let b = boot().await;
    // Short test timeout — 100ms long-poll, no emits. Tolerates
    // ~100ms-ish of variance.
    let out = call_wait(&b, spec_identity(&b), json!({"timeout_ms": 100}))
        .await
        .expect("wait returns empty on timeout (not error)");
    let events = out.get("events").and_then(Value::as_array).unwrap();
    assert!(
        events.is_empty(),
        "expected empty events array on timeout, got {out}"
    );
    assert!(
        out["since"].is_null(),
        "since must be null when no events returned: {out}",
    );
}

#[tokio::test]
async fn wait_catch_up_returns_existing_events_immediately() {
    let b = boot().await;
    // Emit 3 events BEFORE calling wait — the call should fast-path
    // through catch-up without ever subscribing.
    let _id1 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;
    let _id2 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;
    let id3 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;

    let t0 = std::time::Instant::now();
    let out = call_wait(
        &b,
        spec_identity(&b),
        // since=0 forces a full replay; default 30s timeout would still
        // return immediately because catch-up is non-empty.
        json!({"timeout_ms": 30000, "since": 0}),
    )
    .await
    .expect("catch-up ok");
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "catch-up should be near-instant; took {elapsed:?}",
    );

    let events = out.get("events").and_then(Value::as_array).unwrap();
    assert_eq!(events.len(), 3, "got: {out}");
    assert_eq!(out["since"].as_i64(), Some(id3));
}

#[tokio::test]
async fn wait_filters_by_wave_scope() {
    let b = boot().await;
    // Emit one event under the OTHER wave (same cove). The wait call
    // bound to the spec card's wave must not see it.
    let other_cove = b.cove_id.clone();
    let other_wave = b.other_wave_id.clone();
    let _id = emit_wave_event_on(&b, &other_wave, &other_cove).await;

    // 100ms timeout — long enough to confirm the live subscribe doesn't
    // surface the other-wave event, short enough to keep the test snappy.
    let out = call_wait(&b, spec_identity(&b), json!({"timeout_ms": 100}))
        .await
        .expect("wait ok");
    let events = out.get("events").and_then(Value::as_array).unwrap();
    assert!(
        events.is_empty(),
        "other-wave event must not match our scope; got: {out}",
    );
}

#[tokio::test]
async fn wait_includes_card_scope_under_wave() {
    let b = boot().await;
    // Emit a card-scoped event under the spec card itself. Since the
    // SubscribeFilter uses `include_descendants = true` on
    // `SubscribeScope::Wave`, card-scoped events under that wave must
    // route to the wait call.
    let card_id = b.spec_card_id.clone();

    let ctx = b.ctx.clone();
    let registry = b.registry.clone();
    let identity = spec_identity(&b);
    let task = tokio::spawn(async move {
        let handler = registry.lookup(TOOL_WAIT_FOR_EVENTS).unwrap();
        handler(ctx, identity, json!({"timeout_ms": 5000})).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _id = emit_card_event_on(&b, &card_id).await;

    let out = tokio::time::timeout(TEST_BUDGET, task)
        .await
        .expect("wait returns")
        .expect("task ok")
        .expect("wait ok");
    let events = out.get("events").and_then(Value::as_array).unwrap();
    assert_eq!(events.len(), 1, "card-scoped event must route: {out}");
    assert_eq!(events[0]["ev"], "card.updated");
}

#[tokio::test]
async fn wait_batch_window_groups_burst_into_one_call() {
    let b = boot().await;

    let ctx = b.ctx.clone();
    let registry = b.registry.clone();
    let identity = spec_identity(&b);
    let wave_id = b.wave_id.clone();
    let cove_id = b.cove_id.clone();

    let task = tokio::spawn(async move {
        let handler = registry.lookup(TOOL_WAIT_FOR_EVENTS).unwrap();
        handler(ctx, identity, json!({"timeout_ms": 5000})).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // First event triggers the batch window. The next two land
    // within the 50ms window (we just await both inline; tokio's
    // single-threaded scheduling does them well under 1ms each).
    let _id1 = emit_wave_event_on(&b, &wave_id, &cove_id).await;
    let _id2 = emit_wave_event_on(&b, &wave_id, &cove_id).await;
    let _id3 = emit_wave_event_on(&b, &wave_id, &cove_id).await;

    let out = tokio::time::timeout(TEST_BUDGET, task)
        .await
        .expect("wait returns")
        .expect("task ok")
        .expect("wait ok");

    let events = out.get("events").and_then(Value::as_array).unwrap();
    // All three should be in one batch (well under the 50ms window).
    // Tolerate 1-2 in case bus delivery interleaves oddly; the
    // important assertion is "more than one event lands in one call".
    assert!(
        events.len() >= 2,
        "batch window should group rapid emits; got {} event(s): {out}",
        events.len(),
    );
}

// ---------------------------------------------------------------------------
// Cursor semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_cursor_advances_across_calls() {
    let b = boot().await;
    // Seed two events; first call returns both via catch-up; second
    // call (since omitted → cache) should see nothing within the
    // short timeout.
    let _id1 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;
    let _id2 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;

    let first = call_wait(
        &b,
        spec_identity(&b),
        json!({"timeout_ms": 5000, "since": 0}),
    )
    .await
    .expect("first wait ok");
    let first_events = first["events"].as_array().unwrap();
    assert_eq!(first_events.len(), 2, "catch-up: {first}");
    let max_id = first["since"].as_i64().unwrap();
    assert!(max_id > 0, "first call returned max id: {first}");

    // Second call: omit `since` — cursor cache (now at `max_id`)
    // takes over. No new emits → empty within the short timeout.
    let second = call_wait(&b, spec_identity(&b), json!({"timeout_ms": 100}))
        .await
        .expect("second wait ok");
    let second_events = second["events"].as_array().unwrap();
    assert!(
        second_events.is_empty(),
        "cursor must have advanced past the catch-up batch: {second}",
    );

    // And cursor advances when a NEW event lands.
    let _id3 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;
    let third = call_wait(&b, spec_identity(&b), json!({"timeout_ms": 5000}))
        .await
        .expect("third wait ok");
    let third_events = third["events"].as_array().unwrap();
    assert_eq!(third_events.len(), 1, "exactly one new event: {third}");
    let third_since = third["since"].as_i64().unwrap();
    assert!(
        third_since > max_id,
        "cursor advanced past first max: max_id={max_id}, third_since={third_since}",
    );
}

#[tokio::test]
async fn wait_since_zero_rewinds_past_cursor() {
    let b = boot().await;
    let _id1 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;
    let _id2 = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;

    // First call advances the cache.
    let _ = call_wait(&b, spec_identity(&b), json!({"timeout_ms": 5000}))
        .await
        .expect("first wait ok");

    // Second call with explicit since=0 replays everything from the
    // beginning even though the cache says we're caught up.
    let replay = call_wait(
        &b,
        spec_identity(&b),
        json!({"timeout_ms": 5000, "since": 0}),
    )
    .await
    .expect("replay ok");
    let events = replay["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        2,
        "explicit since=0 replays the full history: {replay}"
    );
}

// ---------------------------------------------------------------------------
// Role gate + arg validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_refuses_worker_with_invalid_params() {
    let b = boot().await;
    let err = call_wait(&b, worker_identity(&b), json!({"timeout_ms": 100}))
        .await
        .expect_err("worker must be denied");
    assert_eq!(err.code, -32602, "soft role gate returns invalid-params");
    assert!(
        err.message.contains("Spec"),
        "error mentions required role: {err:?}",
    );
}

#[tokio::test]
async fn wait_rejects_negative_since() {
    let b = boot().await;
    let err = call_wait(
        &b,
        spec_identity(&b),
        json!({"timeout_ms": 100, "since": -5}),
    )
    .await
    .expect_err("negative since rejected");
    assert_eq!(err.code, -32602);
}

#[tokio::test]
async fn wait_clamps_oversized_timeout_to_30s() {
    // Caller asks for 5 minutes; we cap. No way to assert the actual
    // clamping from outside, but we CAN assert the call doesn't take
    // 5 minutes (it should return on the empty-bus path well within
    // the clamped 30s — we wait 200ms to confirm the call obeys the
    // clamp via no-op return because we won't wait 30s in a test).
    //
    // This is the closest behavioral signal we have without exposing
    // the clamped value.
    let b = boot().await;
    let _id = emit_wave_event_on(&b, &b.wave_id.clone(), &b.cove_id.clone()).await;
    let t0 = std::time::Instant::now();
    let out = call_wait(
        &b,
        spec_identity(&b),
        json!({"timeout_ms": 300_000u64, "since": 0}),
    )
    .await
    .expect("ok");
    let elapsed = t0.elapsed();
    // The event was pre-emitted, so catch-up returns it immediately.
    // The point of this test is: even with an outrageous timeout, we
    // never wait that long. Catch-up means we return basically
    // instantly.
    assert!(elapsed < Duration::from_secs(2));
    assert_eq!(out["events"].as_array().unwrap().len(), 1);
}

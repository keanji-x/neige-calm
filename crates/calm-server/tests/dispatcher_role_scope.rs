//! Cross-layer role-gate + scope coverage for the wave-as-actor dispatcher
//! pathway (issue #199, acceptance #2).
//!
//! Where existing tests sit:
//!
//!   * `role_enforcement.rs` exercises the role gate from
//!     `write_with_event_typed` / `log_pure_event` directly, but never
//!     touches the actor header path or the Worker scope semantics
//!     end-to-end.
//!   * `wave_as_actor_smoke.rs` boots real axum + SqlxRepo + role cache
//!     and runs the happy path (Spec card emits CodexJobRequested → worker
//!     mint), but the *deny* paths are unexercised.
//!
//! This file fills the gap with focused assertions on the cross-layer
//! invariants that production relies on:
//!
//!   1. A Worker-roled card attempting to emit a `Wave`-scoped event is
//!      refused by the role gate before the event row lands.
//!   2. A Worker emitting a Card-scoped event in its *own* card scope
//!      succeeds (positive control for the gate's section-3 logic).
//!   3. A Worker emitting into another card's scope (cross-card, even
//!      within the same wave) is refused — the gate is per-card-id strict.
//!   4. The `actor_middleware` defaults to `ActorId::User` when no
//!      `X-Calm-Actor` header is set; this is the "older bridges /
//!      anonymous callers" contract documented on `Actor::DEFAULT`.
//!   5. A Worker emitting a Card-scoped event with a card from a
//!      DIFFERENT wave is refused — the gate's scope match is `scope.card
//!      == self`, so the wave context doesn't matter from the gate's
//!      perspective, but documenting the (lack of) wave-level
//!      cross-check matters for future hardening (see "Surprises" in the
//!      PR body).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::actor::{Actor, actor_middleware};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

async fn boot_repo() -> (Arc<SqlxRepo>, EventBus, CardRoleCache, WaveCoveCache) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let bus = EventBus::new();
    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    let wcc = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();
    (repo, bus, cache, wcc)
}

/// Seed a cove + wave + Worker-roled card. The worker's role lands in
/// both the cards row (so a future cache-reseed picks it up) and the
/// in-memory cache (so the gate sees it now). The wave's cove also
/// lands in the supplied `wcc` so the gate's #234 cove check passes
/// for the home wave.
async fn seed_worker_in_wave(
    repo: &SqlxRepo,
    cache: &CardRoleCache,
    wcc: &WaveCoveCache,
    cove_name: &str,
    wave_title: &str,
) -> (CoveId, WaveId, CardId) {
    let cove = repo
        .cove_create(NewCove {
            name: cove_name.into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: wave_title.into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = 'worker' WHERE id = ?1")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(
        card.id.clone(),
        CardRole::Worker,
        WaveId::from(wave.id.as_str()),
    );
    // #234 — bind the wave's cove into the cache the gate consults, so
    // the cove cross-check has a populated entry for the worker's home
    // wave.
    wcc.insert(
        WaveId::from(wave.id.as_str()),
        CoveId::from(cove.id.as_str()),
    );
    (
        CoveId::from(cove.id.as_str()),
        WaveId::from(wave.id.as_str()),
        CardId::from(card.id.as_str()),
    )
}

fn task_completed(idem: &str) -> Event {
    Event::TaskCompleted {
        idempotency_key: idem.into(),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
    }
}

async fn count_events(repo: &SqlxRepo, kind: &str) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    row.0
}

// ---------------------------------------------------------------------------
// Test 1 — Worker emitting Wave-scoped event is rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_emitting_wave_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (cove, wave, worker) = seed_worker_in_wave(&repo, &cache, &wcc, "c", "w").await;
    let mut sub = bus.subscribe();

    let baseline_total = count_events(&repo, "task.completed").await;

    let scope = EventScope::Wave {
        wave: wave.clone(),
        cove: cove.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-wave-1"),
        )
        .await;

    // The gate's section-3 check fires: a Worker actor with a Wave scope
    // doesn't match `scope.card == self`, so the write is refused.
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope")
        ),
        "Worker emitting wave scope must be refused: {res:?}",
    );

    // Event row count is unchanged — the transaction rolled back.
    let after = count_events(&repo, "task.completed").await;
    assert_eq!(
        after, baseline_total,
        "rejected worker write must not append an event row",
    );

    // Bus subscription saw nothing — broadcast-after-commit invariant.
    assert!(sub.try_recv().is_err(), "rejected write must not broadcast",);
}

// ---------------------------------------------------------------------------
// Test 2 — Worker emitting Card scope in its OWN card succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_emitting_own_card_scope_is_accepted() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (cove, wave, worker) = seed_worker_in_wave(&repo, &cache, &wcc, "c", "w").await;
    let mut sub = bus.subscribe();

    let scope = EventScope::Card {
        card: worker.clone(),
        wave: wave.clone(),
        cove: cove.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-own-1"),
        )
        .await;
    assert!(
        res.is_ok(),
        "Worker emitting its own card scope must succeed: {res:?}",
    );

    let env = sub.try_recv().expect("envelope on bus");
    assert!(matches!(env.event, Event::TaskCompleted { .. }));
    assert!(matches!(
        env.actor,
        ActorId::AiCodex(ref c) if c == &worker,
    ));
}

// ---------------------------------------------------------------------------
// Test 3 — Worker emitting Card scope of ANOTHER card is rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_emitting_other_card_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (cove, wave, worker_a) = seed_worker_in_wave(&repo, &cache, &wcc, "c", "w").await;

    // A second card in the same wave — also Worker-roled to ensure the
    // refusal hinges on the *scope.card != actor.card* mismatch, not on a
    // role lookup failure for the other id.
    let card_b = repo
        .card_create(NewCard {
            wave_id: wave.as_str().into(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = 'worker' WHERE id = ?1")
        .bind(card_b.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(card_b.id.clone(), CardRole::Worker, wave.clone());

    let scope = EventScope::Card {
        card: CardId::from(card_b.id.as_str()),
        wave: wave.clone(),
        cove: cove.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker_a.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-cross-card"),
        )
        .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope")
        ),
        "Worker A emitting into Worker B's scope must be refused: {res:?}",
    );
}

// ---------------------------------------------------------------------------
// Test 4 — missing X-Calm-Actor defaults to "user"
// ---------------------------------------------------------------------------
//
// The actor middleware's documented contract: a request with no
// `X-Calm-Actor` header lands as `Actor("user")` (constant
// `Actor::DEFAULT`), which `to_actor_id()` resolves to `ActorId::User`.
// We exercise this via a probe route that surfaces the actor it sees;
// going through the real middleware (instead of constructing an `Actor`
// by hand) catches regressions in the wiring layer specifically.

#[tokio::test]
async fn missing_actor_header_defaults_to_user() {
    use axum::Router;
    use axum::extract::Extension;
    use axum::routing::get;

    async fn probe(Extension(actor): Extension<Actor>) -> String {
        actor.as_str().to_string()
    }

    let app = Router::new()
        .route("/probe", get(probe))
        .layer(axum::middleware::from_fn(actor_middleware));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/probe")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        Actor::DEFAULT,
        "missing X-Calm-Actor must default to `user` — the contract older bridges rely on",
    );

    // Empty-string header (some clients send `X-Calm-Actor: ` with no
    // value) collapses to the same default.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/probe")
                .header(Actor::HEADER, "")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), Actor::DEFAULT);
}

// ---------------------------------------------------------------------------
// Test 5 — cross-wave: Worker in Wave A emitting into Wave B
// ---------------------------------------------------------------------------
//
// Issue #232 (PR #232 closed): the role gate now cross-checks
// `scope.wave == cache.wave_of(card)` for Worker actors, mirroring the
// existing per-card-id check. A Worker in Wave A that forges
// `scope.wave = B` (even with the correct `scope.card`) is refused
// before the event row lands.

#[tokio::test]
async fn worker_with_mismatched_wave_in_card_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (cove_a, _wave_a, worker_a) =
        seed_worker_in_wave(&repo, &cache, &wcc, "cove-a", "wave-a").await;
    let (_cove_b, wave_b, _worker_b) =
        seed_worker_in_wave(&repo, &cache, &wcc, "cove-b", "wave-b").await;

    let baseline_total = count_events(&repo, "task.completed").await;
    let mut sub = bus.subscribe();

    // Forge an `EventScope::Card` whose `card` is Worker A's id but
    // whose `wave` is Wave B. Pre-#232 this was accepted because the
    // gate only compared `scope.card == actor.card`; #232 closes the
    // foot-gun by also checking `scope.wave == cache.wave_of(card)`.
    let scope = EventScope::Card {
        card: worker_a.clone(),
        wave: wave_b.clone(),
        cove: cove_a.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker_a.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-a-into-wave-b"),
        )
        .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope") && msg.contains("scope.wave mismatch")
        ),
        "Worker A forging scope.wave = Wave B must be refused (#232): {res:?}",
    );

    // Event row count is unchanged — the transaction rolled back, and
    // no row for the forged idempotency key landed.
    let after = count_events(&repo, "task.completed").await;
    assert_eq!(
        after, baseline_total,
        "rejected worker write must not append an event row",
    );
    let forged_row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT scope_wave FROM events \
         WHERE kind = 'task.completed' \
           AND json_extract(payload, '$.idempotency_key') = 'worker-a-into-wave-b'",
    )
    .fetch_optional(repo.pool())
    .await
    .unwrap();
    assert!(
        forged_row.is_none(),
        "no event row should exist for the forged scope.wave: {forged_row:?}",
    );

    // Bus subscription saw nothing — broadcast-after-commit invariant
    // mirrors the other rejection tests above.
    assert!(sub.try_recv().is_err(), "rejected write must not broadcast");
}

// ---------------------------------------------------------------------------
// Test 5b — cross-cove: Worker in Cove A emitting into Cove B
// ---------------------------------------------------------------------------
//
// Issue #234 (same shape as #232 one level up): the role gate now also
// cross-checks `scope.cove == wave_cove_cache.cove_of(home_wave)` for
// Worker actors, so a Worker with the right `scope.card` + `scope.wave`
// but a forged `scope.cove` is refused before the event row lands. This
// closes the last fan-out spoof axis — pre-#234 the row would still
// carry a fake `cove_id` and any client filtering on cove would see
// the event.

#[tokio::test]
async fn worker_with_mismatched_cove_in_card_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (_cove_a, wave_a, worker_a) =
        seed_worker_in_wave(&repo, &cache, &wcc, "cove-a", "wave-a").await;
    let (cove_b, _wave_b, _worker_b) =
        seed_worker_in_wave(&repo, &cache, &wcc, "cove-b", "wave-b").await;

    let baseline_total = count_events(&repo, "task.completed").await;
    let mut sub = bus.subscribe();

    // Forge an `EventScope::Card` whose `card` is Worker A's id and
    // whose `wave` is Wave A (matches), but whose `cove` is Cove B.
    // Pre-#234 the gate only matched card + wave; #234 closes the gap
    // by also matching cove.
    let scope = EventScope::Card {
        card: worker_a.clone(),
        wave: wave_a.clone(),
        cove: cove_b.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker_a.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-a-into-cove-b"),
        )
        .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope") && msg.contains("scope.cove mismatch")
        ),
        "Worker A forging scope.cove = Cove B must be refused (#234): {res:?}",
    );

    // Event row count is unchanged — the transaction rolled back.
    let after = count_events(&repo, "task.completed").await;
    assert_eq!(
        after, baseline_total,
        "rejected worker write must not append an event row",
    );
    let forged_row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT scope_cove FROM events \
         WHERE kind = 'task.completed' \
           AND json_extract(payload, '$.idempotency_key') = 'worker-a-into-cove-b'",
    )
    .fetch_optional(repo.pool())
    .await
    .unwrap();
    assert!(
        forged_row.is_none(),
        "no event row should exist for the forged scope.cove: {forged_row:?}",
    );

    // Bus subscription saw nothing — broadcast-after-commit invariant.
    assert!(sub.try_recv().is_err(), "rejected write must not broadcast");
}

// ---------------------------------------------------------------------------
// Test 6 — positive control: Spec card emits Wave-scoped event
// ---------------------------------------------------------------------------
//
// Mirrors the rejection test above to confirm we haven't broken the
// happy path. The smoke test in `wave_as_actor_smoke.rs` does the same
// at the dispatcher level; this one runs through `log_pure_event`
// directly so a regression in just the gate's WaveUpdated branch
// (vs the dispatcher harness) fails here too.

#[tokio::test]
async fn spec_emitting_wave_scope_is_accepted() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = 'spec' WHERE id = ?1")
        .bind(spec.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(
        spec.id.clone(),
        CardRole::Spec,
        WaveId::from(wave.id.as_str()),
    );

    let scope = EventScope::Wave {
        wave: WaveId::from(wave.id.as_str()),
        cove: CoveId::from(cove.id.as_str()),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiSpec(CardId::from(spec.id.as_str())),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            Event::CodexJobRequested {
                idempotency_key: "spec-pos-1".into(),
                goal: "go".into(),
                context: Value::Null,
                acceptance_criteria: None,
            },
        )
        .await;
    assert!(
        res.is_ok(),
        "Spec card emitting Wave-scoped CodexJobRequested must be accepted: {res:?}",
    );
}

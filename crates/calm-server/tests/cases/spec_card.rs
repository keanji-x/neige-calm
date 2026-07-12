//! PR6 (#136) — atomic spec card binding on wave create.
//!
//! Coverage:
//!   * `POST /api/waves` atomically mints a single `CardRole::Spec`
//!     codex card under the wave.
//!   * Two events emit in order: `Event::WaveUpdated` (wave-scoped),
//!     then `Event::CardAdded` (card-scoped). No spurious
//!     `card.updated`.
//!   * The card_role_cache carries `Spec` for the auto-minted card.
//!   * `enforce_role` permits the spec card to emit `WaveUpdated`
//!     (via direct CardRoleCache lookup + `enforce_role` call).
//!   * With a broken shared codex daemon, wave create still returns
//!     201 and commits an inert spec card with no terminal row.
//!
//! Strategy mirrors `tests/codex_card_endpoint.rs`: build a real Axum
//! router with `AppState::from_parts`, hit it with `tower::ServiceExt`,
//! and assert on the persisted state + the event broadcast stream.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, WaveId};
use calm_server::model::{CardRole, NewCove};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::role_gate::enforce_role;
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    cove_id: String,
    events: EventBus,
    repo: Arc<dyn Repo>,
    card_role_cache: CardRoleCache,
    _tmp: TempDir,
}

/// Boot a router pointing at a non-existent codex bin. The shared daemon
/// start fails, but `POST /api/waves` still commits the wave/spec/report
/// rows and returns 201 with an inert spec card.
async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "spec-card-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-spec-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        {
            // Deterministically-broken codex bin (absolute, absent) so the
            // spec-push app-server boot fails fast regardless of PATH. Wave
            // create tolerates this (#293 / PR #311) and returns 201; the
            // commit-time events still broadcast before the boot attempt,
            // which is what this test asserts.
            let mut codex = CodexClient::new_stub();
            codex.codex_bin = "/nonexistent-codex-bin-spec-card-test".into();
            Arc::new(codex)
        },
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        cove_id: cove.id.to_string(),
        events,
        repo,
        card_role_cache,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Drain at least `n` envelopes from a broadcast subscriber, with a
/// short deadline. Returns the collected envelopes (or panics if the
/// timeout elapses).
async fn collect_envelopes(events: &EventBus, n: usize) -> Vec<BroadcastEnvelope> {
    let mut rx = events.subscribe_filtered();
    // The caller subscribes *before* triggering the emit; here we
    // pump until we have n.
    let mut out = Vec::with_capacity(n);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while out.len() < n {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "expected {n} envelopes within deadline; got {} so far: {:?}",
                out.len(),
                out
            );
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => out.push(env),
            Ok(Err(e)) => panic!("broadcast recv error: {e:?}"),
            Err(_) => continue,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Spec card binding.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_api_waves_mints_spec_card_atomically() {
    let boot = boot().await;

    // Subscribe before firing so we catch both envelopes the route
    // produces (commit-then-emit invariant). The daemon spawn will
    // fail (binary doesn't exist) — that errors *after* the events
    // already broadcast, so the test still sees them.
    // Issue #229 PR B — wave create now emits four envelopes in one
    // tx: `WaveUpdated`, `CardAdded(spec)`, `CardAdded(report)`,
    // `OverlaySet(layout)`. Order in the bus matches the order the
    // closure pushes them. The two CardAdded envelopes are
    // distinguishable by `card.kind` ("codex" vs "wave-report").
    let subscription = {
        let events = boot.events.clone();
        tokio::spawn(async move { collect_envelopes(&events, 4).await })
    };
    // Tiny pause so the subscribe-before-emit ordering is reliable.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "first wave", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    // Issue #293 / PR #311: the spec-push app-server boot is non-fatal.
    // With a broken codex bin the boot fails, but the route still returns
    // 201 (inert wave). The persisted rows (wave + spec card + terminal)
    // and the events that emitted at commit-time — which is what this test
    // asserts — survive regardless of the boot outcome.
    assert_eq!(
        status,
        StatusCode::CREATED,
        "broken codex bin → 201 (boot is non-fatal, #293/#311); persisted rows + events still survive; body={body}",
    );

    // Drain the envelope subscription with a generous deadline.
    let envelopes = tokio::time::timeout(Duration::from_secs(3), subscription)
        .await
        .expect("collector finished")
        .expect("collector task ok");

    // First envelope: WaveUpdated, wave-scoped, actor=User.
    assert!(
        matches!(&envelopes[0].event, Event::WaveUpdated(_)),
        "first envelope must be WaveUpdated; got: {:?}",
        envelopes[0].event,
    );
    assert!(
        matches!(&envelopes[0].scope, EventScope::Wave { .. }),
        "WaveUpdated must be wave-scoped; got: {:?}",
        envelopes[0].scope,
    );
    assert_eq!(envelopes[0].actor, ActorId::User);

    // Second envelope: CardAdded (spec), card-scoped, actor=User.
    assert!(
        matches!(&envelopes[1].event, Event::CardAdded(_)),
        "second envelope must be CardAdded(spec); got: {:?}",
        envelopes[1].event,
    );
    assert!(
        matches!(&envelopes[1].scope, EventScope::Card { .. }),
        "CardAdded must be card-scoped; got: {:?}",
        envelopes[1].scope,
    );
    assert_eq!(envelopes[1].actor, ActorId::User);

    // Third envelope: CardAdded (wave-report — PR B), card-scoped.
    assert!(
        matches!(&envelopes[2].event, Event::CardAdded(_)),
        "third envelope must be CardAdded(wave-report); got: {:?}",
        envelopes[2].event,
    );
    let spec_card_id = match &envelopes[1].event {
        Event::CardAdded(c) => {
            assert_eq!(c.kind, "codex", "second envelope is the spec card");
            c.id.clone()
        }
        _ => unreachable!(),
    };
    match &envelopes[2].event {
        Event::CardAdded(c) => {
            assert_eq!(
                c.kind, "wave-report",
                "third envelope is the wave-report card"
            );
            assert!(!c.deletable, "wave-report card is kernel-owned");
        }
        _ => unreachable!(),
    }

    // Fourth envelope: OverlaySet(layout) — kernel-seeded layout
    // overlay positioning the wave-report card at the top of the grid.
    assert!(
        matches!(&envelopes[3].event, Event::OverlaySet(_)),
        "fourth envelope must be OverlaySet(layout); got: {:?}",
        envelopes[3].event,
    );

    // Cache write-through invariant: CardRole::Spec is visible.
    assert_eq!(
        boot.card_role_cache.get(&spec_card_id),
        Some(CardRole::Spec),
        "spec card's role must be Spec in the cache",
    );

    // DB invariants: spec + wave-report cards under the wave, kind=codex
    // for the spec, and no terminal row for the inert spec card.
    let wave_id = match &envelopes[0].event {
        Event::WaveUpdated(w) => w.id.clone(),
        _ => unreachable!(),
    };
    let cards = boot.repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    assert_eq!(cards.len(), 2, "spec + wave-report card per wave at create",);
    let spec_in_db = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("spec card in db");
    assert_eq!(spec_in_db.id, spec_card_id);
    assert!(
        cards.iter().any(|c| c.kind == "wave-report"),
        "wave-report card in db",
    );
    let term = boot
        .repo
        .terminal_get_by_card(spec_card_id.as_str())
        .await
        .unwrap();
    assert!(
        term.is_none(),
        "inert spec card should not have a terminal row"
    );
}

#[tokio::test]
async fn spec_card_can_emit_wave_updated_via_enforce_role() {
    // The spec card minted by `POST /api/waves` must satisfy
    // `enforce_role`'s `WaveUpdated`-from-AiSpec rule. We don't
    // actually go through the route here — we mint the card directly
    // via the cache + call the gate to lock in the contract.
    let cache = CardRoleCache::new();
    let spec_id = CardId::from("spec-card-pr6");
    cache.insert(spec_id.clone(), CardRole::Spec, WaveId::from("w"));

    // A WaveUpdated event from AiSpec(spec_id) under Wave scope.
    let evt = Event::WaveUpdated(calm_server::event::WaveUpdatedPayload::new(
        calm_server::model::Wave {
            id: "w".into(),
            cove_id: "c".into(),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: calm_server::model::WaveLifecycle::Draft,
            cwd: String::new(),
            workflow_id: None,
            purpose: None,
            workflow_input: None,
            terminal_at: None,
            created_at: 0,
            updated_at: 0,
        },
        None,
    ));
    let scope = EventScope::Wave {
        wave: "w".into(),
        cove: "c".into(),
    };
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    let res = enforce_role(
        &ActorId::AiSpec(spec_id.clone()),
        &evt,
        &scope,
        &cache,
        &wcc,
    );
    assert!(
        res.is_ok(),
        "AiSpec(spec-card) must be permitted to emit WaveUpdated: {res:?}",
    );
}

// ---------------------------------------------------------------------------
// `write_with_events_typed` plural helper coverage.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_with_events_typed_persists_and_broadcasts_multiple_in_order() {
    use calm_server::db::sqlite::{cove_create_tx, wave_create_tx};
    use calm_server::db::write_with_events_typed;
    use calm_server::model::{NewCove, NewWave};

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();

    let mut rx = events.subscribe_filtered();

    // The closure emits two distinct events: CoveUpdated under
    // EventScope::Cove, and WaveUpdated under EventScope::Wave.
    let event_ids: Vec<i64> = write_with_events_typed(
        repo.as_ref(),
        ActorId::User,
        None,
        &events,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        |tx| {
            Box::pin(async move {
                let cove = cove_create_tx(
                    tx,
                    NewCove {
                        name: "plural".into(),
                        color: "#fff".into(),
                        sort: None,
                    },
                )
                .await?;
                let wave = wave_create_tx(
                    tx,
                    NewWave {
                        workflow_input: None,
                        cove_id: cove.id.clone(),
                        title: "plural-wave".into(),
                        sort: None,
                        cwd: String::new(),
                        workflow_id: None,
                        attach_folder: false,
                        theme: calm_server::routes::theme::RequestTheme::default_dark(),
                    },
                    &calm_server::wave_cove_cache::WaveCoveCache::new(),
                )
                .await?;
                let cove_scope = EventScope::Cove {
                    cove: cove.id.clone(),
                };
                let wave_scope = EventScope::Wave {
                    wave: wave.id.clone(),
                    cove: cove.id.clone(),
                };
                Ok((
                    (),
                    vec![
                        (cove_scope, Event::CoveUpdated(cove)),
                        (
                            wave_scope,
                            Event::WaveUpdated(calm_server::event::WaveUpdatedPayload::new(
                                wave, None,
                            )),
                        ),
                    ],
                ))
            })
        },
    )
    .await
    .expect("plural write succeeds")
    .1;

    assert_eq!(event_ids.len(), 2, "two event ids returned");
    assert!(
        event_ids[1] > event_ids[0],
        "event ids monotonically increasing: {event_ids:?}",
    );

    // Both broadcasts hit the subscription, in declared order.
    let env1 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("first envelope arrives")
        .unwrap();
    let env2 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("second envelope arrives")
        .unwrap();
    assert!(matches!(env1.event, Event::CoveUpdated(_)));
    assert!(matches!(env1.scope, EventScope::Cove { .. }));
    assert!(matches!(env2.event, Event::WaveUpdated(_)));
    assert!(matches!(env2.scope, EventScope::Wave { .. }));
}

#[tokio::test]
async fn write_with_events_typed_rolls_back_when_closure_errors() {
    use calm_server::db::sqlite::cove_create_tx;
    use calm_server::db::write_with_events_typed;
    use calm_server::error::CalmError;
    use calm_server::model::NewCove;

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();

    // Pre-check: no coves exist.
    assert!(repo.coves_list().await.unwrap().is_empty());

    // Closure writes a cove then explodes — the cove row must vanish
    // and no event must broadcast.
    let mut rx = events.subscribe_filtered();
    let res = write_with_events_typed::<(), _>(
        repo.as_ref(),
        ActorId::User,
        None,
        &events,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        |tx| {
            Box::pin(async move {
                let _cove = cove_create_tx(
                    tx,
                    NewCove {
                        name: "doomed".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                Err(CalmError::Internal("closure aborts after writing".into()))
            })
        },
    )
    .await;

    assert!(res.is_err(), "closure error must bubble");
    assert!(
        repo.coves_list().await.unwrap().is_empty(),
        "cove row must be rolled back",
    );
    // No envelope should be in flight.
    assert!(
        rx.try_recv().is_err(),
        "rolled-back tx must not broadcast any envelope",
    );
}

#[tokio::test]
async fn write_with_events_typed_rolls_back_on_enforce_role_violation() {
    use calm_server::db::sqlite::{cove_create_tx, wave_create_tx};
    use calm_server::db::write_with_events_typed;
    use calm_server::model::{NewCove, NewWave};

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let cache = CardRoleCache::new();

    // Actor is `AiCodex(known-worker)` — *cannot* emit WaveUpdated
    // per enforce_role. Closure returns two events; the second is
    // the WaveUpdated that will trip the gate. Everything must
    // roll back.
    let worker_id = CardId::from("worker-card-id");
    cache.insert(
        worker_id.clone(),
        CardRole::Worker,
        WaveId::from("worker-wave"),
    );
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();

    assert!(repo.coves_list().await.unwrap().is_empty());

    let mut rx = events.subscribe_filtered();
    let wcc_for_tx = wcc.clone();
    let res = write_with_events_typed::<(), _>(
        repo.as_ref(),
        ActorId::AiCodex(worker_id),
        None,
        &events,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        move |tx| {
            Box::pin(async move {
                let cove = cove_create_tx(
                    tx,
                    NewCove {
                        name: "gated".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                let wave = wave_create_tx(
                    tx,
                    NewWave {
                        workflow_input: None,
                        cove_id: cove.id.clone(),
                        title: "gated-wave".into(),
                        sort: None,
                        cwd: String::new(),
                        workflow_id: None,
                        attach_folder: false,
                        theme: calm_server::routes::theme::RequestTheme::default_dark(),
                    },
                    &wcc_for_tx,
                )
                .await?;
                let cove_scope = EventScope::Cove {
                    cove: cove.id.clone(),
                };
                let wave_scope = EventScope::Wave {
                    wave: wave.id.clone(),
                    cove: cove.id.clone(),
                };
                Ok((
                    (),
                    vec![
                        // First event passes the gate (CoveUpdated +
                        // Cove scope — section 2 of enforce_role only
                        // gates WaveUpdated). Second one violates.
                        (cove_scope, Event::CoveUpdated(cove)),
                        (
                            wave_scope,
                            Event::WaveUpdated(calm_server::event::WaveUpdatedPayload::new(
                                wave, None,
                            )),
                        ),
                    ],
                ))
            })
        },
    )
    .await;

    assert!(res.is_err(), "role violation must surface as Err");
    // No rows survive — the violation rolled back BOTH the cove
    // and the wave even though the cove emit itself was legal.
    assert!(
        repo.coves_list().await.unwrap().is_empty(),
        "cove must be rolled back when any later event in the batch trips the gate",
    );
    // No broadcast either — commit-then-emit means the rollback
    // suppresses every event, not just the violating one.
    assert!(
        rx.try_recv().is_err(),
        "rolled-back tx must not broadcast any envelope",
    );
}

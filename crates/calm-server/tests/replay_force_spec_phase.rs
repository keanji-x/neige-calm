//! Issue #682 PR-1 — `POST /dev/force-spec-phase` on the replay binary.
//!
//! These tests exercise the replay-mode boot (`replay::boot_in_memory`,
//! the same path `cargo run --bin replay -- --serve` takes) plus the
//! fixtures-gated `SpecHarness::force_phase_for_dev` seam.
//!
//! Step-0 probe findings (recorded here and in the PR commit body):
//! in replay boot the shared codex app-server is `new_stub` (supervisor
//! state `Idle`, no `fake`), so `is_running()` is false and the
//! `spec-harness-start` operation submitted by `POST /api/waves` fails
//! at `validate` ("shared codex app-server is not running") — wave +
//! spec/report cards are created, but NO runtime row exists and NO
//! harness is registered. `probe_replay_boot_wave_create_leaves_spec_card_inert`
//! pins that, which is why the dev endpoint must stand up its own
//! runtime row + harness (fixtures-gated `run_unstarted_for_test`-style
//! spawn) instead of 404ing on registry miss.

#![cfg(feature = "fixtures")]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::error::CalmError;
use calm_server::event::Event;
use calm_server::harness::HarnessPhaseTag;
use calm_server::model::NewCove;
use calm_server::replay;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt;

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value, String) {
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
    let text = String::from_utf8_lossy(&bytes).to_string();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

struct Boot {
    app: axum::Router,
    state: calm_server::state::AppState,
    repo: Arc<calm_server::db::sqlite::SqlxRepo>,
    bus: calm_server::event::EventBus,
    cove_id: String,
}

impl Boot {
    fn dyn_repo(&self) -> Arc<dyn Repo> {
        self.repo.clone()
    }
}

async fn boot() -> Boot {
    let (repo, bus, state) = replay::boot_in_memory().await.expect("boot_in_memory");
    let cove = repo
        .cove_create(NewCove {
            name: "force-spec-phase-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        repo,
        bus,
        cove_id: cove.id.to_string(),
    }
}

async fn create_wave(boot: &Boot) -> (String, String) {
    let (status, body, text) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "probe wave",
            "cwd": "/tmp/issue-682-force-spec-phase",
            "attach_folder": true,
            "theme": { "fg": [255, 255, 255], "bg": [0, 0, 0] },
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "wave create must succeed: status={status} body={text}"
    );
    let wave_id = body["id"].as_str().expect("wave id").to_string();
    let cards = boot.repo.cards_by_wave(&wave_id).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("wave create auto-mints a spec codex card");
    (wave_id, spec_card.id.to_string())
}

/// Step-0 probe — pinned as a regression test. In replay boot (stub
/// shared codex app-server) the wave-create `spec-harness-start`
/// operation fails at validate, leaving the spec card with no runtime
/// row and no registered harness; `GET /spec/run` answers dormant.
#[tokio::test]
async fn probe_replay_boot_wave_create_leaves_spec_card_inert() {
    let boot = boot().await;
    let (_wave_id, spec_card_id) = create_wave(&boot).await;

    let runtime = boot
        .repo
        .runtime_get_active_for_card(&spec_card_id)
        .await
        .unwrap();
    assert!(
        runtime.is_none(),
        "stub daemon: spec-harness-start must have failed before runtime_start_tx; got {runtime:?}"
    );

    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/cards/{spec_card_id}/spec/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["runtime_id"].is_null() && body["phase"].is_null(),
        "spec/run must answer dormant in replay boot; got {body}"
    );
}

/// Count persisted `harness.phase.changed` rows whose `new_phase` is `tag`.
async fn phase_changed_events(boot: &Boot, tag: HarnessPhaseTag) -> usize {
    boot.repo
        .events_since(0, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, _, _, ev)| {
            matches!(
                ev,
                Event::HarnessPhaseChanged { new_phase, .. } if *new_phase == tag
            )
        })
        .count()
}

/// (a) Force-phase on a valid spec card: the forced phase must agree on
/// all three read surfaces — `GET /spec/run` (live in-memory snapshot),
/// the emitted `harness.phase.changed` event (persisted row + bus
/// envelope, i.e. what WS clients see), and the persisted runtime
/// snapshot (`handle_state_json`).
#[tokio::test]
async fn force_spec_phase_three_surfaces_agree() {
    let boot = boot().await;
    let (_wave_id, spec_card_id) = create_wave(&boot).await;
    let mut bus_rx = boot.bus.subscribe();

    let outcome = replay::force_spec_phase(
        &boot.state,
        boot.dyn_repo(),
        &spec_card_id,
        HarnessPhaseTag::TurnRunning,
    )
    .await
    .expect("force_spec_phase on a valid spec card");
    assert_eq!(outcome.card_id, spec_card_id);
    assert_eq!(
        outcome.old_phase,
        HarnessPhaseTag::PendingThreadStart,
        "dev-stood-up harness starts from the initial snapshot phase"
    );
    assert_eq!(outcome.new_phase, HarnessPhaseTag::TurnRunning);

    // Surface 1 — GET /spec/run reads the live harness snapshot.
    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/cards/{spec_card_id}/spec/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["runtime_id"], json!(outcome.runtime_id));
    assert_eq!(body["phase"], json!("turn_running"));

    // Surface 2 — `harness.phase.changed` is persisted AND broadcast.
    assert_eq!(
        phase_changed_events(&boot, HarnessPhaseTag::TurnRunning).await,
        1,
        "exactly one phase-changed row for the forced transition"
    );
    let envelope = timeout(Duration::from_secs(5), async {
        loop {
            let envelope = bus_rx.recv().await.expect("bus closed before phase event");
            if let Event::HarnessPhaseChanged {
                old_phase,
                new_phase,
                ..
            } = &envelope.event
            {
                return (*old_phase, *new_phase);
            }
        }
    })
    .await
    .expect("phase-changed envelope must reach bus subscribers");
    assert_eq!(
        envelope,
        (
            HarnessPhaseTag::PendingThreadStart,
            HarnessPhaseTag::TurnRunning
        )
    );

    // Surface 3 — the persisted runtime snapshot + status columns.
    let runtime = boot
        .repo
        .runtime_get_active_for_card(&spec_card_id)
        .await
        .unwrap()
        .expect("force must have stood up an active runtime row");
    assert_eq!(runtime.id, outcome.runtime_id);
    let snapshot = runtime
        .handle_state_json
        .expect("forced runtime must carry a persisted snapshot");
    assert_eq!(snapshot["phase"], json!("turn_running"));
    assert_eq!(
        runtime.status,
        calm_server::runtime_repo::RunStatus::TurnPending,
        "run_status_for(TurnRunning) writes turn_pending to the runtime row"
    );
}

/// (b) Guard chain mirrors the production `/spec/*` routes: non-spec cards
/// are 403 Forbidden, unknown cards 404 NotFound.
#[tokio::test]
async fn force_spec_phase_rejects_non_spec_and_unknown_cards() {
    let boot = boot().await;
    let (wave_id, _spec_card_id) = create_wave(&boot).await;

    let report_card = boot
        .repo
        .cards_by_wave(&wave_id)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind != "codex")
        .expect("wave create auto-mints a non-codex report card");
    let err = replay::force_spec_phase(
        &boot.state,
        boot.dyn_repo(),
        report_card.id.as_str(),
        HarnessPhaseTag::Idle,
    )
    .await
    .expect_err("non-spec card must be rejected");
    assert!(
        matches!(err, CalmError::Forbidden(_)),
        "expected Forbidden, got {err:?}"
    );
    assert_eq!(err.status(), StatusCode::FORBIDDEN);

    let err = replay::force_spec_phase(
        &boot.state,
        boot.dyn_repo(),
        "no-such-card",
        HarnessPhaseTag::Idle,
    )
    .await
    .expect_err("unknown card must be rejected");
    assert!(
        matches!(err, CalmError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

/// (c) Forcing the same phase twice goes through the persist path twice
/// but emits the phase event only once — `persist_snapshot` only emits
/// when `last_phase != new_phase`.
#[tokio::test]
async fn force_spec_phase_same_phase_twice_emits_one_event() {
    let boot = boot().await;
    let (_wave_id, spec_card_id) = create_wave(&boot).await;

    let first = replay::force_spec_phase(
        &boot.state,
        boot.dyn_repo(),
        &spec_card_id,
        HarnessPhaseTag::TurnRunning,
    )
    .await
    .expect("first force");
    assert_eq!(first.new_phase, HarnessPhaseTag::TurnRunning);

    let second = replay::force_spec_phase(
        &boot.state,
        boot.dyn_repo(),
        &spec_card_id,
        HarnessPhaseTag::TurnRunning,
    )
    .await
    .expect("second force of the same phase");
    assert_eq!(
        second.old_phase,
        HarnessPhaseTag::TurnRunning,
        "second force starts from the already-forced phase"
    );
    assert_eq!(second.new_phase, HarnessPhaseTag::TurnRunning);
    assert_eq!(
        second.runtime_id, first.runtime_id,
        "repeat forces reuse the stood-up runtime + harness"
    );

    assert_eq!(
        phase_changed_events(&boot, HarnessPhaseTag::TurnRunning).await,
        1,
        "same-phase repeat must not emit a duplicate phase event"
    );

    // And a real transition afterwards still emits exactly one more.
    let third = replay::force_spec_phase(
        &boot.state,
        boot.dyn_repo(),
        &spec_card_id,
        HarnessPhaseTag::TurnCompleted,
    )
    .await
    .expect("force to a different phase");
    assert_eq!(third.old_phase, HarnessPhaseTag::TurnRunning);
    assert_eq!(third.new_phase, HarnessPhaseTag::TurnCompleted);
    assert_eq!(
        phase_changed_events(&boot, HarnessPhaseTag::TurnCompleted).await,
        1
    );
}

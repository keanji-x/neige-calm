//! Issue #682 PR-1 — `POST /dev/force-planner-phase` on the replay binary.
//!
//! These tests exercise the replay-mode boot (`replay::boot_in_memory`,
//! the same path `cargo run --bin replay -- --serve` takes) plus the
//! fixtures-gated `PlannerHarness::force_phase_for_dev` seam.
//!
//! Step-0 probe findings (recorded here and in the PR commit body):
//! in replay boot the shared codex app-server is `new_stub` (supervisor
//! state `Idle`, no `fake`), so `is_running()` is false and the
//! `planner-harness-start` operation submitted by `POST /api/tracks` fails
//! at `validate` ("shared codex app-server is not running") — track +
//! planner/report cards are created, but NO runtime row exists and NO
//! harness is registered. `probe_replay_boot_track_create_leaves_planner_card_inert`
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
use calm_server::model::{CardRole, NewArea, NewCard};
use calm_server::replay;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

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
    area_id: String,
}

impl Boot {
    fn dyn_repo(&self) -> Arc<dyn Repo> {
        self.repo.clone()
    }
}

async fn boot() -> Boot {
    let (repo, bus, state) = replay::boot_in_memory().await.expect("boot_in_memory");
    let area = repo
        .area_create(NewArea {
            name: "force-planner-phase-test".into(),
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
        area_id: area.id.to_string(),
    }
}

async fn create_track(boot: &Boot) -> (String, String) {
    let (status, body, text) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "probe track",
            "cwd": attached_repo_fixture("issue-682-force-planner-phase"),
            "attach_folder": true,
            "theme": { "fg": [255, 255, 255], "bg": [0, 0, 0] },
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "track create must succeed: status={status} body={text}"
    );
    let track_id = body["id"].as_str().expect("track id").to_string();
    let cards = boot.repo.cards_by_track(&track_id).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("track create auto-mints a planner codex card");
    (track_id, planner_card.id.to_string())
}

/// Step-0 probe — pinned as a regression test. In replay boot (stub
/// shared codex app-server) the track-create `planner-harness-start`
/// operation fails at validate, leaving the planner card with no runtime
/// row and no registered harness; `GET /planner/run` answers dormant.
#[tokio::test]
async fn probe_replay_boot_track_create_leaves_planner_card_inert() {
    let boot = boot().await;
    let (_track_id, planner_card_id) = create_track(&boot).await;

    let runtime = boot
        .repo
        .session_projection_active_for_card(&planner_card_id)
        .await
        .unwrap();
    assert!(
        runtime.is_none(),
        "stub daemon: planner-harness-start must have failed before session_start_runtime_tx; got {runtime:?}"
    );

    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/cards/{planner_card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["worker_session_id"].is_null() && body["phase"].is_null(),
        "planner/run must answer dormant in replay boot; got {body}"
    );
}

#[tokio::test]
async fn area_chat_planner_start_backdoors_are_forbidden_without_runtime_rows() {
    let boot = boot().await;
    let (track_id, planner_card_id) = create_track(&boot).await;
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(&track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();

    let (reset_status, _, reset_text) = post(
        boot.app.clone(),
        &format!("/api/cards/{planner_card_id}/planner/reset"),
        json!({}),
    )
    .await;
    assert_eq!(reset_status, StatusCode::FORBIDDEN, "{reset_text}");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
        .bind(&planner_card_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "reset fence must precede runtime creation");

    let force_error = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
        HarnessPhaseTag::Idle,
    )
    .await
    .expect_err("the fixtures endpoint engine must reject area-chat planner cards");
    assert_eq!(force_error.status(), StatusCode::FORBIDDEN);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
        .bind(&planner_card_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "force fence must precede runtime creation");
}

#[tokio::test]
async fn area_chat_plain_chat_card_can_be_forced_and_uses_codex_card_runtime() {
    let boot = boot().await;
    let (track_id, _) = create_track(&boot).await;
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(&track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let chat = boot
        .repo
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    boot.state
        .card_role_cache
        .insert(chat.id.clone(), CardRole::Worker, chat.track_id.clone());

    replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        chat.id.as_str(),
        HarnessPhaseTag::Idle,
    )
    .await
    .expect("Worker plain-chat card must pass the replay area-chat fence");
    let runtime = boot
        .repo
        .session_projection_active_for_card(&chat.id.to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.kind,
        calm_server::session_projection_repo::WorkerSessionKind::CodexCard
    );
    if let Some(handle) = boot.state.harness.remove(&runtime.id) {
        handle.shutdown().await.unwrap();
    }
}

/// Count persisted `harness.phase.changed` rows whose `new_phase` is `tag`.
async fn phase_changed_events(boot: &Boot, tag: HarnessPhaseTag) -> usize {
    boot.repo
        .events_since(0, i64::MAX)
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

/// (a) Force-phase on a valid planner card: the forced phase must agree on
/// all three read surfaces — `GET /planner/run` (live in-memory snapshot),
/// the emitted `harness.phase.changed` event (persisted row + bus
/// envelope, i.e. what WS clients see), and the persisted runtime
/// snapshot (`handle_state_json`).
#[tokio::test]
async fn force_planner_phase_three_surfaces_agree() {
    let boot = boot().await;
    let (_track_id, planner_card_id) = create_track(&boot).await;
    let mut bus_rx = boot.bus.subscribe();

    let outcome = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
        HarnessPhaseTag::TurnRunning,
    )
    .await
    .expect("force_planner_phase on a valid planner card");
    assert_eq!(outcome.card_id, planner_card_id);
    assert_eq!(
        outcome.old_phase,
        HarnessPhaseTag::PendingThreadStart,
        "dev-stood-up harness starts from the initial snapshot phase"
    );
    assert_eq!(outcome.new_phase, HarnessPhaseTag::TurnRunning);

    // Surface 1 — GET /planner/run reads the live harness snapshot.
    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/cards/{planner_card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["worker_session_id"], json!(outcome.worker_session_id));
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
        .session_projection_active_for_card(&planner_card_id)
        .await
        .unwrap()
        .expect("force must have stood up an active runtime row");
    assert_eq!(runtime.id, outcome.worker_session_id);
    let snapshot = runtime
        .handle_state_json
        .expect("forced runtime must carry a persisted snapshot");
    assert_eq!(snapshot["phase"], json!("turn_running"));
    assert_eq!(
        runtime.status,
        calm_server::session_projection_repo::WorkerSessionState::TurnPending,
        "run_status_for(TurnRunning) writes turn_pending to the runtime row"
    );
}

/// (b) Guard chain mirrors the production `/planner/*` routes: non-planner cards
/// are 403 Forbidden, unknown cards 404 NotFound.
#[tokio::test]
async fn force_planner_phase_rejects_non_planner_and_unknown_cards() {
    let boot = boot().await;
    let (track_id, _planner_card_id) = create_track(&boot).await;

    let report_card = boot
        .repo
        .cards_by_track(&track_id)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind != "codex")
        .expect("track create auto-mints a non-codex report card");
    let err = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        report_card.id.as_str(),
        HarnessPhaseTag::Idle,
    )
    .await
    .expect_err("non-planner card must be rejected");
    assert!(
        matches!(err, CalmError::Forbidden(_)),
        "expected Forbidden, got {err:?}"
    );
    assert_eq!(err.status(), StatusCode::FORBIDDEN);

    let err = replay::force_planner_phase(
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
async fn force_planner_phase_same_phase_twice_emits_one_event() {
    let boot = boot().await;
    let (_track_id, planner_card_id) = create_track(&boot).await;

    let first = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
        HarnessPhaseTag::TurnRunning,
    )
    .await
    .expect("first force");
    assert_eq!(first.new_phase, HarnessPhaseTag::TurnRunning);

    let second = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
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
        second.worker_session_id, first.worker_session_id,
        "repeat forces reuse the stood-up runtime + harness"
    );

    assert_eq!(
        phase_changed_events(&boot, HarnessPhaseTag::TurnRunning).await,
        1,
        "same-phase repeat must not emit a duplicate phase event"
    );

    // And a real transition afterwards still emits exactly one more.
    let third = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
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

/// (d) #684 review — `wedged` is rejected with 400. Persisting a forced
/// Wedged writes `WorkerSessionState::Failed`, which `session_projection_active_for_card`
/// filters out: `GET /planner/run` would instantly answer dormant and the
/// next force would mint a second runtime. The guard runs before any
/// stand-up, so a rejected force leaves no runtime row behind.
#[tokio::test]
async fn force_planner_phase_rejects_wedged_with_bad_request() {
    let boot = boot().await;
    let (_track_id, planner_card_id) = create_track(&boot).await;

    let err = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
        HarnessPhaseTag::Wedged,
    )
    .await
    .expect_err("wedged must be rejected");
    assert!(
        matches!(err, CalmError::BadRequest(_)),
        "expected BadRequest, got {err:?}"
    );
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    let message = err.to_string();
    for tag in [
        "pending_thread_start",
        "idle",
        "issuing_turn",
        "issuing_interrupt",
        "turn_running",
        "turn_completed",
        "resumed",
    ] {
        assert!(
            message.contains(tag),
            "error must list supported tag {tag}; got: {message}"
        );
    }

    let runtime = boot
        .repo
        .session_projection_active_for_card(&planner_card_id)
        .await
        .unwrap();
    assert!(
        runtime.is_none(),
        "wedged guard runs before stand-up; no runtime row may be created, got {runtime:?}"
    );
}

/// (e) #684 review — the dev-stood-up harness must never run the issuing
/// loop against the replay stub daemon. `/planner/input` (registry fast path,
/// hard-fire `UserMessage`) on a forced-`idle` harness would otherwise
/// flip to `issuing_turn` on the next 50ms tick, fail `turn_start` against
/// the stub, re-buffer with `hard_fire`, and churn phases forever. With
/// issuance paused the observation enqueues and the phase stays put.
#[tokio::test]
async fn forced_harness_planner_input_enqueues_without_issuing_turns() {
    let boot = boot().await;
    let (_track_id, planner_card_id) = create_track(&boot).await;

    let outcome = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
        HarnessPhaseTag::Idle,
    )
    .await
    .expect("force to idle");

    // PR-2's happy path: `/planner/input` through the real route. The forced
    // harness is registered, so `ensure_live_planner_harness` takes the
    // registry fast path (no daemon-liveness 503).
    let (status, body, text) = post(
        boot.app.clone(),
        &format!("/api/cards/{planner_card_id}/planner/input"),
        json!({ "text": "hello from the dev-forced harness" }),
    )
    .await;
    assert!(
        status.is_success(),
        "/planner/input must stay functional on a forced harness: status={status} body={text}"
    );
    assert_eq!(body["worker_session_id"], json!(outcome.worker_session_id));

    // UserMessage is hard-fire: an unpaused harness would issue on the
    // next 50ms tick. Give the run loop several ticks (and clear the
    // 250ms debounce floor) to prove nothing fires.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let (status, run) = get(
        boot.app.clone(),
        &format!("/api/cards/{planner_card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        run["phase"],
        json!("idle"),
        "phase must stay stable (no issuing churn); got {run}"
    );
    assert_eq!(
        phase_changed_events(&boot, HarnessPhaseTag::IssuingTurn).await,
        0,
        "no issuing_turn transition may ever be emitted (turn_start attempt)"
    );
    let harness = boot
        .state
        .harness
        .get(&outcome.worker_session_id)
        .expect("forced harness stays registered");
    assert_eq!(
        harness.pending_len_for_test().await,
        1,
        "observation must stay enqueued — issuance is paused, not observation intake"
    );
}

/// (f) #684 review — the `/dev/reset` drain seam: every registered harness
/// is shut down and deregistered so reseeding can't leave orphaned
/// 50ms-tick tasks warning against wiped runtime rows. A later force
/// stands a fresh harness back up.
#[tokio::test]
async fn shutdown_registered_harnesses_drains_registry_and_allows_reforce() {
    let boot = boot().await;
    let (_track_id, planner_card_id) = create_track(&boot).await;

    let outcome = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
        HarnessPhaseTag::Idle,
    )
    .await
    .expect("force to idle stands the harness up");
    assert_eq!(boot.state.harness.len_active(), 1);

    let drained = replay::shutdown_registered_harnesses(&boot.state).await;
    assert_eq!(drained, 1, "exactly the dev-forced harness is drained");
    assert_eq!(
        boot.state.harness.len_active(),
        0,
        "registry must be empty after the dev-reset drain"
    );

    // Re-forcing after a drain recovers: the runtime row is still active,
    // so the same runtime gets a freshly spawned harness.
    let again = replay::force_planner_phase(
        &boot.state,
        boot.dyn_repo(),
        &planner_card_id,
        HarnessPhaseTag::TurnRunning,
    )
    .await
    .expect("force after drain respawns the harness");
    assert_eq!(again.worker_session_id, outcome.worker_session_id);
    assert!(
        boot.state.harness.get(&again.worker_session_id).is_some(),
        "re-force must register a fresh harness"
    );
}

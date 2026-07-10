//! Regression anchor: codex `PermissionRequest` hook ingest → role gate →
//! card FSM → wave-scoped `any_card_needs_input` overlay, end-to-end at the
//! kernel level (no real codex CLI involved).
//!
//! Two cases live in this file:
//!
//!   1. **Worker card** — passes today. Documents the working pipeline so a
//!      future regression that breaks the FSM or the aggregator surfaces
//!      here instead of in a hand-tested UI bug report.
//!
//!   2. **Spec card** — historically failed at the role gate. The
//!      `role_gate.rs` `Some(CardRole::Spec)` arm of the `AiCodex` actor
//!      match unconditionally rejected every write, including the codex
//!      bridge's lifecycle hook POST. The fix carves out `Event::CodexHook`
//!      from an `AiCodex(spec_card)` actor as a pure lifecycle
//!      observation (the bridge runs as a subprocess of codex regardless
//!      of card role and can't easily know the role at fire time); other
//!      events from `AiCodex(spec_card)` are still refused, and
//!      `WaveUpdated` is still gated separately at the top of the
//!      function. This test pins the regression: without the carveout,
//!      the FSM never observes `permission_request`, and the wave
//!      `any_card_needs_input` overlay never flips.
//!
//! Patterns lifted from:
//!
//!   * `crates/calm-server/tests/codex_ingest.rs` — `AppState` /
//!     `EventBus` / `actor_middleware` scaffolding.
//!   * `crates/calm-server/src/card_fsm.rs::tests::needs_input_overlay_*`
//!     — what to poll for once the FSM has observed `permission_request`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use calm_server::actor::actor_middleware;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::Value;
use tower::ServiceExt;

/// How long we'll wait for the `any_card_needs_input` overlay to land
/// after POSTing the hook. The FSM commits inside a single transaction
/// (overlay + event row) so this only needs to outlast the tokio task
/// hop + sqlite commit; 2 s is comfortable.
const OVERLAY_DEADLINE: Duration = Duration::from_secs(2);
/// Poll interval inside the deadline.
const OVERLAY_POLL: Duration = Duration::from_millis(50);

/// Shared fixture used by both cases. Returns the assembled axum app +
/// the repo (so the assertions can poll overlays directly) + the card id
/// (so the assertions can scope to it).
///
/// The caller decides the card's role via `role`. We override the cache
/// entry after the standard `card_create` (which seeds `Worker`) so we
/// don't have to fan out to the `card_create_with_id_tx` machinery that
/// the production spec-card mint uses. The role gate only reads the
/// cache, so a cache override is sufficient to reproduce the gate
/// decision the production path would make.
async fn setup(role: CardRole) -> (axum::Router, Arc<dyn Repo>, String, String) {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
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
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();

    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    // Override: `card_create` seeds `Worker`; for the Spec-card case we
    // need the cache to report `Spec`, mirroring what the real
    // `card_with_codex_create_tx` would have written.
    cache.insert(card.id.clone(), role, wave.id.clone());

    let wave_cove_cache = WaveCoveCache::new();
    // The wave-cove cache is write-through populated in `wave_create_tx`,
    // but our SqlxRepo instance holds its own cache; we re-seed the one
    // we'll thread through `AppState` and the FSM so the role gate's
    // worker-scope cross-check (and our wave-scoped overlay aggregate)
    // can both resolve `wave -> cove` without going to the DB.
    wave_cove_cache.insert(wave.id.clone(), cove.id.clone());

    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-perm-req-overlay"),
            Vec::new(),
            events.clone(),
            calm_server::state::WriteContext::new(cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    // Spawn the FSM projector *before* we POST the hook, so it's
    // subscribed by the time the bus broadcasts `Event::CodexHook`.
    // (The existing `codex_ingest.rs` integration test stops at the bus
    // assertion; we want the full overlay-aggregation path.)
    calm_server::card_fsm::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wave_cove_cache),
    );
    // Give the spawn a tick to subscribe — matches the unit-test pattern
    // in `card_fsm::tests`.
    tokio::task::yield_now().await;

    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    (app, repo, card.id.to_string(), wave.id.to_string())
}

/// POST the `PermissionRequest` hook for `card_id` and assert 204.
async fn post_permission_request(app: &axum::Router, card_id: &str) {
    let body = serde_json::json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": "calm__report__write",
        "tool_input": {},
    })
    .to_string();
    let uri = format!("/internal/codex/hook?card_id={card_id}");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        204,
        "POST /internal/codex/hook expected 204; got {}",
        resp.status()
    );
}

/// Poll `overlays_for("wave", wave_id)` until the
/// `any_card_needs_input` overlay shows `value: true`, or the deadline
/// expires. Returns the overlay payload on success; panics with a
/// descriptive message on timeout.
async fn await_wave_needs_input(repo: &Arc<dyn Repo>, wave_id: &str) -> Value {
    let poll = async {
        loop {
            let overlays = repo.overlays_for("wave", wave_id).await.unwrap();
            if let Some(o) = overlays.iter().find(|o| o.kind == "any_card_needs_input")
                && o.payload.get("value").and_then(Value::as_bool) == Some(true)
            {
                return o.payload.clone();
            }
            tokio::time::sleep(OVERLAY_POLL).await;
        }
    };
    match tokio::time::timeout(OVERLAY_DEADLINE, poll).await {
        Ok(payload) => payload,
        Err(_) => {
            // Re-read overlays for the failure message so the caller sees
            // exactly what the projection landed on (or didn't).
            let overlays = repo.overlays_for("wave", wave_id).await.unwrap();
            panic!(
                "timed out waiting for `any_card_needs_input` overlay with `value: true` \
                 on wave {wave_id}; current wave overlays: {overlays:?}",
            );
        }
    }
}

/// Poll the card-scoped `status` overlay until it reads `AwaitingInput`,
/// or the deadline expires. This isolates "FSM observed the transition"
/// from "FSM observed but the wave aggregator broke" — if the card
/// overlay flipped but the wave one didn't, the bug is in
/// `recompute_wave_needs_input`; if neither flipped, the bug is upstream
/// of the FSM (e.g. the role gate refusing the write).
async fn await_card_awaiting_input(repo: &Arc<dyn Repo>, card_id: &str) {
    let poll = async {
        loop {
            let overlays = repo.overlays_for("card", card_id).await.unwrap();
            if let Some(o) = overlays.iter().find(|o| o.kind == "status")
                && o.payload.get("state").and_then(Value::as_str) == Some("AwaitingInput")
            {
                return;
            }
            tokio::time::sleep(OVERLAY_POLL).await;
        }
    };
    if tokio::time::timeout(OVERLAY_DEADLINE, poll).await.is_err() {
        let overlays = repo.overlays_for("card", card_id).await.unwrap();
        panic!(
            "timed out waiting for card status overlay `state: AwaitingInput` on card \
             {card_id}; current card overlays: {overlays:?}",
        );
    }
}

#[tokio::test]
async fn worker_card_permission_request_flips_wave_needs_input() {
    let (app, repo, card_id, wave_id) = setup(CardRole::Worker).await;

    post_permission_request(&app, &card_id).await;

    // Card-scoped status flips first (the FSM writes it before the
    // wave-scoped aggregate).
    await_card_awaiting_input(&repo, &card_id).await;
    // Wave-scoped aggregate follows.
    let payload = await_wave_needs_input(&repo, &wave_id).await;
    assert_eq!(payload["value"], Value::Bool(true));
}

#[tokio::test]
async fn spec_card_permission_request_flips_wave_needs_input() {
    let (app, repo, card_id, wave_id) = setup(CardRole::Spec).await;

    // The POST itself returns 204 today: `routes::codex::ingest_hook`
    // uses `log_pure_event`, which on a role-gate violation rolls the
    // write back but the HTTP handler converts the resulting error into
    // a 4xx/5xx. If that ever surfaces, the post helper will panic on
    // the status mismatch — and that *is* the visible failure mode of
    // the bug from a different angle. The canonical failure we expect
    // here, however, is the overlay never flipping, which the poller
    // surfaces below.
    post_permission_request(&app, &card_id).await;

    // Same assertions as the Worker case. With the bug in place, the
    // role gate refuses the write at `role_gate.rs:226-232`, the FSM
    // never sees the CodexHook event, and neither overlay flips —
    // the first await below times out with a descriptive message.
    await_card_awaiting_input(&repo, &card_id).await;
    let payload = await_wave_needs_input(&repo, &wave_id).await;
    assert_eq!(payload["value"], Value::Bool(true));
}

//! Codex `PermissionRequest` hook ingest → role gate → card FSM → track-scoped `any_card_needs_input` overlay.
//! The Planner case pins the role gate's carve-out: `Event::CodexHook` from an `AiCodex(planner_card)` actor is
//! a lifecycle observation and must not be refused like other planner-card writes.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use calm_server::actor::actor_middleware;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use serde_json::Value;
use tower::ServiceExt;

/// The FSM commits overlay + event row in one transaction, so this only needs to outlast a task hop + sqlite commit.
const OVERLAY_DEADLINE: Duration = Duration::from_secs(2);
const OVERLAY_POLL: Duration = Duration::from_millis(50);

/// The role gate only reads the cache, so overriding the cache entry after `card_create` reproduces the
/// production gate decision for either role.
async fn setup(role: CardRole) -> (axum::Router, Arc<dyn Repo>, String, String) {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();

    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    // `card_create` seeds `Worker`; override for the Planner case.
    cache.insert(card.id.clone(), role, track.id.clone());

    let track_area_cache = TrackAreaCache::new();
    // Re-seed the track-area cache threaded through `AppState` and the FSM, so the role gate's worker-scope
    // cross-check and the track-scoped aggregate resolve `track -> area` without the DB.
    track_area_cache.insert(track.id.clone(), area.id.clone());

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
            calm_server::state::WriteContext::new(cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache.clone()),
        Some(track_area_cache.clone()),
    );

    // Spawn the FSM projector before the POST so it is subscribed when the bus broadcasts `Event::CodexHook`.
    calm_server::card_fsm::spawn(
        repo.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), track_area_cache),
    );
    // Give the spawn a tick to subscribe.
    tokio::task::yield_now().await;

    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    (app, repo, card.id.to_string(), track.id.to_string())
}

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

/// Poll until the track's `any_card_needs_input` overlay shows `value: true`; panics on timeout.
async fn await_track_needs_input(repo: &Arc<dyn Repo>, track_id: &str) -> Value {
    let poll = async {
        loop {
            let overlays = repo.overlays_for("track", track_id).await.unwrap();
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
            let overlays = repo.overlays_for("track", track_id).await.unwrap();
            panic!(
                "timed out waiting for `any_card_needs_input` overlay with `value: true` \
                 on track {track_id}; current track overlays: {overlays:?}",
            );
        }
    }
}

/// Isolates "FSM observed the transition" from "the track aggregator broke": if the card overlay flipped but the
/// track one did not, the bug is in `recompute_track_needs_input`; if neither, it is upstream of the FSM.
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
async fn worker_card_permission_request_flips_track_needs_input() {
    let (app, repo, card_id, track_id) = setup(CardRole::Worker).await;

    post_permission_request(&app, &card_id).await;

    // Card-scoped status flips first (the FSM writes it before the track-scoped aggregate).
    await_card_awaiting_input(&repo, &card_id).await;
    let payload = await_track_needs_input(&repo, &track_id).await;
    assert_eq!(payload["value"], Value::Bool(true));
}

#[tokio::test]
async fn planner_card_permission_request_flips_track_needs_input() {
    let (app, repo, card_id, track_id) = setup(CardRole::Planner).await;

    // The POST returns 204 even when the role gate rolls the write back; the visible failure is the overlay never flipping.
    post_permission_request(&app, &card_id).await;

    await_card_awaiting_input(&repo, &card_id).await;
    let payload = await_track_needs_input(&repo, &track_id).await;
    assert_eq!(payload["value"], Value::Bool(true));
}

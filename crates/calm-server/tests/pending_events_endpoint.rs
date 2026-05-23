//! PR8 (#136) — integration tests for the HTTP fallback long-poll
//! endpoint `/internal/codex/pending_events`.
//!
//! Mirrors `codex_ingest.rs`'s shape: build a real `AppState` via
//! `from_parts`, mount the routes through `axum::Router`, fire
//! requests via `tower::ServiceExt::oneshot`.
//!
//! Coverage:
//!   * happy path (emit events → GET returns them with right shape)
//!   * timeout returns empty events array (not an error)
//!   * unknown card_id → 404
//!   * malformed query → 400
//!   * cursor is shared with the MCP tool's cache (a previous wait
//!     advances the cursor that a follow-up pending_events sees)

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use calm_server::actor::actor_middleware;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::write_with_event_typed;
use calm_server::db::{prelude::*, sqlite::SqlxRepo};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::event_cursor::EventCursorCache;
use calm_server::ids::ActorId;
use calm_server::mcp_server::tools::wait::wait_for_events_for_card;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, Wave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Boot {
    state: AppState,
    repo: Arc<dyn Repo>,
    card_id: String,
    wave_id: String,
    role_cache: CardRoleCache,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "pending-test".into(),
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
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    cache.insert(card.id.clone(), CardRole::Spec, wave.id.clone());

    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events.clone(),
            cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache.clone()),
    );

    Boot {
        state,
        card_id: card.id.as_str().to_string(),
        wave_id: wave.id.as_str().to_string(),
        repo: repo as Arc<dyn Repo>,
        role_cache: cache,
    }
}

fn build_app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state)
}

async fn emit_wave_event(boot: &Boot) -> i64 {
    let wave = boot.repo.wave_get(&boot.wave_id).await.unwrap().unwrap();
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_clone = wave.clone();
    let (_, id) = write_with_event_typed::<Wave, _>(
        boot.state.repo.as_ref(),
        ActorId::User,
        scope,
        None,
        &boot.state.events,
        &boot.role_cache,
        move |_tx| {
            let w = wave_clone.clone();
            Box::pin(async move { Ok((w.clone(), Event::WaveUpdated(w))) })
        },
    )
    .await
    .expect("emit wave.updated");
    id
}

async fn body_to_value(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response body is JSON")
}

// ---------------------------------------------------------------------------
// Happy paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pending_events_returns_catch_up_immediately() {
    let b = boot().await;
    let _id1 = emit_wave_event(&b).await;
    let _id2 = emit_wave_event(&b).await;

    let app = build_app(b.state.clone());
    let uri = format!(
        "/internal/codex/pending_events?card_id={}&timeout_ms=30000&since=0",
        b.card_id,
    );
    let t0 = std::time::Instant::now();
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
    assert_eq!(resp.status(), 200);
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "catch-up should be fast",
    );

    let value = body_to_value(resp).await;
    let events = value["events"].as_array().expect("events array");
    assert_eq!(
        events.len(),
        2,
        "catch-up returned both pre-seeded events: {value}"
    );
    assert!(value["since"].is_i64(), "since echoes the max id: {value}",);
}

#[tokio::test]
async fn pending_events_returns_empty_array_on_timeout() {
    let b = boot().await;
    let app = build_app(b.state);
    let uri = format!(
        "/internal/codex/pending_events?card_id={}&timeout_ms=100",
        b.card_id,
    );
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
    assert_eq!(resp.status(), 200, "empty result is 200, not 204/404");
    let value = body_to_value(resp).await;
    assert!(value["events"].as_array().unwrap().is_empty());
    assert!(value["since"].is_null());
}

// ---------------------------------------------------------------------------
// Validation errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pending_events_unknown_card_id_returns_404() {
    let b = boot().await;
    let app = build_app(b.state);
    let uri = "/internal/codex/pending_events?card_id=nonexistent-card-id&timeout_ms=100";
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
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn pending_events_empty_card_id_returns_400() {
    let b = boot().await;
    let app = build_app(b.state);
    // We URL-encode `   ` to ensure the trim() reduces to empty.
    let uri = "/internal/codex/pending_events?card_id=%20%20%20";
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
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn pending_events_negative_since_returns_400() {
    let b = boot().await;
    let app = build_app(b.state);
    let uri = format!(
        "/internal/codex/pending_events?card_id={}&since=-5",
        b.card_id,
    );
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
    assert_eq!(resp.status(), 400);
}

// ---------------------------------------------------------------------------
// Cursor sharing with the MCP tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pending_events_shares_cursor_cache_with_mcp_tool() {
    let b = boot().await;
    let _id1 = emit_wave_event(&b).await;
    let _id2 = emit_wave_event(&b).await;

    // First: directly call the shared helper (the same one the MCP
    // tool uses) to advance the cursor. This proves both paths reach
    // the same cache.
    let card_id_typed = calm_server::ids::CardId::from(b.card_id.as_str());
    let wave_id_typed = calm_server::ids::WaveId::from(b.wave_id.as_str());
    let (envs, max) = wait_for_events_for_card(
        b.state.repo.as_ref(),
        &b.state.events,
        &b.state.event_cursor_cache,
        &card_id_typed,
        &wave_id_typed,
        Some(0),
        5_000,
    )
    .await
    .expect("first call ok");
    assert_eq!(envs.len(), 2);
    let advanced_to = max.expect("max id present");

    // Now the HTTP endpoint with no explicit since — should pick up
    // the cursor and return nothing within the short timeout.
    let app = build_app(b.state.clone());
    let uri = format!(
        "/internal/codex/pending_events?card_id={}&timeout_ms=100",
        b.card_id,
    );
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
    assert_eq!(resp.status(), 200);
    let value = body_to_value(resp).await;
    assert!(
        value["events"].as_array().unwrap().is_empty(),
        "cursor must be honored by the HTTP endpoint: cache had {advanced_to}, body={value}",
    );

    // Belt-and-suspenders: confirm the cache reads the value directly.
    assert_eq!(b.state.event_cursor_cache.get(&card_id_typed), advanced_to);
    // Silence unused warning for the cache helper we constructed for
    // the test fixture shape.
    let _ = EventCursorCache::new();
}

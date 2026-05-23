//! Supplemental backward-compat coverage for the schemaVersion read-side
//! guard (issue #199 followup — supplemental to PR #225).
//!
//! PR #225's `tests/replay_fixtures.rs` already pins the *replay* leg of
//! the legacy missing-`schemaVersion` round-trip (via the
//! `schema_forward_compat.events.json` fixture against both replay and
//! the REST `/api/events` route). This file adds two coverage gaps that
//! the fixture-driven approach can't easily express:
//!
//!   1. The **live-broadcast** leg of the same legacy missing-field
//!      case — rows written before the `schemaVersion` field existed
//!      must still be delivered on `bus.recv()` (not just on replay).
//!      The validator treats absent as version 1
//!      (see `validation::payload_schema_version`); we lock in that
//!      contract from the live surface so a future refactor that
//!      tightens "absent → drop" announces itself here.
//!
//!   2. The **cursor-advance** assertion for the mixed-history case —
//!      when replay encounters a legacy row followed by a future-version
//!      row that gets dropped, `_replay_complete._id` must still advance
//!      past the dropped row so the client doesn't re-poll it.

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::sqlite::{SqlxRepo, overlay_upsert_tx};
use calm_server::db::{Repo, write_with_event_typed};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::NewOverlay;
use calm_server::plugin_host::PluginHost;
use calm_server::state::{AppState, DaemonClient};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TMessage;

// ---------------------------------------------------------------------------
// Boot helpers — mirror tests/ws_replay.rs::boot() so the harness shape
// stays familiar to anyone jumping between the two files.
// ---------------------------------------------------------------------------

async fn boot() -> (std::net::SocketAddr, Arc<SqlxRepo>, EventBus) {
    let events = EventBus::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(calm_server::plugin_host::PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-replay-legacy"),
            Vec::new(),
            events.clone(),
            CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    let app = axum::Router::new().merge(ws::router()).with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, repo, events)
}

/// Seed an Overlay row whose payload omits `schemaVersion` entirely —
/// simulating a row written before the field existed. The validator
/// treats this as version 1 (the only version this kernel currently
/// supports), so the read-side guard MUST pass it through.
async fn seed_legacy_overlay(repo: &SqlxRepo, bus: &EventBus) -> i64 {
    let legacy = NewOverlay {
        plugin_id: "p-legacy".into(),
        entity_kind: "wave".into(),
        entity_id: "w-legacy".into(),
        kind: "status".into(),
        // Deliberately no `schemaVersion` field — the absent-as-v1 case.
        payload: json!({ "state": "running" }),
    };
    let (_o, event_id) = write_with_event_typed(
        repo as &dyn Repo,
        ActorId::User,
        EventScope::System,
        None,
        bus,
        &CardRoleCache::new(),
        &calm_server::wave_cove_cache::WaveCoveCache::new(),
        move |tx| {
            Box::pin(async move {
                let o = overlay_upsert_tx(tx, legacy).await?;
                Ok((o.clone(), Event::OverlaySet(o)))
            })
        },
    )
    .await
    .expect("seed legacy overlay");
    event_id
}

/// Helper that drains one JSON frame off the WS stream and asserts a
/// reasonable timeout — copied from `tests/ws_replay.rs::recv_json`.
async fn recv_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed unexpectedly")
        .expect("ws error");
    match msg {
        TMessage::Text(t) => serde_json::from_str(&t.to_string()).expect("json"),
        other => panic!("expected text, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// LIVE: missing schemaVersion is delivered on the broadcast leg too
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_broadcast_delivers_overlay_set_with_missing_schema_version() {
    let (addr, repo, bus) = boot().await;

    // Subscribe first, then write — so the row hits the live-broadcast
    // path (`bus.recv()` branch) rather than the replay query.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    // No `since` → no replay, just live. Give the server a tick to set
    // up its subscriber before we emit.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let legacy_id = seed_legacy_overlay(&repo, &bus).await;

    let v = recv_json(&mut ws).await;
    assert_eq!(v["_id"], legacy_id);
    assert_eq!(v["ev"], "overlay.set");
    assert_eq!(v["data"]["payload"]["state"], "running");
    assert!(v["data"]["payload"].get("schemaVersion").is_none());
}

// ---------------------------------------------------------------------------
// Mixed-history regression: legacy AND future rows interleaved
// ---------------------------------------------------------------------------
//
// A realistic upgrade-window scenario: the DB carries pre-schemaVersion
// rows AND post-schemaVersion future rows. Replay must deliver the
// legacy row, drop the future row, AND advance the cursor past both so
// the client never re-polls either.

#[tokio::test]
async fn replay_mixes_legacy_pass_through_with_future_drop() {
    let (addr, repo, bus) = boot().await;

    // Seed legacy (delivered) → then future (dropped). Order matters
    // because the assertion uses `_replay_complete._id` to confirm the
    // cursor advanced past BOTH rows.
    let legacy_id = seed_legacy_overlay(&repo, &bus).await;

    let future = NewOverlay {
        plugin_id: "p-future".into(),
        entity_kind: "wave".into(),
        entity_id: "w-future".into(),
        kind: "status".into(),
        payload: json!({ "schemaVersion": 999, "state": "from-future" }),
    };
    let (_o, future_id) = write_with_event_typed(
        repo.as_ref() as &dyn Repo,
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &CardRoleCache::new(),
        &calm_server::wave_cove_cache::WaveCoveCache::new(),
        move |tx| {
            Box::pin(async move {
                let o = overlay_upsert_tx(tx, future).await?;
                Ok((o.clone(), Event::OverlaySet(o)))
            })
        },
    )
    .await
    .unwrap();
    assert!(future_id > legacy_id, "future row must land after legacy");

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    // First frame: legacy row delivered.
    let v = recv_json(&mut ws).await;
    assert_eq!(v["_id"], legacy_id);
    assert_eq!(v["ev"], "overlay.set");
    assert!(v["data"]["payload"].get("schemaVersion").is_none());

    // Next frame: `_replay_complete` — future row was dropped silently,
    // cursor still advances to its id so the client doesn't re-poll.
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(
        done["_id"], future_id,
        "cursor must advance past dropped future row",
    );
}

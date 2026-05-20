//! Replay-based regression tests — the **first feature riding on the full
//! sync engine**. Per design doc §6.3, fixtures live under
//! `tests/fixtures/events/` as JSON traces. A test loads each fixture,
//! raw-inserts the events into a fresh in-memory server, connects a WS
//! subscriber with `since=0`, drains the replay window, then asserts that
//! the resulting state — both event sequence and any final overlay
//! payloads — matches the fixture's `expected` block.
//!
//! Scope E ships just one fixture (`wave-grid-layout-trace`) — the wave-
//! grid layout migration's smoke trace. The infrastructure here is the
//! seed for the broader "bug report = file + one replay command" story:
//! future bugs become reproducible artifacts in the same shape.
//!
//! Why a separate test file from `sync_engine.rs`: that file exercises
//! the write-side atomicity / replay-then-live ordering of the sync
//! engine itself (Scope A's contracts). This file is about the *consumer*
//! side — given a known-good event log, the system converges to a known
//! state. The two halves share the same WS protocol but the test
//! ergonomics are different (fixture loader vs hand-driven writes).
//!
//! ## Fixture format
//!
//! ```json
//! {
//!   "name": "...",                  // descriptive — surfaces in test output
//!   "description": "...",           // free-form notes
//!   "events": [                     // inserted in order via Repo::log_pure_event
//!     { "kind": "card.added", "actor": "user", "payload": { ... } },
//!     ...
//!   ],
//!   "expected": {
//!     "last_event_kind": "overlay.set",
//!     "layout_positions": { "<card_id>": { x, y, w, h }, ... }
//!   }
//! }
//! ```
//!
//! We seed events via `Repo::log_pure_event` rather than the
//! `#[cfg(test)]`-gated `SqlxRepo::event_append_fixture` (that helper
//! is private to crate-internal unit tests; integration tests live in
//! a separate compilation unit and can't reach it). `log_pure_event`
//! is the same shape — it persists an event row + broadcasts on the
//! bus — and gives us deterministic `events.id`s in append order
//! without dragging in entity-table writes the fixture doesn't need.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, DaemonClient};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TMessage;

// ---------------------------------------------------------------------------
// Fixture shape
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
    events: Vec<FixtureEvent>,
    expected: FixtureExpected,
}

#[derive(Debug, Deserialize)]
struct FixtureEvent {
    kind: String,
    actor: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct FixtureExpected {
    last_event_kind: String,
    #[serde(default)]
    layout_positions: serde_json::Map<String, serde_json::Value>,
}

fn load_fixture(name: &str) -> Fixture {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push("events");
    path.push(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {}", path.display(), e));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse fixture {}: {}", path.display(), e))
}

// ---------------------------------------------------------------------------
// In-memory server boot
// ---------------------------------------------------------------------------

/// Boot a minimal axum app exposing the WS events router with a fresh
/// in-memory `SqlxRepo` and `EventBus`. Mirrors `ws_replay.rs::boot` —
/// kept duplicated rather than abstracted because the test-harness shape
/// is what changes most often, and a shared helper would invite
/// pre-emptive parameterization.
async fn boot() -> (std::net::SocketAddr, Arc<SqlxRepo>, EventBus) {
    let events = EventBus::new();
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let state = AppState {
        repo: repo.clone(),
        events: events.clone(),
        daemon: Arc::new(DaemonClient::new_stub()),
        plugin: Arc::new(PluginHost::new(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
        )),
        codex: Arc::new(calm_server::state::CodexClient::new_stub()),
    };
    let app = axum::Router::new().merge(ws::router()).with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Tiny grace for the listener task to start accepting before we
    // open a WS — mirrors the wait in `ws_replay.rs`.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, repo, events)
}

/// Raw-insert fixture events into the `events` table via the public
/// `Repo::log_pure_event` path. That helper persists + broadcasts each
/// event with the commit-then-emit invariant intact, returning the
/// assigned `events.id` — exactly what we need to reconstruct a known
/// event log for replay assertions. The integration-test boundary
/// can't reach the `#[cfg(test)]`-gated raw `event_append_fixture`
/// helper, so we go through the public path; the net effect is the
/// same since we only seed events without any entity-table tendrils.
async fn raw_insert_fixture_events(repo: &SqlxRepo, bus: &EventBus, fixture: &Fixture) -> Vec<i64> {
    let mut out = Vec::with_capacity(fixture.events.len());
    for ev in &fixture.events {
        let event = Event::from_kind_and_payload(&ev.kind, ev.payload.clone())
            .unwrap_or_else(|e| panic!("reconstruct event {}: {}", ev.kind, e));
        let id = repo
            .log_pure_event(&ev.actor, None, bus, event)
            .await
            .expect("log_pure_event");
        out.push(id);
    }
    out
}

async fn recv_json<S>(ws: &mut S) -> serde_json::Value
where
    S: futures_util::Stream<
            Item = std::result::Result<TMessage, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timeout")
        .expect("ws closed under us")
        .expect("ws transport error");
    let t = match msg {
        TMessage::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {:?}", other),
    };
    serde_json::from_str(&t).expect("non-JSON frame")
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_wave_grid_layout_trace() {
    let fixture = load_fixture("wave-grid-layout-trace.events.json");
    let (addr, repo, bus) = boot().await;
    let ids = raw_insert_fixture_events(&repo, &bus, &fixture).await;
    assert_eq!(
        ids.len(),
        fixture.events.len(),
        "all fixture events inserted"
    );

    // Open WS, replay everything from id=0.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect ws");
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .expect("send sub");

    // Drain replay frames, capture the last `overlay.set` payload, and
    // stop when `_replay_complete` arrives. The fixture lays down the
    // layout overlay twice (initial + move); the *second* one is the
    // canonical post-replay state — that's what the assertions check.
    let mut received: Vec<(i64, String)> = Vec::new();
    let mut last_layout_payload: Option<serde_json::Value> = None;

    loop {
        let frame = recv_json(&mut ws).await;
        if frame["ev"] == "_replay_complete" {
            break;
        }
        let id = frame["_id"].as_i64().expect("_id present");
        let kind = frame["ev"].as_str().expect("ev present").to_string();
        if kind == "overlay.set"
            && frame["data"]["entity_kind"] == "view"
            && frame["data"]["kind"] == "layout"
        {
            last_layout_payload = Some(frame["data"]["payload"].clone());
        }
        received.push((id, kind));
    }

    // Every fixture event arrived, in id order, exactly once.
    assert_eq!(received.len(), fixture.events.len(), "frame count");
    for w in received.windows(2) {
        assert!(w[0].0 < w[1].0, "monotonic ids: {:?}", received);
    }
    for ((_id, kind), fix_ev) in received.iter().zip(fixture.events.iter()) {
        assert_eq!(kind, &fix_ev.kind, "frame kind matches fixture order");
    }

    // Last replayed event matches the fixture's `expected.last_event_kind`.
    let last_kind = &received.last().expect("at least one frame").1;
    assert_eq!(
        last_kind, &fixture.expected.last_event_kind,
        "last event kind"
    );

    // Final layout-overlay payload (after replaying both `overlay.set`
    // frames) carries the expected positions. The second `overlay.set`
    // in the fixture is the "move card_2" step; the assertion proves
    // upsert ordering survives replay.
    let last_layout = last_layout_payload.expect("layout overlay present in replay");
    let actual_positions = last_layout
        .get("positions")
        .and_then(|v| v.as_object())
        .expect("positions object")
        .clone();
    for (card_id, expected) in &fixture.expected.layout_positions {
        let actual = actual_positions
            .get(card_id)
            .unwrap_or_else(|| panic!("missing card_id {} in replayed layout", card_id));
        assert_eq!(
            actual, expected,
            "card_id {} position mismatch — replay diverged from fixture",
            card_id
        );
    }
    assert_eq!(
        actual_positions.len(),
        fixture.expected.layout_positions.len(),
        "positions cardinality must match — no extras",
    );
}

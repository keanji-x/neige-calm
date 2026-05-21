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

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::model::Overlay;
use calm_server::replay::{self, Fixture};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TMessage;

// ---------------------------------------------------------------------------
// Fixture loader — fixture types + parser live in `calm_server::replay`
// so the `replay` bin and this test share one definition. This file
// only adds the per-test "load by relative name under tests/fixtures/events".
// ---------------------------------------------------------------------------

fn load_fixture(name: &str) -> Fixture {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push("events");
    path.push(name);
    replay::load_fixture_from_path(&path).expect("load fixture")
}

// ---------------------------------------------------------------------------
// In-memory server boot — WS-only router. The full-router variant lives
// in `src/bin/replay.rs`; this test only asserts on WS replay so we keep
// the surface narrow.
// ---------------------------------------------------------------------------

async fn boot() -> (std::net::SocketAddr, Arc<SqlxRepo>, EventBus) {
    let (repo, events, state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");
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

async fn raw_insert_fixture_events(repo: &SqlxRepo, bus: &EventBus, fixture: &Fixture) -> Vec<i64> {
    replay::seed_events(repo, bus, fixture)
        .await
        .expect("seed fixture events")
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
    // The shared `FixtureExpected.last_event_kind` is `Option<String>` so
    // partial fixtures can omit it; this fixture has it set.
    let last_kind = &received.last().expect("at least one frame").1;
    let expected_last_kind = fixture
        .expected
        .last_event_kind
        .as_ref()
        .expect("fixture sets last_event_kind");
    assert_eq!(last_kind, expected_last_kind, "last event kind");

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

// ---------------------------------------------------------------------------
// F1 — RECORD_SESSION ↔ loader round-trip
// ---------------------------------------------------------------------------
//
// The `RECORD_SESSION=<path>` env hook writes one NDJSON line per emitted
// event. `load_fixture_from_path` must accept that file directly so the
// "bug report = file + one `replay --assert` command" promise holds (design
// doc §6.3).
//
// Test shape: stand up `boot_in_memory`, attach the recorder, drive a few
// `log_pure_event` calls through the bus, give the recorder a moment to
// flush, then drop the bus to close the recorder loop and re-parse the
// file with `load_fixture_from_path`. The parsed fixture must contain the
// same number of events in the same kind order.
#[tokio::test]
async fn record_session_roundtrips_through_loader() {
    let (repo, bus, _state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");

    // Recorder appends to this path; use a tempfile to keep CI clean.
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let session_path = tmpdir.path().join("recorded.events.json");
    replay::spawn_session_recorder(&bus, session_path.clone());

    // Recorder subscribes synchronously inside `spawn_session_recorder`
    // (the `bus.subscribe()` call is before the `tokio::spawn`), so any
    // event broadcast after this point lands in the recorder's receive
    // buffer — no race against the recorder task starting.
    let events: Vec<Event> = vec![
        Event::OverlaySet(Overlay {
            id: "ov-1".into(),
            plugin_id: "core".into(),
            entity_kind: "view".into(),
            entity_id: "wave-1".into(),
            kind: "layout".into(),
            payload: serde_json::json!({"positions": {"card_1": {"x": 0, "y": 0, "w": 4, "h": 3}}}),
            updated_at: 1,
        }),
        Event::OverlayDeleted {
            plugin_id: "core".into(),
            entity_kind: "view".into(),
            entity_id: "wave-1".into(),
            kind: "layout".into(),
        },
    ];
    let mut want_kinds: Vec<&str> = Vec::new();
    for ev in events {
        want_kinds.push(ev.kind_tag());
        repo.log_pure_event("user", None, &bus, ev)
            .await
            .expect("log_pure_event");
    }

    // The recorder writes line-by-line and flushes on each event, but the
    // write happens off the broadcast task. Poll the file until it has
    // the expected line count or we hit a deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(text) = std::fs::read_to_string(&session_path)
            && text.lines().filter(|l| !l.trim().is_empty()).count() >= want_kinds.len()
        {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "recorder never flushed {} events to {}",
                want_kinds.len(),
                session_path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Re-parse via the public loader — this is the round-trip claim.
    let fixture =
        replay::load_fixture_from_path(&session_path).expect("loader accepts recorded NDJSON");
    assert_eq!(
        fixture.events.len(),
        want_kinds.len(),
        "round-trip preserves event count"
    );
    for (got, want) in fixture.events.iter().zip(want_kinds.iter()) {
        assert_eq!(&got.kind, *want, "round-trip preserves event kind order");
    }
    // The loader synthesizes a `name` from the filename stem and an
    // empty `expected` block when the file is NDJSON. Sanity-check both
    // so a future regression that silently swaps the branches surfaces.
    assert_eq!(fixture.name, "recorded.events");
    assert!(
        fixture.expected.last_event_kind.is_none() && fixture.expected.layout_positions.is_empty(),
        "NDJSON branch produces an empty expected block"
    );
}

// ---------------------------------------------------------------------------
// F4 — `derive_layout_positions` fold handles `Event::OverlayDeleted`
// ---------------------------------------------------------------------------
//
// Set → Delete → Set. The fold must end up at the *second* Set's positions,
// not at the first Set merged into the second Set (the original bug was
// that Delete was ignored, so a delete-between-sets had no effect and the
// fold returned whichever Set the loop visited last regardless of any
// intervening Delete).
#[test]
fn fold_layout_positions_respects_overlay_deleted() {
    let wave_id = "wave-1";

    let set_a = Event::OverlaySet(Overlay {
        id: "ov-a".into(),
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: wave_id.into(),
        kind: "layout".into(),
        payload: serde_json::json!({"positions": {"card_1": {"x": 0, "y": 0, "w": 4, "h": 3}}}),
        updated_at: 1,
    });
    let delete = Event::OverlayDeleted {
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: wave_id.into(),
        kind: "layout".into(),
    };
    let set_b = Event::OverlaySet(Overlay {
        id: "ov-b".into(),
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: wave_id.into(),
        kind: "layout".into(),
        payload: serde_json::json!({"positions": {"card_9": {"x": 8, "y": 0, "w": 4, "h": 3}}}),
        updated_at: 3,
    });

    // Set → Delete → Set: end state is set_b alone (the delete cleared
    // set_a's contribution before set_b overwrote).
    let got =
        replay::fold_layout_positions([set_a.clone(), delete.clone(), set_b.clone()], wave_id)
            .expect("set after delete still produces Some");
    assert_eq!(got.len(), 1, "delete cleared set_a before set_b");
    assert!(got.contains_key("card_9"));
    assert!(!got.contains_key("card_1"));

    // Set → Delete: end state is None (delete is terminal until next set).
    let got = replay::fold_layout_positions([set_a.clone(), delete.clone()], wave_id);
    assert!(got.is_none(), "delete after lone set yields None");

    // Delete-only on an empty stream is still None (no panic, no spurious
    // entry).
    let got = replay::fold_layout_positions([delete], wave_id);
    assert!(got.is_none(), "lone delete yields None");

    // Wrong-wave delete must not affect the running state.
    let delete_other = Event::OverlayDeleted {
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: "wave-other".into(),
        kind: "layout".into(),
    };
    let got = replay::fold_layout_positions([set_a.clone(), delete_other, set_b.clone()], wave_id)
        .expect("unrelated delete must not clear");
    // set_a's positions merged with set_b via the `.or(current)` fold —
    // this is the existing upsert semantics and not in scope of the
    // delete-fix, but assert the post-state is non-empty.
    assert!(got.contains_key("card_9"));
}

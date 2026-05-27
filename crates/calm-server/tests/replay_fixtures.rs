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
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
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
        repo.log_pure_event(
            ActorId::User,
            EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::wave_cove_cache::WaveCoveCache::new(),
            ev,
        )
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

// ---------------------------------------------------------------------------
// `reset_from_fixture` — wipe + reseed contract
// ---------------------------------------------------------------------------
//
// Issue #56 followup: the `POST /dev/reset` endpoint in `bin/replay.rs`
// calls into `reset_from_fixture` to give the Playwright `a11y` project
// a hermetic per-test starting state. The test below locks the contract:
//
//   1. Seed a fixture once; verify the event log has `N` rows.
//   2. Mutate the repo via the eventized write path (drop in a new
//      `log_pure_event` for an `overlay.set`) so the log grows past
//      what the fixture seeded.
//   3. Call `reset_from_fixture`; verify the log is back to exactly
//      the fixture's events, in fixture order, and the highest event
//      id is back to `N` (the `sqlite_sequence` reset path).
#[tokio::test]
async fn reset_from_fixture_wipes_and_reseeds() {
    let fixture = load_fixture("wave-grid-layout-trace.events.json");
    let (repo, bus, _state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");

    let initial_ids = replay::seed_events(&repo, &bus, &fixture)
        .await
        .expect("initial seed");
    let n = fixture.events.len() as i64;
    assert_eq!(initial_ids.len() as i64, n);
    assert_eq!(
        *initial_ids.last().expect("non-empty"),
        n,
        "initial seed assigns ids 1..=N because sqlite_sequence starts fresh"
    );

    // Mutate via the eventized write path: drop in one extra
    // `overlay.set` so the event log grows past the fixture tip.
    let extra = Event::OverlaySet(Overlay {
        id: "ov-extra".into(),
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: "wave-extra".into(),
        kind: "layout".into(),
        payload: serde_json::json!({"positions": {}}),
        updated_at: 99,
    });
    let extra_id = repo
        .log_pure_event(
            ActorId::User,
            EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::wave_cove_cache::WaveCoveCache::new(),
            extra,
        )
        .await
        .expect("log extra event");
    assert_eq!(extra_id, n + 1, "extra event sits at id=N+1");

    // Reset: drop everything, reseed from the fixture, assert ids
    // re-start at 1 and the log carries exactly the fixture again.
    let reseeded = replay::reset_from_fixture(&repo, &bus, &fixture)
        .await
        .expect("reset succeeds");
    assert_eq!(
        reseeded.len() as i64,
        n,
        "reseeded event count matches fixture"
    );
    assert_eq!(
        *reseeded.first().expect("non-empty"),
        1,
        "reset wipes sqlite_sequence — first event id is 1"
    );
    assert_eq!(
        *reseeded.last().expect("non-empty"),
        n,
        "reset wipes sqlite_sequence — last event id is N (no carry-over from the extra)"
    );

    // Verify the persisted log shape matches the fixture.
    let log = repo
        .events_since(0, None)
        .await
        .expect("events_since after reset");
    assert_eq!(log.len() as i64, n, "log has only the reseeded events");
    for ((_id, _ver, _scope, ev), fix_ev) in log.iter().zip(fixture.events.iter()) {
        assert_eq!(
            ev.kind_tag(),
            fix_ev.kind,
            "reseeded log preserves fixture event order"
        );
    }
}

// ---------------------------------------------------------------------------
// Issue #199 — schemaVersion forward-compat: both read paths drop future.
// ---------------------------------------------------------------------------
//
// The contract this test pins is the kernel's *acceptance criterion*
// from issue #199: an unsupported future-`schemaVersion` overlay
// payload must NOT be silently consumed by the frontend. The two
// client-facing read paths for overlay state are:
//
//   1. **WS `/api/events` replay** — historically streamed the raw
//      persisted envelope verbatim. PR #214 added a read-side guard on
//      `GET /api/overlays` + `GET /api/waves/{id}`; PR #220 closed the
//      leak on the third surface by extending the same per-row
//      predicate (`crate::validation::should_skip_event_for_overlay_version`)
//      to both the live-broadcast and the cursor-replay legs of
//      `ws::events`. So replay now drops `Event::OverlaySet` rows whose
//      payload `schemaVersion` exceeds what this binary supports for
//      the kind — advancing `last_id` past dropped rows so a client
//      never re-polls them on reconnect.
//
//   2. **REST `/api/overlays`** — same guard, applied row-by-row by
//      `routes::overlays::filter_unsupported_overlay_versions` (now a
//      thin wrapper around the shared `should_skip_overlay`).
//
// Why this *strengthens* the #199 acceptance: previously the contract
// was asymmetric (replay transparent, REST strict), which left the
// frontend's resilience-on-replay path on the hook for handling
// unknown-version envelopes. Post-#220 both read paths agree — the
// frontend literally cannot observe a v999 overlay over either surface
// — so "unsupported future payload is not silently consumed" is
// enforced by the kernel on every client-facing read, not just the
// REST audit list.
//
// The fixture (`schema_forward_compat.events.json`) carries both
// shapes back-to-back. Neither WS replay nor `GET /api/overlays`
// surfaces the v999 row; both surface the v1 one.
//
// Why one combined test instead of two: the contract is "future
// versions are dropped on every kernel→client read path" — splitting
// the surfaces would let a regression on one path still tick the
// other green. Composing them in one flow pins the invariant across
// both paths the frontend can observe.

#[tokio::test]
async fn schema_version_future_dropped_on_both_replay_and_rest_read() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use calm_server::model::NewOverlay;
    use calm_server::validation::{
        OVERLAY_STATUS_SCHEMA_VERSION, max_supported_overlay_schema_version, payload_schema_version,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let fixture = load_fixture("schema_forward_compat.events.json");

    // Sanity: helper reads `1` for the missing-field shape and `999`
    // for the future-shape. These are the inputs to the read-side
    // guard, so locking them here surfaces a payload_schema_version()
    // refactor that drifts away from "absent → 1".
    let v1_payload = &fixture.events[2].payload["payload"];
    let v999_payload = &fixture.events[3].payload["payload"];
    assert_eq!(
        payload_schema_version(v1_payload),
        1,
        "missing schemaVersion must read as 1 (backward-compat default)"
    );
    assert_eq!(
        payload_schema_version(v999_payload),
        999,
        "explicit schemaVersion=999 must round-trip through the helper"
    );
    assert_eq!(
        max_supported_overlay_schema_version("status"),
        Some(OVERLAY_STATUS_SCHEMA_VERSION),
        "the kernel's status overlay support ceiling drives what the read guard accepts"
    );

    // ---- Replay arm: seed both events, drain over WS, assert ONLY
    //                  the v1 overlay.set frame surfaces. Post-#220 the
    //                  WS replay path runs each row through
    //                  `should_skip_event_for_overlay_version` and
    //                  drops `Event::OverlaySet` envelopes whose
    //                  payload `schemaVersion` exceeds the kernel's
    //                  ceiling for the kind. Pre-#220 the regression
    //                  was the opposite shape: the wire kept the v999
    //                  envelope verbatim. We now lock in the stricter
    //                  invariant on this surface too.
    let (addr, repo, bus) = boot().await;
    let ids = raw_insert_fixture_events(&repo, &bus, &fixture).await;
    assert_eq!(ids.len(), fixture.events.len(), "seed inserted all events");

    // Pre-check the events table directly to pin the failure layer if
    // the WS assertion below misfires. The WS replay path is
    // `events_since(0) → filter via should_skip_event_for_overlay_version →
    // render_envelope → tx.send`; if the DB-side has both overlay.set
    // rows but the WS frame count diverges from the expected
    // post-filter count, the regression is unambiguously in
    // `ws::events::run_replay` (either the filter is missing or it's
    // over-filtering).
    let db_rows = repo
        .events_since(0, None)
        .await
        .expect("events_since after seed");
    assert_eq!(
        db_rows.len(),
        fixture.events.len(),
        "events_since(0) returns every seeded row; seed-layer drift"
    );
    let db_overlay_count = db_rows
        .iter()
        .filter(|(_, _, _, ev)| ev.kind_tag() == "overlay.set")
        .count();
    assert_eq!(
        db_overlay_count, 2,
        "both overlay.set rows are persisted in the events log; the v999 row \
         is expected to be dropped on the WS replay path (#220), not on the seed."
    );

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect ws");
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .expect("send sub");

    // Drain every frame in the replay window into `all_frames` so on
    // failure we can print exactly what the server sent. The contract
    // under test (post-#220) is "the replay window contains exactly
    // the v1 overlay.set envelope — the v999 row is filtered server-
    // side — terminated by `_replay_complete`". The `recv_json` helper
    // bounds each per-frame wait at 2s, so a missing trailing frame
    // surfaces as a timeout panic with the frames-so-far visible in
    // `all_frames` via the assertion message.
    let mut all_frames: Vec<serde_json::Value> = Vec::new();
    let mut overlay_frames: Vec<serde_json::Value> = Vec::new();
    loop {
        let frame = recv_json(&mut ws).await;
        all_frames.push(frame.clone());
        if frame["ev"] == "overlay.set" {
            overlay_frames.push(frame.clone());
        }
        if frame["ev"] == "_replay_complete" {
            break;
        }
    }
    assert_eq!(
        overlay_frames.len(),
        1,
        "post-#220: WS replay drops the v999 overlay.set; only the v1 frame \
         (ov-v1) should reach the client. Got {} overlay.set frames in the \
         replay window. All frames: {:#?}",
        overlay_frames.len(),
        all_frames,
    );
    let v1_frame = overlay_frames
        .iter()
        .find(|f| f["data"]["id"] == "ov-v1")
        .expect("v1 overlay frame present in replay");
    assert!(
        overlay_frames.iter().all(|f| f["data"]["id"] != "ov-v999"),
        "v999 overlay frame must NOT appear in replay — post-#220 the WS \
         path filters unsupported future-version `Event::OverlaySet` rows. \
         All frames: {:#?}",
        all_frames,
    );
    // And the v1 row genuinely has no schemaVersion key — guards
    // against a regression that secretly coerces missing → 1 on the
    // wire.
    assert!(
        v1_frame["data"]["payload"].get("schemaVersion").is_none(),
        "v1 frame retains its missing-field shape (not coerced to {{schemaVersion: 1}})"
    );

    // ---- Read-side arm: the WS arm above pinned that the replay path
    // drops the v999 envelope before it reaches the client. To
    // exercise the REST read guard on the *other* client-facing read
    // surface, we need the same shapes to also live in the OVERLAYS
    // table. The fixture's `overlay.set` events only write to the
    // events log, not the overlays table (the route handler does the
    // upsert separately). So we mirror them via the repo's
    // `overlay_upsert` to set up the read-side test bed. The write
    // path's `validate_overlay_payload` would refuse the v999
    // payload, which is exactly why we go through the repo trait
    // directly — to simulate a "row written by a future kernel"
    // scenario.
    // Use two distinct overlay `kind`s so they coexist (the unique
    // key on the overlays table is (plugin_id, entity_kind,
    // entity_id, kind) — upserting the same kind would replace,
    // not co-store).
    //
    //   * `status` v1 — the legacy row, no schemaVersion field
    //   * `progress` v999 — the future-kernel row whose schemaVersion
    //     exceeds the kernel's ceiling (both kinds cap at v1 today)
    repo.overlay_upsert(NewOverlay {
        plugin_id: "core".into(),
        entity_kind: "wave".into(),
        entity_id: "wave-fwd".into(),
        kind: "status".into(),
        payload: serde_json::json!({"state": "ok"}),
    })
    .await
    .expect("upsert v1 status overlay (no schemaVersion)");
    repo.overlay_upsert(NewOverlay {
        plugin_id: "core".into(),
        entity_kind: "wave".into(),
        entity_id: "wave-fwd".into(),
        kind: "progress".into(),
        // The route's write-side validator (`validate_overlay_payload`)
        // would reject this; going through `repo.overlay_upsert`
        // bypasses the route layer on purpose — we're simulating "row
        // landed via a future kernel binary writing the same DB" so
        // the read-side guard's drop path actually has something to
        // drop.
        payload: serde_json::json!({
            "value": 0.5,
            "schemaVersion": 999,
            "fromFuture": "yes",
        }),
    })
    .await
    .expect("upsert v999 progress overlay (future-kernel simulation)");

    // Verify the raw repo returns BOTH rows — the filter is a route-
    // layer concern, not a repo concern.
    let raw = repo
        .overlays_for("wave", "wave-fwd")
        .await
        .expect("repo overlays_for");
    assert_eq!(
        raw.len(),
        2,
        "repo.overlays_for returns the raw row count (filter happens at the route layer)"
    );

    // Mount the full router with the AppState the route handlers need.
    // We can't reuse `boot()` here — it stands up a WS-only router.
    // Build the full stack from `boot_in_memory` (re-using the same
    // repo so the data we inserted above is visible).
    let app = build_full_app(repo.clone(), bus.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/overlays?entity_kind=wave&entity_id=wave-fwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("decode list");
    assert_eq!(
        listed.len(),
        1,
        "read-side guard must drop the v999 row (kernel-owned `progress` kind, future version); \
         got {} rows: {:?}",
        listed.len(),
        listed,
    );
    assert_eq!(
        listed[0]["kind"], "status",
        "the surviving row is the v1-shaped status overlay (no schemaVersion field)"
    );
    assert!(
        listed[0]["payload"].get("schemaVersion").is_none(),
        "the v1 row reaches the client without an added schemaVersion field"
    );
}

/// Build the full kernel HTTP router (REST + WS + plugins + …) against
/// an existing in-memory `SqlxRepo` and `EventBus`. Mirrors what
/// `boot_in_memory()` does internally but with the repo + bus the
/// caller already has — so the data inserted via the repo above is
/// visible to the route handlers.
fn build_full_app(repo: Arc<calm_server::db::sqlite::SqlxRepo>, events: EventBus) -> axum::Router {
    use calm_server::card_role_cache::CardRoleCache;
    use calm_server::plugin_host::{PluginHost, PluginRegistry};
    use calm_server::routes;
    use calm_server::state::{AppState, CodexClient, DaemonClient};

    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        std::path::PathBuf::new(),
        std::env::temp_dir().join("calm-plugins-data-schema-fwd"),
        Vec::new(),
        events.clone(),
        card_role_cache.clone(),
        wave_cove_cache.clone(),
    ));
    let state = AppState::from_parts(
        repo,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(wave_cove_cache.clone()),
    );
    routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

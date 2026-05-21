//! Sync engine phase 2 (Scope D) WS replay protocol — end-to-end tests.
//!
//! These exercise the cursor/since side of `ws::events::handle`:
//!
//!   1. `subscribe_with_since_zero_replays_all` — seeded history is fully
//!      streamed when the client opens with `since = 0`.
//!   2. `subscribe_with_since_mid_replays_only_newer` — only events with
//!      `id > since` arrive (the regular cursor-resume case).
//!   3. `subscribe_without_since_only_live` — backward-compat: omit
//!      `since`, get pre-Scope-D behavior (live only, no replay).
//!   4. `replay_complete_terminator_is_sent` — confirms the
//!      `_replay_complete` synthetic frame lands after the historical
//!      window, even when zero rows match.
//!   5. `replay_then_live_no_drop_no_dupe` — crown jewel: open with `since
//!      = mid`, the in-memory bus fires a *new* write during the replay
//!      window, both replay tail and live event arrive exactly once in
//!      strict id order.
//!   6. `client_at_cursor_too_old_gets_snapshot_required` — simulate
//!      retention by deleting early rows and assert the
//!      `_snapshot_required` frame.
//!
//! The integration harness mirrors `tests/ws_events.rs` (boot AppState,
//! spawn axum, drive `tokio_tungstenite`). The seed path runs writes
//! through `write_with_event_typed` so each row hits both the events
//! table (for the replay query to find) and the broadcast bus (which is
//! a no-op until the WS handler subscribes, but harmless).

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::{SqlxRepo, cove_create_tx};
use calm_server::db::{Repo, write_with_event_typed};
use calm_server::event::{Event, EventBus};
use calm_server::model::NewCove;
use calm_server::plugin_host::PluginHost;
use calm_server::state::{AppState, DaemonClient};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TMessage;

/// Boot a minimal axum app with the WS events router + a fresh in-memory
/// SqlxRepo, and return the bound address plus the concrete SqlxRepo /
/// EventBus so tests can seed events directly.
async fn boot() -> (std::net::SocketAddr, Arc<SqlxRepo>, EventBus) {
    let events = EventBus::new();
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new(
            Arc::new(calm_server::plugin_host::PluginRegistry::empty()),
            repo.clone(),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
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

/// Seed a small linear history (3 cove.updated rows). Returns the assigned
/// `events.id`s in append order, plus the assigned cove IDs (since
/// `cove_create_tx` generates ids server-side, we don't get to choose
/// them — the tests just compare to whatever came back).
///
/// The events table is what the WS replay path actually consumes; bus
/// emissions during seed are harmless (no subscriber yet).
async fn seed_three(repo: &SqlxRepo, bus: &EventBus, names: [&str; 3]) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    for name in names {
        let p = NewCove {
            name: name.to_string(),
            color: "#000".into(),
            sort: None,
        };
        let (cove, event_id) =
            write_with_event_typed(repo as &dyn Repo, "user", None, bus, move |tx| {
                Box::pin(async move {
                    let c = cove_create_tx(tx, p).await?;
                    Ok((c.clone(), Event::CoveUpdated(c)))
                })
            })
            .await
            .unwrap();
        out.push((event_id, cove.id));
    }
    out
}

/// Read one text frame off the socket, decoded as `serde_json::Value`.
/// Panics on timeout, close, or non-text — every Scope D test expects text.
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
// 1. since=0 replays all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_with_since_zero_replays_all() {
    let (addr, repo, bus) = boot().await;
    let seeded = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Firehose subscription with since=0 — replay everything in the log.
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    // Three replay frames in id order, then `_replay_complete`.
    for (event_id, cove_id) in seeded.iter() {
        let v = recv_json(&mut ws).await;
        assert_eq!(v["_id"], *event_id, "frame ids in order");
        assert_eq!(v["ev"], "cove.updated");
        assert_eq!(v["data"]["id"], *cove_id);
    }
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(done["_id"], seeded.last().unwrap().0);
}

// ---------------------------------------------------------------------------
// 2. since=mid replays only newer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_with_since_mid_replays_only_newer() {
    let (addr, repo, bus) = boot().await;
    let seeded = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;
    let mid = seeded[0].0; // resume after the first event

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(format!(
        r#"{{"sub":["*"], "since": {}}}"#,
        mid
    )))
    .await
    .unwrap();

    // Expect the 2nd and 3rd seeded coves then `_replay_complete`. The
    // first cove must not appear — its id is at-or-below `since`.
    let v = recv_json(&mut ws).await;
    assert_eq!(v["data"]["id"], seeded[1].1);
    let v = recv_json(&mut ws).await;
    assert_eq!(v["data"]["id"], seeded[2].1);
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(done["_id"], seeded[2].0);
}

// ---------------------------------------------------------------------------
// 3. omit `since` — backward compat (live only, no replay)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_without_since_only_live() {
    let (addr, repo, bus) = boot().await;
    // Seed pre-connection history that a live-only sub must NOT see.
    let _ = seed_three(&repo, &bus, ["before-1", "before-2", "before-3"]).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Pre-Scope-D message shape (no `since` field).
    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // No `_replay_complete`, no historical frames. Confirm by emitting a
    // brand-new live event and asserting that's the first thing the client
    // sees. (`bus.emit` is the synthetic test-only emit that produces
    // `id = 0`; the assertion below intentionally accepts that as a
    // canary — the client never advances its cursor off these frames,
    // which is the right behavior for unpersisted broadcasts.)
    bus.emit(Event::CoveUpdated(calm_server::model::Cove {
        id: "live-only".into(),
        name: "n".into(),
        color: "#fff".into(),
        sort: 0.0,
        created_at: 0,
        updated_at: 0,
    }));

    let v = recv_json(&mut ws).await;
    assert_eq!(v["ev"], "cove.updated");
    assert_eq!(v["data"]["id"], "live-only");
    assert_eq!(v["_id"], 0);
}

// ---------------------------------------------------------------------------
// 4. _replay_complete terminator always fires
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_complete_terminator_is_sent_even_when_zero_rows() {
    let (addr, _repo, _bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    // Empty events table + since=0 → zero replay rows, but the terminator
    // still arrives. This is the cue the client uses to drop its
    // "reconnecting" banner and run a defensive batch invalidate.
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    // No rows → cursor stays at `since` (=0). The client will keep its
    // own `lastEventId` and advance it from live frames.
    assert_eq!(done["_id"], 0);
}

// ---------------------------------------------------------------------------
// 5. Crown jewel: replay-then-live with no drop / no dupe
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_then_live_no_drop_no_dupe() {
    let (addr, repo, bus) = boot().await;
    let seeded = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Resume after the first seeded event. The handler subscribes to the
    // live bus BEFORE running the events_since query (design §2.2); we
    // race a live write against the replay to confirm dedupe + no drop.
    let since = seeded[0].0;

    // Fire `{sub, since}` — kicks off the replay path inside the handler.
    ws.send(TMessage::Text(format!(
        r#"{{"sub":["*"], "since": {}}}"#,
        since
    )))
    .await
    .unwrap();
    // Small breath so the handler enters the replay branch and registers
    // its bus subscription. Without this, the live emit below can land
    // before the handler called `state.events.subscribe()`, which is a
    // separate problem the connect handshake guards against (the handler
    // grabs `state.events.subscribe()` *before* it reads any client frame
    // — see `handle()` in src/ws/events.rs — so this sleep is paranoia
    // rather than correctness-critical).
    tokio::time::sleep(Duration::from_millis(20)).await;

    // While the replay path is mid-stream (or has just finished), fire a
    // brand-new write through the write_with_event path so it's both
    // persisted (in the events table) and broadcast (on the bus).
    let new_cove = NewCove {
        name: "live-during-replay".into(),
        color: "#000".into(),
        sort: None,
    };
    let (_c, live_id) =
        write_with_event_typed(repo.as_ref() as &dyn Repo, "user", None, &bus, move |tx| {
            Box::pin(async move {
                let c = cove_create_tx(tx, new_cove).await?;
                Ok((c.clone(), Event::CoveUpdated(c)))
            })
        })
        .await
        .unwrap();
    assert!(
        live_id > seeded[2].0,
        "live event must come after seeded ids"
    );

    // Drain everything until we've seen `_replay_complete` AND the live
    // frame. Either of two orderings is acceptable:
    //   - the live event lands during the replay SQL window — then
    //     events_since returns it as part of the replay tail and the
    //     broadcast-dedup drops the duplicate when it arrives over the
    //     bus.
    //   - the live event lands after the SELECT — then it arrives via
    //     the live forward branch after `_replay_complete`.
    // Either way: every id appears exactly once, no gaps, monotonic.
    let mut seen: Vec<i64> = Vec::new();
    let mut got_complete = false;
    let mut got_live = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !(got_complete && got_live) && std::time::Instant::now() < deadline {
        let v = match timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                serde_json::from_str::<serde_json::Value>(&t).expect("json")
            }
            other => panic!("unexpected ws message: {:?}", other),
        };
        if v["ev"] == "_replay_complete" {
            got_complete = true;
            continue;
        }
        let id = v["_id"].as_i64().expect("_id present");
        seen.push(id);
        if id == live_id {
            got_live = true;
        }
    }
    assert!(got_complete, "_replay_complete must arrive");
    assert!(got_live, "live event must arrive after replay window");

    // Strict monotonic, no duplicates.
    for w in seen.windows(2) {
        assert!(
            w[0] < w[1],
            "ids must arrive in strictly ascending order, got {:?}",
            seen
        );
    }
    let unique: std::collections::BTreeSet<i64> = seen.iter().copied().collect();
    assert_eq!(
        unique.len(),
        seen.len(),
        "each event must be delivered exactly once"
    );
    // The full content set is exactly { seeded[1], seeded[2], live }; the
    // first seeded id must NOT appear (cursor was past it).
    assert!(
        !seen.contains(&seeded[0].0),
        "first seed already past cursor"
    );
    assert!(seen.contains(&seeded[1].0));
    assert!(seen.contains(&seeded[2].0));
    assert!(seen.contains(&live_id));
}

// ---------------------------------------------------------------------------
// 6. Snapshot required when cursor predates retention horizon
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_at_cursor_too_old_gets_snapshot_required() {
    let (addr, repo, bus) = boot().await;
    let seeded = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;

    // Simulate retention pruning by removing the earliest event(s).
    sqlx::query("DELETE FROM events WHERE id IN (?1, ?2)")
        .bind(seeded[0].0)
        .bind(seeded[1].0)
        .execute(repo.pool())
        .await
        .unwrap();

    // Client resumes from a cursor below the surviving earliest_id. They
    // can't be backfilled contiguously — they need a snapshot.
    // earliest_id is now seeded[2].0; `since = 1` is well below that and
    // the gap check (`since < earliest - 1`) triggers the control frame.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 1}"#.to_string()))
        .await
        .unwrap();

    let frame = recv_json(&mut ws).await;
    assert_eq!(frame["ev"], "_snapshot_required");
    assert_eq!(frame["_id"], seeded[2].0);
    assert_eq!(frame["data"]["earliest_id"], seeded[2].0);

    // Connection closes shortly after — tolerate either an explicit close
    // frame, a transport-level closure, or a timeout falling through.
    let _ = timeout(Duration::from_millis(500), ws.next()).await;
}

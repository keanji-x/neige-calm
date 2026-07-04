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

use calm_server::db::sqlite::{SqlxRepo, cove_create_tx, overlay_upsert_tx};
use calm_server::db::{Repo, RepoEventWrite, write_with_event_typed};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::events_prune::{EventsRetentionPolicy, prune_events_once};
use calm_server::ids::{ActorId, CardId};
use calm_server::model::{NewCove, NewOverlay};
use calm_server::plugin_host::PluginHost;
use calm_server::state::{AppState, DaemonClient};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
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
        Arc::new(PluginHost::new_full(
            Arc::new(calm_server::plugin_host::PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events.clone(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
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
        let (cove, event_id) = write_with_event_typed(
            repo as &dyn Repo,
            ActorId::User,
            EventScope::System,
            None,
            bus,
            &calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
            move |tx| {
                Box::pin(async move {
                    let c = cove_create_tx(tx, p).await?;
                    Ok((c.clone(), Event::CoveUpdated(c)))
                })
            },
        )
        .await
        .unwrap();
        out.push((event_id, cove.id.to_string()));
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
    bus.emit(
        ActorId::User,
        Event::CoveUpdated(calm_server::model::Cove {
            id: "live-only".into(),
            name: "n".into(),
            color: "#fff".into(),
            sort: 0.0,
            kind: calm_server::model::CoveKind::User,
            created_at: 0,
            updated_at: 0,
        }),
    );

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
    let (_c, live_id) = write_with_event_typed(
        repo.as_ref() as &dyn Repo,
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let c = cove_create_tx(tx, new_cove).await?;
                Ok((c.clone(), Event::CoveUpdated(c)))
            })
        },
    )
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

// ---------------------------------------------------------------------------
// 7. Tier A read-side guard, REPLAY surface (issue #198 concern 4, PR #214
//    follow-up). The events table can hold an `Event::OverlaySet` row whose
//    `schemaVersion` was written by a newer kernel binary against the same DB
//    (downgrade or split-deploy scenario). PR #214 filtered such rows out of
//    `/api/overlays` and `GET /api/waves/{id}`; this assertion locks the
//    invariant on the replay leg of `/api/events` too.
// ---------------------------------------------------------------------------

/// Seed two `Event::OverlaySet` rows directly through `write_with_event_typed`
/// (bypass route-layer `validate_overlay_payload` so the future-version row
/// actually lands — same `raw_repo()`-equivalent bypass pattern PR #214 used
/// for its HTTP read-side test). Returns the assigned event ids in seed
/// order: `[supported_event_id, future_event_id]`.
async fn seed_supported_and_future_overlays(repo: &SqlxRepo, bus: &EventBus) -> (i64, i64) {
    // Supported: status overlay at the current schemaVersion.
    let supported = NewOverlay {
        plugin_id: "p1".into(),
        entity_kind: "wave".into(),
        entity_id: "w-1".into(),
        kind: "status".into(),
        payload: json!({ "schemaVersion": 1, "state": "running" }),
    };
    let (_o, supported_id) = write_with_event_typed(
        repo as &dyn Repo,
        ActorId::User,
        EventScope::System,
        None,
        bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let o = overlay_upsert_tx(tx, supported).await?;
                Ok((o.clone(), Event::OverlaySet(o)))
            })
        },
    )
    .await
    .unwrap();

    // Future: same kind, schemaVersion above the current max. Inserted via
    // the same code path so both the overlay row and its event row land in
    // the same transactional unit the replay path will read.
    let future = NewOverlay {
        plugin_id: "p1".into(),
        entity_kind: "wave".into(),
        entity_id: "w-1".into(),
        kind: "status".into(),
        payload: json!({ "schemaVersion": 999, "state": "from-future" }),
    };
    let (_o, future_id) = write_with_event_typed(
        repo as &dyn Repo,
        ActorId::User,
        EventScope::System,
        None,
        bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let o = overlay_upsert_tx(tx, future).await?;
                Ok((o.clone(), Event::OverlaySet(o)))
            })
        },
    )
    .await
    .unwrap();

    (supported_id, future_id)
}

#[tokio::test]
async fn replay_skips_future_schema_version_overlay_set() {
    let (addr, repo, bus) = boot().await;
    let (supported_id, future_id) = seed_supported_and_future_overlays(&repo, &bus).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // since=0 + firehose: every persisted event is in scope of the replay.
    // The future-version row must NOT make it onto the wire, but `last_id`
    // (carried in `_replay_complete._id`) must still advance to the future
    // row's id — the read-side guard drops the frame but advances the
    // cursor so the client never re-polls it.
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    // First frame: the supported overlay (older id). The overlay's own
    // `data.id` is a server-side nanoid we can't predict, so we assert on
    // the kernel-stamped fields instead.
    let v = recv_json(&mut ws).await;
    assert_eq!(v["_id"], supported_id);
    assert_eq!(v["ev"], "overlay.set");
    assert_eq!(v["data"]["kind"], "status");
    assert_eq!(v["data"]["payload"]["state"], "running");
    assert_eq!(v["data"]["payload"]["schemaVersion"], 1);

    // Next frame: `_replay_complete`. The future-version overlay must
    // NOT appear between the supported row and the terminator. The
    // terminator's `_id` advances to the future row's id even though
    // its payload was dropped — confirms the cursor invariant.
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(
        done["_id"], future_id,
        "cursor must advance past the dropped row so the client never re-polls it"
    );
}

// ---------------------------------------------------------------------------
// 8. Issue #290 — after a `/dev/reset` reseed (events wiped + `sqlite_sequence`
//    wiped → reseeded events restart at id=1), a fresh WS subscription's
//    `_replay_complete._id` reflects the SERVER'S NEW LOG TIP, not 0 and not
//    the pre-reset high-water mark. This is the server-side invariant the
//    client-side reset detection in `web/src/api/events.ts` relies on:
//    without it, a stale client cursor (e.g. id=3 from a pre-reset session)
//    would see the post-reset terminator carry `_id = 3` (the in-window
//    high-water from the empty `since=3` SELECT) and never trigger the
//    "server regressed" branch.
//
// Pre-PR-303 behavior: `_replay_complete._id` was the in-window high-water
// (= `since` when no rows matched). This test would have failed there with
// `_id = since = 0` instead of the post-reset `MAX(id) = 1`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_complete_id_reflects_server_tip_after_reset() {
    let (addr, repo, bus) = boot().await;

    // Pre-reset: seed three events, then drop in two more so the tip is
    // id=5. We need pre-reset tip strictly greater than post-reset tip so
    // the regression check below ("post-reset tip below pre-reset tip")
    // is meaningful — the post-reset reseed only writes two rows.
    let seeded = seed_three(&repo, &bus, ["pre-1", "pre-2", "pre-3"]).await;
    let extra1 = repo
        .log_pure_event(
            calm_server::ids::ActorId::User,
            calm_server::event::EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::wave_cove_cache::WaveCoveCache::new(),
            Event::CoveUpdated(calm_server::model::Cove {
                id: "pre-4".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::CoveKind::User,
                created_at: 0,
                updated_at: 0,
            }),
        )
        .await
        .unwrap();
    let extra2 = repo
        .log_pure_event(
            calm_server::ids::ActorId::User,
            calm_server::event::EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::wave_cove_cache::WaveCoveCache::new(),
            Event::CoveUpdated(calm_server::model::Cove {
                id: "pre-5".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::CoveKind::User,
                created_at: 0,
                updated_at: 0,
            }),
        )
        .await
        .unwrap();
    assert_eq!(extra1, seeded[2].0 + 1);
    assert_eq!(extra2, extra1 + 1);
    let pre_reset_tip = extra2;
    {
        let url = format!("ws://{}/api/events", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
            .await
            .unwrap();
        // Drain replay frames until we hit the terminator.
        loop {
            let v = recv_json(&mut ws).await;
            if v["ev"] == "_replay_complete" {
                assert_eq!(
                    v["_id"], pre_reset_tip,
                    "pre-reset _replay_complete._id matches MAX(id) of seeded events"
                );
                break;
            }
        }
        // Drop the socket; the next subscription is FRESH and will see
        // the post-reset state.
    }

    // Simulate `replay::reset_from_fixture`'s structural wipe: drop every
    // domain row + the event log + `sqlite_sequence` so AUTOINCREMENT
    // restarts at 1. We bypass the high-level helper because this test
    // doesn't have a fixture wired up — the invariant under test is on
    // the `events_latest_id()` + WS terminator path, not on fixture
    // reseed semantics (which `replay_fixtures::reset_from_fixture_wipes_and_reseeds`
    // already covers).
    {
        let pool = repo.pool();
        let mut tx = pool.begin().await.unwrap();
        for stmt in [
            "DELETE FROM events",
            "DELETE FROM overlays",
            "DELETE FROM terminals",
            "DELETE FROM cards",
            // `worker_sessions.wave_id` is a NO ACTION FK, so sessions must
            // leave before their parent waves.
            "DELETE FROM worker_sessions",
            "DELETE FROM waves",
            "DELETE FROM coves",
            "DELETE FROM plugin_kv",
            "DELETE FROM plugin_tokens",
            "DELETE FROM plugins",
            "DELETE FROM settings",
            "DELETE FROM sqlite_sequence",
        ] {
            sqlx::query(stmt).execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
    }

    // Reseed two events through the normal eventized write path. Because
    // `sqlite_sequence` was wiped, the first row lands at id=1 — the
    // fresh log tip a post-reset cold-boot client would see. We use only
    // two events (not five) so the post-reset tip is well below the
    // pre-reset tip and the regression invariant is observable.
    let post1 = repo
        .log_pure_event(
            calm_server::ids::ActorId::User,
            calm_server::event::EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::wave_cove_cache::WaveCoveCache::new(),
            Event::CoveUpdated(calm_server::model::Cove {
                id: "post-1".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::CoveKind::User,
                created_at: 0,
                updated_at: 0,
            }),
        )
        .await
        .unwrap();
    let post2 = repo
        .log_pure_event(
            calm_server::ids::ActorId::User,
            calm_server::event::EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::wave_cove_cache::WaveCoveCache::new(),
            Event::CoveUpdated(calm_server::model::Cove {
                id: "post-2".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::CoveKind::User,
                created_at: 0,
                updated_at: 0,
            }),
        )
        .await
        .unwrap();
    let post_reset_tip = post2;
    assert_eq!(
        post1, 1,
        "sqlite_sequence reset → first reseeded event lands at id=1"
    );
    assert_eq!(
        post_reset_tip, 2,
        "two reseeded events → tip id=2, well below pre-reset tip=5"
    );
    assert!(
        post_reset_tip < pre_reset_tip,
        "post-reset tip ({post_reset_tip}) must be below pre-reset tip ({pre_reset_tip}) — this is the regression the client detects"
    );

    // Crown jewel: FRESH WS subscription with `since = pre_reset_tip`. This
    // simulates a client whose persisted cursor predates the reset — exactly
    // the case the client-side reset detection in `web/src/api/events.ts`
    // needs to fire on. The `events_since(pre_reset_tip, _)` query returns
    // ZERO rows because every reseeded event has `id <= post_reset_tip <
    // pre_reset_tip`. Pre-PR-303, the terminator stamped `last_id` (which
    // remained at `since` when zero rows matched), so the client saw
    // `_replay_complete._id = pre_reset_tip` and couldn't tell anything
    // had changed. Post-PR-303, the terminator stamps `events_latest_id()`
    // = `post_reset_tip`, which the client compares against its persisted
    // cursor (`pre_reset_tip`) and triggers the reset re-bootstrap.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(format!(
        r#"{{"sub":["*"], "since": {}}}"#,
        pre_reset_tip
    )))
    .await
    .unwrap();
    let frame = recv_json(&mut ws).await;
    assert_eq!(
        frame["ev"], "_replay_complete",
        "stale-cursor subscription returns zero rows → terminator is the first frame"
    );
    assert_eq!(
        frame["_id"], post_reset_tip,
        "post-reset _replay_complete._id must equal events.MAX(id) = {post_reset_tip}, \
         NOT the pre-PR-303 in-window high-water (which equaled `since` = {pre_reset_tip})"
    );
    assert!(
        frame["_id"].as_i64().unwrap() < pre_reset_tip,
        "terminator id must be below the client's stale cursor — this is the regression signal"
    );

    // Belt-and-suspenders: also confirm a cold-boot `since=0` client sees
    // the same tip (catches a regression where the two `events_latest_id()`
    // call sites diverge).
    let (mut ws2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws2.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();
    loop {
        let v = recv_json(&mut ws2).await;
        if v["ev"] == "_replay_complete" {
            assert_eq!(
                v["_id"], post_reset_tip,
                "cold-boot terminator also carries the post-reset tip"
            );
            break;
        }
    }
}

#[tokio::test]
async fn replay_skips_future_schema_version_overlay_set_assertion_strict() {
    // Belt-and-suspenders form of the previous test that asserts on the
    // exact frame contents (not on overlay-id substring matching) so a
    // regression where the future row leaks would fail loudly even if
    // the supported row coincidentally shared a prefix. Reads frames
    // until `_replay_complete` and checks no frame carries
    // `schemaVersion: 999`.
    let (addr, repo, bus) = boot().await;
    let (_, future_id) = seed_supported_and_future_overlays(&repo, &bus).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    let mut saw_future_payload = false;
    let mut saw_complete = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !saw_complete && std::time::Instant::now() < deadline {
        let v = recv_json(&mut ws).await;
        if v["ev"] == "_replay_complete" {
            saw_complete = true;
            // Cursor must advance through the dropped row.
            assert_eq!(v["_id"], future_id);
            break;
        }
        if v["ev"] == "overlay.set" && v["data"]["payload"]["schemaVersion"] == 999 {
            saw_future_payload = true;
        }
    }
    assert!(saw_complete, "_replay_complete terminator must arrive");
    assert!(
        !saw_future_payload,
        "future-schemaVersion overlay row must be filtered from replay"
    );
}

// ---------------------------------------------------------------------------
// 8. Snapshot required when the retention pruner deleted an INTERIOR row
//    (#854 slice 2). Structural events are permanent, so `MIN(id)` never
//    advances past the first structural row — the earliest-id check alone
//    can't see holes the events pruner punches mid-stream. The durable
//    retention watermark (highest id ever pruned) closes that gap: any
//    cursor below it gets `_snapshot_required`; any cursor at or above it
//    still gets a normal contiguous replay.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_below_prune_watermark_gets_snapshot_required() {
    let (addr, repo, bus) = boot().await;

    let head = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;
    let hook_id = calm_server::db::RepoEventWrite::log_pure_event(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::card_role_cache::CardRoleCache::new(),
        &calm_server::wave_cove_cache::WaveCoveCache::new(),
        Event::ClaudeHook {
            card_id: CardId::from("card-hook"),
            kind: "stop".into(),
            hook_idempotency_key: String::new(),
            payload: json!({}),
        },
    )
    .await
    .expect("log claude.hook");
    let tail = seed_three(&repo, &bus, ["c-4", "c-5", "c-6"]).await;

    // Age the seeded rows past a millisecond horizon, then run the real
    // pruner: it deletes exactly the interior `claude.hook` row (structural
    // cove.updated rows are not allowlisted).
    tokio::time::sleep(Duration::from_millis(50)).await;
    let policy = EventsRetentionPolicy {
        horizon: Duration::from_millis(1),
        ..EventsRetentionPolicy::default()
    };
    let pruned = prune_events_once(repo.pool(), &policy)
        .await
        .expect("prune pass");
    assert_eq!(pruned, 1, "exactly the interior hook row is pruned");
    assert_eq!(
        RepoEventWrite::events_earliest_id(repo.as_ref())
            .await
            .expect("earliest"),
        Some(head[0].0),
        "structural head survives — MIN(id) cannot signal the interior hole"
    );

    // Cursor BELOW the watermark: the pruned row sits inside the replay
    // window (since < hook_id <= watermark) — must snapshot, not stream a
    // gappy window.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(format!(
        r#"{{"sub":["*"], "since": {}}}"#,
        head[2].0
    )))
    .await
    .unwrap();
    let frame = recv_json(&mut ws).await;
    assert_eq!(frame["ev"], "_snapshot_required");
    assert_eq!(frame["data"]["earliest_id"], head[0].0);
    let _ = timeout(Duration::from_millis(500), ws.next()).await;

    // Cursor AT the watermark: everything above `since` still exists, so
    // the guard must NOT over-fire — normal contiguous replay.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(format!(
        r#"{{"sub":["*"], "since": {}}}"#,
        hook_id
    )))
    .await
    .unwrap();
    let mut seen = Vec::new();
    loop {
        let frame = recv_json(&mut ws).await;
        if frame["ev"] == "_replay_complete" {
            break;
        }
        seen.push(frame["_id"].as_i64().unwrap());
    }
    assert_eq!(
        seen,
        tail.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        "cursor at the watermark replays the contiguous tail"
    );
}

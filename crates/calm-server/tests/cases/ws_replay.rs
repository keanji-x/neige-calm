//! WS replay protocol (the `since` cursor side of `ws::events::handle`) — end-to-end tests.

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::{SqlxRepo, area_create_tx, overlay_upsert_tx};
use calm_server::db::{Repo, RepoEventWrite, write_with_event_typed};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::events_prune::{EventsRetentionPolicy, prune_events_once};
use calm_server::ids::{ActorId, CardId};
use calm_server::model::{NewArea, NewOverlay};
use calm_server::plugin_host::PluginHost;
use calm_server::state::{AppState, DaemonClient};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TMessage;

/// Boot a minimal axum app with the WS events router and return the address plus the repo and bus for seeding.
async fn boot() -> (std::net::SocketAddr, Arc<SqlxRepo>, EventBus) {
    boot_with_cap(None).await
}

/// Pins the WS replay cap on this test's own `AppState`; `NEIGE_WS_REPLAY_MAX_EVENTS` is process-global and would race sibling tests.
async fn boot_with_cap(cap: Option<i64>) -> (std::net::SocketAddr, Arc<SqlxRepo>, EventBus) {
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
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    let state = match cap {
        Some(cap) => state.with_ws_replay_cap(cap),
        None => state,
    };
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

/// Seed a linear history of 3 area.updated rows; returns the assigned `events.id`s and area ids in append order.
async fn seed_three(repo: &SqlxRepo, bus: &EventBus, names: [&str; 3]) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    for name in names {
        let p = NewArea {
            name: name.to_string(),
            color: "#000".into(),
            sort: None,
        };
        let (area, event_id) = write_with_event_typed(
            repo as &dyn Repo,
            ActorId::User,
            EventScope::System,
            None,
            bus,
            &calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
            move |tx| {
                Box::pin(async move {
                    let c = area_create_tx(tx, p).await?;
                    Ok((c.clone(), Event::AreaUpdated(c)))
                })
            },
        )
        .await
        .unwrap();
        out.push((event_id, area.id.to_string()));
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

#[tokio::test]
async fn subscribe_with_since_zero_replays_all() {
    let (addr, repo, bus) = boot().await;
    let seeded = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    for (event_id, area_id) in seeded.iter() {
        let v = recv_json(&mut ws).await;
        assert_eq!(v["_id"], *event_id, "frame ids in order");
        assert_eq!(v["ev"], "area.updated");
        assert_eq!(v["data"]["id"], *area_id);
    }
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(done["_id"], seeded.last().unwrap().0);
}

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

    let v = recv_json(&mut ws).await;
    assert_eq!(v["data"]["id"], seeded[1].1);
    let v = recv_json(&mut ws).await;
    assert_eq!(v["data"]["id"], seeded[2].1);
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(done["_id"], seeded[2].0);
}

#[tokio::test]
async fn subscribe_without_since_only_live() {
    let (addr, repo, bus) = boot().await;
    let _ = seed_three(&repo, &bus, ["before-1", "before-2", "before-3"]).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // `bus.emit` is the synthetic test-only emit that produces `id = 0`; the client never advances its cursor off these frames.
    bus.emit(
        ActorId::User,
        Event::AreaUpdated(calm_server::model::Area {
            id: "live-only".into(),
            name: "n".into(),
            color: "#fff".into(),
            sort: 0.0,
            kind: calm_server::model::AreaKind::User,
            default_template_id: None,
            default_cwd: None,
            created_at: 0,
            updated_at: 0,
        }),
    );

    let v = recv_json(&mut ws).await;
    assert_eq!(v["ev"], "area.updated");
    assert_eq!(v["data"]["id"], "live-only");
    assert_eq!(v["_id"], 0);
}

#[tokio::test]
async fn replay_complete_terminator_is_sent_even_when_zero_rows() {
    let (addr, _repo, _bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(done["_id"], 0);
}

#[tokio::test]
async fn replay_then_live_no_drop_no_dupe() {
    let (addr, repo, bus) = boot().await;
    let seeded = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // The handler subscribes to the live bus BEFORE running the events_since query; race a live write against the replay.
    let since = seeded[0].0;

    ws.send(TMessage::Text(format!(
        r#"{{"sub":["*"], "since": {}}}"#,
        since
    )))
    .await
    .unwrap();
    // The handler subscribes before reading any client frame, so this sleep is paranoia rather than correctness-critical.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let new_area = NewArea {
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
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let c = area_create_tx(tx, new_area).await?;
                Ok((c.clone(), Event::AreaUpdated(c)))
            })
        },
    )
    .await
    .unwrap();
    assert!(
        live_id > seeded[2].0,
        "live event must come after seeded ids"
    );

    // Either ordering is acceptable (live event inside the replay SQL window and deduped, or after it via live forward); every id appears exactly once.
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
    assert!(
        !seen.contains(&seeded[0].0),
        "first seed already past cursor"
    );
    assert!(seen.contains(&seeded[1].0));
    assert!(seen.contains(&seeded[2].0));
    assert!(seen.contains(&live_id));
}

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

    // `since = 1` is well below the surviving earliest_id, so the gap check (`since < earliest - 1`) triggers the control frame.
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

/// Seed two `Event::OverlaySet` rows directly through `write_with_event_typed`, bypassing route-layer validation so the future-version row lands; returns `[supported_event_id, future_event_id]`.
async fn seed_supported_and_future_overlays(repo: &SqlxRepo, bus: &EventBus) -> (i64, i64) {
    // Supported: status overlay at the current schemaVersion.
    let supported = NewOverlay {
        plugin_id: "p1".into(),
        entity_kind: "track".into(),
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
            calm_server::track_area_cache::TrackAreaCache::new(),
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

    // Future: same kind, schemaVersion above the current max.
    let future = NewOverlay {
        plugin_id: "p1".into(),
        entity_kind: "track".into(),
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
            calm_server::track_area_cache::TrackAreaCache::new(),
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

    // The read-side guard drops the future-version frame but still advances the cursor so the client never re-polls it.
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    // The overlay's own `data.id` is a server-side nanoid, so assert on the kernel-stamped fields.
    let v = recv_json(&mut ws).await;
    assert_eq!(v["_id"], supported_id);
    assert_eq!(v["ev"], "overlay.set");
    assert_eq!(v["data"]["kind"], "status");
    assert_eq!(v["data"]["payload"]["state"], "running");
    assert_eq!(v["data"]["payload"]["schemaVersion"], 1);

    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(
        done["_id"], future_id,
        "cursor must advance past the dropped row so the client never re-polls it"
    );
}

#[tokio::test]
async fn replay_complete_id_reflects_server_tip_after_reset() {
    let (addr, repo, bus) = boot().await;

    // The pre-reset tip must be strictly greater than the post-reset tip for the regression check to be meaningful.
    let seeded = seed_three(&repo, &bus, ["pre-1", "pre-2", "pre-3"]).await;
    let extra1 = repo
        .log_pure_event(
            calm_server::ids::ActorId::User,
            calm_server::event::EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::track_area_cache::TrackAreaCache::new(),
            Event::AreaUpdated(calm_server::model::Area {
                id: "pre-4".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::AreaKind::User,
                default_template_id: None,
                default_cwd: None,
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
            &calm_server::track_area_cache::TrackAreaCache::new(),
            Event::AreaUpdated(calm_server::model::Area {
                id: "pre-5".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::AreaKind::User,
                default_template_id: None,
                default_cwd: None,
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
    }

    // Simulate the reset's structural wipe: every domain row, the event log, and `sqlite_sequence` so AUTOINCREMENT restarts at 1.
    {
        let pool = repo.pool();
        let mut tx = pool.begin().await.unwrap();
        for stmt in [
            "DELETE FROM events",
            "DELETE FROM overlays",
            "DELETE FROM terminals",
            "DELETE FROM cards",
            // `worker_sessions.track_id` is a NO ACTION FK, so sessions must
            // leave before their parent tracks.
            "DELETE FROM worker_sessions",
            "DELETE FROM tracks",
            "DELETE FROM areas",
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

    // Only two events, so the post-reset tip is well below the pre-reset tip.
    let post1 = repo
        .log_pure_event(
            calm_server::ids::ActorId::User,
            calm_server::event::EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::track_area_cache::TrackAreaCache::new(),
            Event::AreaUpdated(calm_server::model::Area {
                id: "post-1".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::AreaKind::User,
                default_template_id: None,
                default_cwd: None,
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
            &calm_server::track_area_cache::TrackAreaCache::new(),
            Event::AreaUpdated(calm_server::model::Area {
                id: "post-2".into(),
                name: "n".into(),
                color: "#000".into(),
                sort: 0.0,
                kind: calm_server::model::AreaKind::User,
                default_template_id: None,
                default_cwd: None,
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

    // A fresh subscription with `since = pre_reset_tip` matches zero rows; the terminator must stamp `events_latest_id()`, not `since`.
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

    // A cold-boot `since=0` client sees the same tip (the two `events_latest_id()` call sites must not diverge).
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
    // Asserts on exact frame contents so a leaked future row fails even if it shared a prefix with the supported row.
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

// Replay cap: `since == 0` over-cap skips the backlog straight to `_replay_complete` at the tip (never `_snapshot_required`,
// whose handler reconnects cold at since=0 and would loop forever); `since > 0` over-cap gets `_snapshot_required`.

/// Seed `n` `area.updated` rows via `log_pure_event`; returns the assigned `events.id`s in append order.
async fn seed_n_area_updates(repo: &SqlxRepo, bus: &EventBus, n: usize) -> Vec<i64> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = repo
            .log_pure_event(
                calm_server::ids::ActorId::User,
                calm_server::event::EventScope::System,
                None,
                bus,
                &calm_server::card_role_cache::CardRoleCache::new(),
                &calm_server::track_area_cache::TrackAreaCache::new(),
                Event::AreaUpdated(calm_server::model::Area {
                    id: format!("cap-{i}").into(),
                    name: "n".into(),
                    color: "#000".into(),
                    sort: 0.0,
                    kind: calm_server::model::AreaKind::User,
                    default_template_id: None,
                    default_cwd: None,
                    created_at: 0,
                    updated_at: 0,
                }),
            )
            .await
            .unwrap();
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn cold_replay_over_cap_skips_to_tip() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    let seeded = seed_n_area_updates(&repo, &bus, 10).await;
    let tip = *seeded.last().unwrap();

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    // The backlog (10 rows) exceeds the cap (6): the very first frame must
    // be the terminator at the server tip — no event frames stream.
    let first = recv_json(&mut ws).await;
    assert_eq!(
        first["ev"], "_replay_complete",
        "over-cap cold replay must skip the backlog, got {first}"
    );
    assert_eq!(first["_id"], tip, "terminator carries the server tip");

    // Live-forward still works: the skip must not poison the dedup cursor.
    let live_id = seed_n_area_updates(&repo, &bus, 1).await[0];
    let live = recv_json(&mut ws).await;
    assert_eq!(live["ev"], "area.updated");
    assert_eq!(live["_id"], live_id);
}

// The over-cap cold skip promotes its anchor to the tip read at request time, so a row already delivered live must not be re-streamed.
#[tokio::test]
async fn cold_skip_acks_live_delivered_row_without_duplicate() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    // Backlog of 10 (> cap 6) exists BEFORE the connection opens.
    let pre = seed_n_area_updates(&repo, &bus, 10).await;
    let backlog_tip = *pre.last().unwrap();

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Commit a row AFTER the connection subscribed. Its live arrival below
    // proves it postdates the subscribe AND that it was delivered.
    let live_row = seed_n_area_updates(&repo, &bus, 1).await[0];
    assert_eq!(live_row, backlog_tip + 1);
    let live = recv_json(&mut ws).await;
    assert_eq!(live["ev"], "area.updated");
    assert_eq!(live["_id"], live_row);

    // Cold re-anchor: 11 pending rows > cap → promote to the request-time tip; the terminator is the first and only frame.
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();
    let done = recv_json(&mut ws).await;
    assert_eq!(
        done["ev"], "_replay_complete",
        "no replay frame may precede the terminator — the delivered row \
         is covered by the promotion, got {done}"
    );
    assert_eq!(
        done["_id"], live_row,
        "the terminator acks the request-time tip, covering the row that \
         was already delivered live"
    );

    let next_id = seed_n_area_updates(&repo, &bus, 1).await[0];
    let next = recv_json(&mut ws).await;
    assert_eq!(next["ev"], "area.updated");
    assert_eq!(next["_id"], next_id);
}

// A row committed after connect but before the first sub frame was never deliverable live (empty topic set); the request-time promotion folds it into the acked backlog.
#[tokio::test]
async fn cold_skip_folds_pre_sub_frame_commit_into_the_acked_backlog() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    let pre = seed_n_area_updates(&repo, &bus, 10).await;
    let backlog_tip = *pre.last().unwrap();

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    // Commit the row while the client has not yet sent ANY sub frame.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let folded_id = seed_n_area_updates(&repo, &bus, 1).await[0];
    assert_eq!(folded_id, backlog_tip + 1);

    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();
    let done = recv_json(&mut ws).await;
    assert_eq!(
        done["ev"], "_replay_complete",
        "the never-deliverable row folds into the acked backlog, got {done}"
    );
    assert_eq!(done["_id"], folded_id);

    let next_id = seed_n_area_updates(&repo, &bus, 1).await[0];
    let next = recv_json(&mut ws).await;
    assert_eq!(next["ev"], "area.updated");
    assert_eq!(next["_id"], next_id);
}

// The handler must establish the broadcast receiver at accept with no awaited DB work before it, so a persisted commit right after subscribe is the client's FIRST frame.
#[tokio::test]
async fn live_only_client_receives_first_post_connect_commit() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    let _ = seed_n_area_updates(&repo, &bus, 3).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let live_id = seed_n_area_updates(&repo, &bus, 1).await[0];
    let first = recv_json(&mut ws).await;
    assert_eq!(
        first["ev"], "area.updated",
        "live-only client's first frame must be the live event, got {first}"
    );
    assert_eq!(first["_id"], live_id, "persisted id rides the wire");
}

// The log is empty at accept and an over-cap flood commits before the first sub frame; the request-time promotion absorbs it in one pass instead of bouncing through `_snapshot_required`.
#[tokio::test]
async fn cold_over_cap_flood_after_connect_is_absorbed_by_promotion() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let flood = seed_n_area_updates(&repo, &bus, 7).await;
    let flood_tip = *flood.last().unwrap();

    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();
    let done = recv_json(&mut ws).await;
    assert_eq!(
        done["ev"], "_replay_complete",
        "the request-time promotion absorbs the flood in one pass, got {done}"
    );
    assert_eq!(done["_id"], flood_tip, "terminator acks the flood tip");

    let next_id = seed_n_area_updates(&repo, &bus, 1).await[0];
    let next = recv_json(&mut ws).await;
    assert_eq!(next["ev"], "area.updated");
    assert_eq!(next["_id"], next_id);
}

#[tokio::test]
async fn stale_cursor_over_cap_gets_snapshot_required() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    let seeded = seed_n_area_updates(&repo, &bus, 10).await;

    // Resume from just past the first row: 9 pending rows > cap of 6.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let sub = format!(r#"{{"sub":["*"], "since": {}}}"#, seeded[0]);
    ws.send(TMessage::Text(sub)).await.unwrap();

    let frame = recv_json(&mut ws).await;
    assert_eq!(
        frame["ev"], "_snapshot_required",
        "over-cap stale-cursor replay must force a re-snapshot, got {frame}"
    );
    assert_eq!(frame["data"]["earliest_id"], seeded[0]);

    // Connection closes shortly after — tolerate either an explicit close
    // frame, a transport-level closure, or a timeout falling through.
    let _ = timeout(Duration::from_millis(500), ws.next()).await;
}

// Cap edges: a window of exactly `cap` rows is not over-cap, and the over-cap decision counts RAW rows because `events_since` drops unknown-kind rows at deserialization.

/// Insert a raw `events` row whose `kind` matches no `Event` variant; `events_since` drops it at deserialization, the raw-count probe must still see it.
async fn seed_unknown_kind_row(repo: &SqlxRepo) -> i64 {
    let row: (i64,) = sqlx::query_as(
        r#"INSERT INTO events (kind, payload, actor, at, event_version)
           VALUES ('test.unknown_kind', '{}', 'user', 0, 1)
           RETURNING id"#,
    )
    .fetch_one(repo.pool())
    .await
    .expect("insert unknown-kind events row");
    row.0
}

#[tokio::test]
async fn replay_exactly_at_cap_streams_full_window() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    let seeded = seed_n_area_updates(&repo, &bus, 6).await;
    let tip = *seeded.last().unwrap();

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    for id in &seeded {
        let v = recv_json(&mut ws).await;
        assert_eq!(
            v["ev"], "area.updated",
            "at-cap window must stream, got {v}"
        );
        assert_eq!(v["_id"], *id, "frame ids in order");
    }
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(done["_id"], tip);
}

#[tokio::test]
async fn over_cap_decision_counts_raw_rows_not_deserialized() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    // Window after `since = head[0]`: 3 good + 1 unknown-kind + 3 good
    // = 7 RAW rows (> cap 6) that deserialize to 6 events (== cap).
    let head = seed_n_area_updates(&repo, &bus, 4).await;
    let _unknown = seed_unknown_kind_row(&repo).await;
    let _tail = seed_n_area_updates(&repo, &bus, 3).await;

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let sub = format!(r#"{{"sub":["*"], "since": {}}}"#, head[0]);
    ws.send(TMessage::Text(sub)).await.unwrap();

    // A filtered-length decision sees 6 <= 6 and streams the page; the raw
    // count sees 7 > 6 and must force the re-snapshot instead.
    let frame = recv_json(&mut ws).await;
    assert_eq!(
        frame["ev"], "_snapshot_required",
        "over-cap decision must count raw rows (7), not deserialized events (6); got {frame}"
    );

    let _ = timeout(Duration::from_millis(500), ws.next()).await;
}

#[tokio::test]
async fn unknown_kind_row_in_under_cap_window_skips_only_that_row() {
    let (addr, repo, bus) = boot_with_cap(Some(6)).await;
    // 2 good + 1 unknown-kind + 2 good = 5 raw rows <= cap 6: full replay.
    let head = seed_n_area_updates(&repo, &bus, 2).await;
    let unknown_id = seed_unknown_kind_row(&repo).await;
    let tail = seed_n_area_updates(&repo, &bus, 2).await;
    let tip = *tail.last().unwrap();

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();

    let mut expected: Vec<i64> = head.clone();
    expected.extend(&tail);
    for id in &expected {
        let v = recv_json(&mut ws).await;
        assert_eq!(v["ev"], "area.updated", "good rows must stream, got {v}");
        assert_eq!(
            v["_id"], *id,
            "no event may be skipped around the unknown-kind row"
        );
    }
    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(done["_id"], tip);
    assert!(
        unknown_id > head[1] && unknown_id < tail[0],
        "sanity: the unknown-kind row sits inside the replayed window"
    );
}

// Structural events are permanent, so `MIN(id)` never advances past the first structural row; the durable retention watermark is what sees interior holes.

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
        &calm_server::track_area_cache::TrackAreaCache::new(),
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

    // Age the rows past a millisecond horizon, then run the real pruner: it deletes exactly the interior `claude.hook` row.
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

    // Cursor BELOW the watermark: the pruned row sits inside the replay window.
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

// A tail prune leaves the watermark above the live MAX(id); the terminator must ack up to the watermark or the reconnect cursor bounces to `_snapshot_required` forever.

#[tokio::test]
async fn tail_prune_does_not_strand_client_in_snapshot_loop() {
    let (addr, repo, bus) = boot().await;

    let head = seed_three(&repo, &bus, ["c-1", "c-2", "c-3"]).await;
    let hook_id = calm_server::db::RepoEventWrite::log_pure_event(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::card_role_cache::CardRoleCache::new(),
        &calm_server::track_area_cache::TrackAreaCache::new(),
        Event::ClaudeHook {
            card_id: CardId::from("card-hook"),
            kind: "stop".into(),
            hook_idempotency_key: String::new(),
            payload: json!({}),
        },
    )
    .await
    .expect("log tail claude.hook");

    // Age the rows, then prune: the hook is the log's TAIL, so the
    // watermark lands ABOVE the surviving MAX(id).
    tokio::time::sleep(Duration::from_millis(50)).await;
    let policy = EventsRetentionPolicy {
        horizon: Duration::from_millis(1),
        ..EventsRetentionPolicy::default()
    };
    let pruned = prune_events_once(repo.pool(), &policy)
        .await
        .expect("prune pass");
    assert_eq!(pruned, 1, "exactly the tail hook row is pruned");
    assert_eq!(
        RepoEventWrite::events_latest_id(repo.as_ref())
            .await
            .expect("latest"),
        Some(head[2].0),
        "sanity: the live tip now sits below the prune watermark"
    );

    // Cold replay: streams the surviving areas, then the terminator must
    // ack up to the WATERMARK, not the (lower) live tip.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .unwrap();
    let cursor = loop {
        let frame = recv_json(&mut ws).await;
        assert_ne!(
            frame["ev"], "_snapshot_required",
            "cold replay must never be told to re-snapshot"
        );
        if frame["ev"] == "_replay_complete" {
            break frame["_id"].as_i64().unwrap();
        }
    };
    assert_eq!(
        cursor, hook_id,
        "terminator floors at the prune watermark (dead tail ids are acked)"
    );

    // Reconnect with the stamped cursor: a normal empty replay, no `_snapshot_required`, and `_id` must not dip below the cursor.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(format!(
        r#"{{"sub":["*"], "since": {cursor}}}"#
    )))
    .await
    .unwrap();
    let frame = recv_json(&mut ws).await;
    assert_eq!(
        frame["ev"], "_replay_complete",
        "reconnect at the stamped cursor must replay cleanly, got {frame}"
    );
    assert_eq!(
        frame["_id"], cursor,
        "no false log-regression signal from a tail-pruned tip"
    );
}

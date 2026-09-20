//! Backward-compat coverage for the schemaVersion read-side guard: legacy rows
//! with no `schemaVersion` are delivered live, and replay's cursor advances past dropped future rows.

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
            calm_server::state::WriteContext::new(
                CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
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

/// Seed an Overlay row whose payload omits `schemaVersion` entirely; the validator treats absent as version 1.
async fn seed_legacy_overlay(repo: &SqlxRepo, bus: &EventBus) -> i64 {
    let legacy = NewOverlay {
        plugin_id: "p-legacy".into(),
        entity_kind: "track".into(),
        entity_id: "w-legacy".into(),
        kind: "eta".into(),
        payload: json!({ "text": "5m" }),
    };
    let (_o, event_id) = write_with_event_typed(
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
                let o = overlay_upsert_tx(tx, legacy).await?;
                Ok((o.clone(), Event::OverlaySet(o)))
            })
        },
    )
    .await
    .expect("seed legacy overlay");
    event_id
}

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

#[tokio::test]
async fn live_broadcast_delivers_overlay_set_with_missing_schema_version() {
    let (addr, repo, bus) = boot().await;

    // Subscribe first, then write, so the row hits the live-broadcast path rather than the replay query.
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    // No `since` → no replay, just live. Give the server a tick to set up its subscriber before we emit.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let legacy_id = seed_legacy_overlay(&repo, &bus).await;

    let v = recv_json(&mut ws).await;
    assert_eq!(v["_id"], legacy_id);
    assert_eq!(v["ev"], "overlay.set");
    assert_eq!(v["data"]["payload"]["text"], "5m");
    assert!(v["data"]["payload"].get("schemaVersion").is_none());
}

#[tokio::test]
async fn replay_mixes_legacy_pass_through_with_future_drop() {
    let (addr, repo, bus) = boot().await;

    // Order matters: the assertion uses `_replay_complete._id` to confirm the cursor advanced past BOTH rows.
    let legacy_id = seed_legacy_overlay(&repo, &bus).await;

    let future = NewOverlay {
        plugin_id: "p-future".into(),
        entity_kind: "track".into(),
        entity_id: "w-future".into(),
        kind: "eta".into(),
        payload: json!({ "schemaVersion": 999, "text": "from-future" }),
    };
    let (_o, future_id) = write_with_event_typed(
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

    let v = recv_json(&mut ws).await;
    assert_eq!(v["_id"], legacy_id);
    assert_eq!(v["ev"], "overlay.set");
    assert!(v["data"]["payload"].get("schemaVersion").is_none());

    let done = recv_json(&mut ws).await;
    assert_eq!(done["ev"], "_replay_complete");
    assert_eq!(
        done["_id"], future_id,
        "cursor must advance past dropped future row",
    );
}

//! Integration test for `GET /api/events` (track C).
//!
//! Boots a minimal Axum app with the WS events router + AppState (in-memory
//! SqlxRepo, EventBus, stub daemon/plugin), then drives a real WebSocket
//! client via `tokio_tungstenite` to verify:
//!
//!   1. `{"sub":[...]}` replaces the subscription set.
//!   2. Events matching at least one subscribed topic are forwarded.
//!   3. Events not matching are silently dropped.
//!
//! Both dependencies (`axum`, `tokio_tungstenite`) are already in `[dependencies]`
//! so they're usable here without touching `Cargo.toml`.

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::Cove;
use calm_server::plugin_host::PluginHost;
use calm_server::state::{AppState, DaemonClient};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TMessage;

async fn boot() -> (std::net::SocketAddr, EventBus) {
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
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events.clone(),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
    );
    let app = axum::Router::new().merge(ws::router()).with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Give the server a beat to be ready.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, events)
}

fn sample_cove(id: &str) -> Cove {
    Cove {
        id: id.into(),
        name: "n".into(),
        color: "#fff".into(),
        sort: 0.0,
        created_at: 0,
        updated_at: 0,
    }
}

#[tokio::test]
async fn forwards_matching_event() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Subscribe to cove:c-001 only.
    ws.send(TMessage::Text(r#"{"sub":["cove:c-001"]}"#.to_string()))
        .await
        .unwrap();

    // Give the subscription time to register before emitting.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Non-matching event first; must NOT arrive.
    bus.emit(ActorId::User, Event::CoveUpdated(sample_cove("c-other")));
    // Matching event; must arrive.
    bus.emit(ActorId::User, Event::CoveUpdated(sample_cove("c-001")));

    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed")
        .expect("ws error");

    let text = match msg {
        TMessage::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {:?}", other),
    };

    // The body should be the *matching* event (c-001), not c-other.
    assert!(text.contains("cove.updated"), "got: {}", text);
    assert!(text.contains("c-001"), "got: {}", text);
    assert!(!text.contains("c-other"), "got: {}", text);
}

#[tokio::test]
async fn empty_sub_drops_everything() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Empty sub — connection stays open, but nothing should be forwarded.
    ws.send(TMessage::Text(r#"{"sub":[]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    bus.emit(ActorId::User, Event::CoveUpdated(sample_cove("c-001")));

    // Expect a timeout (no message arrives).
    let res = timeout(Duration::from_millis(300), ws.next()).await;
    assert!(res.is_err(), "expected no message, got {:?}", res);
}

#[tokio::test]
async fn firehose_receives_all() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    bus.emit(ActorId::User, Event::CoveDeleted { id: "c-x".into() });

    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timeout")
        .expect("closed")
        .expect("err");
    if let TMessage::Text(t) = msg {
        assert!(t.contains("cove.deleted"));
        assert!(t.contains("c-x"));
    } else {
        panic!("not text");
    }
}

#[tokio::test]
async fn replaces_not_extends() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // First sub: cove:c-001.
    ws.send(TMessage::Text(r#"{"sub":["cove:c-001"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Now replace with cove:c-002.
    ws.send(TMessage::Text(r#"{"sub":["cove:c-002"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Emit c-001: should be dropped (we replaced, not extended).
    bus.emit(ActorId::User, Event::CoveUpdated(sample_cove("c-001")));
    let res = timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        res.is_err(),
        "c-001 should NOT have been forwarded after replace, got {:?}",
        res
    );

    // Emit c-002: should arrive.
    bus.emit(ActorId::User, Event::CoveUpdated(sample_cove("c-002")));
    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timeout")
        .expect("closed")
        .expect("err");
    if let TMessage::Text(t) = msg {
        assert!(t.contains("c-002"));
    } else {
        panic!("not text");
    }
}

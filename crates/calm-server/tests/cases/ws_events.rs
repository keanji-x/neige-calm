//! Integration test for `GET /api/events`: a `sub` replaces the subscription set, matching events forward, others drop.

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{Area, AreaKind, Overlay};
use calm_server::plugin_host::PluginHost;
use calm_server::state::{AppState, DaemonClient};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
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
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
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
    (addr, events)
}

fn sample_area(id: &str) -> Area {
    Area {
        id: id.into(),
        name: "n".into(),
        color: "#fff".into(),
        sort: 0.0,
        kind: AreaKind::User,
        default_template_id: None,
        default_cwd: None,
        created_at: 0,
        updated_at: 0,
    }
}

#[tokio::test]
async fn forwards_matching_event() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(TMessage::Text(r#"{"sub":["area:c-001"]}"#.to_string()))
        .await
        .unwrap();

    // Give the subscription time to register before emitting.
    tokio::time::sleep(Duration::from_millis(50)).await;

    bus.emit(ActorId::User, Event::AreaUpdated(sample_area("c-other")));
    bus.emit(ActorId::User, Event::AreaUpdated(sample_area("c-001")));

    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed")
        .expect("ws error");

    let text = match msg {
        TMessage::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {:?}", other),
    };

    assert!(text.contains("area.updated"), "got: {}", text);
    assert!(text.contains("c-001"), "got: {}", text);
    assert!(!text.contains("c-other"), "got: {}", text);
}

#[tokio::test]
async fn empty_sub_drops_everything() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(TMessage::Text(r#"{"sub":[]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    bus.emit(ActorId::User, Event::AreaUpdated(sample_area("c-001")));

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

    bus.emit(ActorId::User, Event::AreaDeleted { id: "c-x".into() });

    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timeout")
        .expect("closed")
        .expect("err");
    if let TMessage::Text(t) = msg {
        assert!(t.contains("area.deleted"));
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

    ws.send(TMessage::Text(r#"{"sub":["area:c-001"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    ws.send(TMessage::Text(r#"{"sub":["area:c-002"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Emit c-001: should be dropped (we replaced, not extended).
    bus.emit(ActorId::User, Event::AreaUpdated(sample_area("c-001")));
    let res = timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        res.is_err(),
        "c-001 should NOT have been forwarded after replace, got {:?}",
        res
    );

    bus.emit(ActorId::User, Event::AreaUpdated(sample_area("c-002")));
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

/// The future-version row is emitted first so a missing filter would land it on the wire before the supported frame.
#[tokio::test]
async fn future_schema_version_overlay_set_is_filtered_on_live_broadcast() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Firehose so neither overlay is filtered out by the topic check —
    // the only reason for a drop should be the schemaVersion guard.
    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let future = Overlay {
        id: "o-future".into(),
        plugin_id: "p1".into(),
        entity_kind: "track".into(),
        entity_id: "w-1".into(),
        kind: "eta".into(),
        payload: json!({ "schemaVersion": 999, "text": "from-future" }),
        updated_at: 0,
    };
    let supported = Overlay {
        id: "o-supported".into(),
        plugin_id: "p1".into(),
        entity_kind: "track".into(),
        entity_id: "w-1".into(),
        kind: "eta".into(),
        payload: json!({ "schemaVersion": 1, "text": "5m" }),
        updated_at: 0,
    };

    bus.emit(ActorId::User, Event::OverlaySet(future));
    bus.emit(ActorId::User, Event::OverlaySet(supported));

    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed")
        .expect("ws error");
    let text = match msg {
        TMessage::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {:?}", other),
    };
    assert!(
        text.contains("overlay.set"),
        "expected overlay.set, got: {text}"
    );
    assert!(text.contains("o-supported"), "got: {text}");
    assert!(
        !text.contains("o-future"),
        "future-schemaVersion overlay must not appear in any frame, got: {text}"
    );

    let leftover = timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        leftover.is_err(),
        "expected no further frames after the supported overlay, got {:?}",
        leftover,
    );
}

/// A plugin-owned overlay kind has no version policy, so an arbitrarily high `schemaVersion` passes through.
#[tokio::test]
async fn plugin_owned_overlay_passes_through_live_broadcast() {
    let (addr, bus) = boot().await;
    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(TMessage::Text(r#"{"sub":["*"]}"#.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let opaque = Overlay {
        id: "o-plugin".into(),
        plugin_id: "p1".into(),
        entity_kind: "track".into(),
        entity_id: "w-1".into(),
        kind: "custom-badge".into(),
        payload: json!({ "schemaVersion": 999, "anything": true }),
        updated_at: 0,
    };
    bus.emit(ActorId::User, Event::OverlaySet(opaque));

    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed")
        .expect("ws error");
    if let TMessage::Text(t) = msg {
        assert!(t.contains("overlay.set"), "got: {t}");
        assert!(t.contains("o-plugin"), "got: {t}");
        assert!(t.contains("custom-badge"), "got: {t}");
    } else {
        panic!("expected text frame");
    }
}

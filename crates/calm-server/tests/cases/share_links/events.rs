//! Real public socket boundary tests.
use super::reads::track;
use super::*;
use futures_util::StreamExt;
use std::time::Duration;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn frame(ws: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn socket(fx: &Fixture, token: &str) -> (Socket, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = fx.app.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let (ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/share/{token}/events"))
            .await
            .unwrap();
    (ws, server)
}

#[tokio::test]
async fn public_socket_filters_scopes_and_closes_on_revocation() {
    let fx = Fixture::new().await;
    let link = fx.create(true).await;
    let (mut ws, server) = socket(&fx, link["token"].as_str().unwrap()).await;
    assert_eq!(
        frame(&mut ws).await,
        json!({"type":"snapshot","scope":{"kind":"track","id":fx.track}})
    );
    let unrelated = track(&fx, &fx.area).await;
    // Both real routes emit TrackUpdated. Only the shared Track invalidates.
    let (status, value) = fx
        .request(
            "PATCH",
            &format!("/api/tracks/{unrelated}"),
            Some(&fx.cookie),
            json!({"title":"private change"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), ws.next())
            .await
            .is_err()
    );
    let (status, value) = fx
        .request(
            "PATCH",
            &format!("/api/tracks/{}", fx.track),
            Some(&fx.cookie),
            json!({"title":"shared change"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(
        frame(&mut ws).await,
        json!({"type":"invalidate","scope":{"kind":"track","id":fx.track}})
    );
    fx.request(
        "DELETE",
        &format!(
            "/api/share-links/{}",
            link["link"]["delegation"]["id"].as_str().unwrap()
        ),
        Some(&fx.cookie),
        json!(null),
    )
    .await;
    assert_eq!(frame(&mut ws).await, json!({"type":"closed"}));
    assert!(matches!(
        ws.next().await,
        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None
    ));
    server.abort();
}

#[tokio::test]
async fn public_socket_closes_on_track_and_area_deletion() {
    for track_scope in [true, false] {
        let fx = Fixture::new().await;
        let link = fx.create(track_scope).await;
        let (mut ws, server) = socket(&fx, link["token"].as_str().unwrap()).await;
        assert_eq!(frame(&mut ws).await["type"], "snapshot");
        let path = if track_scope {
            format!("/api/tracks/{}", fx.track)
        } else {
            format!("/api/areas/{}", fx.area)
        };
        let (status, value) = fx
            .request("DELETE", &path, Some(&fx.cookie), json!(null))
            .await;
        assert!(status.is_success(), "{status}: {value}");
        assert_eq!(frame(&mut ws).await, json!({"type":"closed"}));
        server.abort();
    }
}

#[tokio::test]
async fn socket_rechecks_revocation_between_subscription_and_snapshot() {
    let fx = Fixture::new().await;
    let link = fx.create(true).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::tracks::install_track_detail_read_hook_for_test(
        &fx.track,
        calm_server::routes::tracks::TrackDetailReadHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let (mut ws, server) = socket(&fx, link["token"].as_str().unwrap()).await;
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    fx.request(
        "DELETE",
        &format!(
            "/api/share-links/{}",
            link["link"]["delegation"]["id"].as_str().unwrap()
        ),
        Some(&fx.cookie),
        json!(null),
    )
    .await;
    release.notify_one();
    assert_eq!(frame(&mut ws).await, json!({"type":"closed"}));
    server.abort();
}

#[tokio::test]
async fn area_socket_invalidates_for_new_descendants_and_their_deletion() {
    let fx = Fixture::new().await;
    let link = fx.create(false).await;
    let (mut ws, server) = socket(&fx, link["token"].as_str().unwrap()).await;
    assert_eq!(frame(&mut ws).await["type"], "snapshot");
    let future = track(&fx, &fx.area).await;
    let (status, value) = fx
        .request(
            "PATCH",
            &format!("/api/tracks/{future}"),
            Some(&fx.cookie),
            json!({"title":"descendant change"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let expected = json!({"type":"invalidate","scope":{"kind":"area","id":fx.area}});
    assert_eq!(frame(&mut ws).await, expected);
    let (status, value) = fx
        .request(
            "DELETE",
            &format!("/api/tracks/{future}"),
            Some(&fx.cookie),
            json!(null),
        )
        .await;
    assert!(status.is_success(), "{status}: {value}");
    assert_eq!(frame(&mut ws).await, expected);
    let hidden = track(&fx, &fx.area).await;
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET purpose='area-chat' WHERE id=?1").bind(&hidden))
        .await
        .unwrap();
    let (status, value) = fx
        .request(
            "PATCH",
            &format!("/api/tracks/{hidden}"),
            Some(&fx.cookie),
            json!({"title":"hidden scaffolding"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), ws.next())
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn area_socket_invalidates_when_visible_descendant_moves_out() {
    let fx = Fixture::new().await;
    let link = fx.create(false).await;
    let token = link["token"].as_str().unwrap();
    let (mut ws, server) = socket(&fx, token).await;
    assert_eq!(frame(&mut ws).await["type"], "snapshot");
    let destination = fx
        .state
        .raw_repo()
        .area_create(
            OWNER_USER_ID,
            NewArea {
                name: "destination".into(),
                color: "#ffffff".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    // Preserve an actual Track event's old home scope, as an event buffered
    // before the move would have. No payload is exposed by the public socket.
    let mut events = fx.state.events.subscribe();
    let (status, value) = fx
        .request(
            "PATCH",
            &format!("/api/tracks/{}", fx.track),
            Some(&fx.cookie),
            json!({"title":"before move"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let event = events.recv().await.unwrap();
    let expected = json!({"type":"invalidate","scope":{"kind":"area","id":fx.area}});
    assert_eq!(frame(&mut ws).await, expected);
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET area_id=?2 WHERE id=?1")
                .bind(&fx.track)
                .bind(destination.id.as_str()),
        )
        .await
        .unwrap();
    let (status, target) = fx
        .request("GET", &format!("/api/share/{token}"), None, json!(null))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(target["tracks"], json!([]));
    fx.state.events.emit_envelope_for_test(event);
    assert_eq!(frame(&mut ws).await, expected);
    server.abort();
}

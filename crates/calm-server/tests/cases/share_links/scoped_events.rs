//! Regular event sockets use only the selected link cookie for replay and live reads.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) async fn serve(fx: &Fixture) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = fx.app.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (address, server)
}
async fn socket(address: std::net::SocketAddr, id: &str, cookie: &str) -> Socket {
    let mut req = format!("ws://{address}/api/events?share_link={id}")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}
async fn frame(ws: &mut Socket) -> Value {
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(next.to_text().unwrap()).unwrap()
}
pub(super) async fn closed(ws: &mut Socket) {
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();
    assert!(
        matches!(next, None | Some(Ok(Message::Close(_))) | Some(Err(_))),
        "{next:?}"
    );
}

#[tokio::test]
async fn regular_socket_filters_replay_and_live_and_closes_on_revoke() {
    let fx = Fixture::new().await;
    let sibling = super::fixtures::track(&fx, &fx.area).await;
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    for (track, title) in [(&sibling, "private replay"), (&fx.track, "shared replay")] {
        assert_eq!(
            fx.request(
                "PATCH",
                &format!("/api/tracks/{track}"),
                Some(&fx.cookie),
                json!({"title":title})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let (address, server) = serve(&fx).await;
    let mut ws = socket(address, id, &cookie).await;
    ws.send(Message::Text(json!({"sub":["*"],"since":0}).to_string()))
        .await
        .unwrap();
    let mut replay = vec![];
    loop {
        let value = frame(&mut ws).await;
        if value["ev"] == "_replay_complete" {
            break;
        }
        assert!(!value.to_string().contains("private replay"), "{value}");
        if value["ev"] == "track.updated" {
            replay.push(value["data"]["title"].clone());
        }
    }
    assert_eq!(replay, vec![json!("shared replay")]);
    fx.request(
        "PATCH",
        &format!("/api/tracks/{sibling}"),
        Some(&fx.cookie),
        json!({"title":"private live"}),
    )
    .await;
    fx.request(
        "PATCH",
        &format!("/api/tracks/{}", fx.track),
        Some(&fx.cookie),
        json!({"title":"shared live"}),
    )
    .await;
    let live = frame(&mut ws).await;
    assert_eq!(live["data"]["title"], "shared live");
    fx.request(
        "DELETE",
        &format!("/api/share-links/{id}"),
        Some(&fx.cookie),
        Value::Null,
    )
    .await;
    closed(&mut ws).await;
    let mut req = format!("ws://{address}/api/events?share_link={id}")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let error = tokio_tungstenite::connect_async(req).await.unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == StatusCode::UNAUTHORIZED)
    );
    server.abort();
}

#[tokio::test]
async fn expiry_closes_socket_and_refuses_every_access_entry() {
    let fx = Fixture::new().await;
    let short = short_lived_link(&fx).await;
    let cookie = super::tokens::unlock(&fx, &short).await;
    let (address, server) = serve(&fx).await;
    let mut ws = socket(address, "short-lived", &cookie).await;
    closed(&mut ws).await;
    for (method, path, expected, body) in [
        (
            "GET",
            "/api/share-links/short-lived/access".to_owned(),
            StatusCode::NOT_FOUND,
            Value::Null,
        ),
        (
            "POST",
            "/api/share-links/short-lived/unlock".to_owned(),
            StatusCode::NOT_FOUND,
            json!({"token":short["token"]}),
        ),
        (
            "GET",
            format!("/api/tracks/{}?share_link=short-lived", fx.track),
            StatusCode::UNAUTHORIZED,
            Value::Null,
        ),
        (
            "POST",
            "/api/cards/missing/planner/input?share_link=short-lived".to_owned(),
            StatusCode::UNAUTHORIZED,
            json!({"text":"expired input"}),
        ),
    ] {
        assert_eq!(
            fx.request(method, &path, Some(&cookie), body).await.0,
            expected,
            "{path}"
        );
    }
    let mut req = format!("ws://{address}/api/events?share_link=short-lived")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let error = tokio_tungstenite::connect_async(req).await.unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == StatusCode::UNAUTHORIZED)
    );
    server.abort();
}

async fn idle_session_invalidation(mode: u8) {
    let fx = Fixture::new().await;
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let (address, server) = serve(&fx).await;
    // Stop the first wait so the replay frame cannot race the next scope check.
    let initial = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "share-events-before-wait",
        id,
        initial.clone(),
    );
    let mut ws = socket(address, id, &cookie).await;
    tokio::time::timeout(Duration::from_secs(5), initial.entered.notified())
        .await
        .unwrap();
    let idle = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test("share-events-before-wait", id, idle.clone());
    ws.send(Message::Text(json!({"sub":[],"since":0}).to_string()))
        .await
        .unwrap();
    initial.release.notify_one();
    assert_eq!(frame(&mut ws).await["ev"], "_replay_complete");
    tokio::time::timeout(Duration::from_secs(5), idle.entered.notified())
        .await
        .unwrap();
    // The scope check is complete; only a lifecycle wake can end this idle wait.
    invalidate(&fx, &link, &cookie, mode).await;
    idle.release.notify_one();
    closed(&mut ws).await;
    server.abort();
}

pub(super) async fn invalidate(fx: &Fixture, link: &Value, cookie: &str, mode: u8) {
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    match mode {
        0 => assert_eq!(
            fx.request(
                "POST",
                &format!("/api/auth/logout?share_link={id}"),
                Some(cookie),
                json!({})
            )
            .await
            .0,
            StatusCode::OK
        ),
        1 => {
            let response = fx
                .app
                .clone()
                .oneshot(
                    Request::post(format!("/api/share-links/{id}/unlock"))
                        .header("cookie", cookie)
                        .header("content-type", "application/json")
                        .body(Body::from(json!({"token":link["token"]}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        2 => fx
            .auth
            .sessions
            .remove(cookie.split_once('=').unwrap().1)
            .unwrap(),
        3 => assert_eq!(
            fx.request(
                "DELETE",
                &format!("/api/share-links/{id}"),
                Some(&fx.cookie),
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        ),
        4 => {} // The immutable link deadline closes an otherwise idle socket.
        _ => unreachable!(),
    }
}
#[tokio::test]
async fn idle_events_close_on_logout_without_application_event() {
    idle_session_invalidation(0).await;
}
#[tokio::test]
async fn idle_events_close_on_reunlock_without_application_event() {
    idle_session_invalidation(1).await;
}
#[tokio::test]
async fn idle_events_close_on_session_store_removal_without_application_event() {
    idle_session_invalidation(2).await;
}

pub(super) async fn short_lived_link(fx: &Fixture) -> Value {
    use sha2::{Digest, Sha256};
    let original = fx.create(true).await;
    let original_id = original["link"]["delegation"]["id"].as_str().unwrap();
    let token = "expiry-only-secret";
    let now = calm_server::model::now_ms();
    fx.state.raw_repo().sqlite_db().unwrap().writer().execute(sqlx::query(
        "INSERT INTO share_links(id,track_id,approver_user_id,purpose,resource_policy,created_at,token_hash,expires_at)
         SELECT 'short-lived',track_id,approver_user_id,purpose,resource_policy,?2,?3,?4 FROM share_links WHERE id=?1"
    ).bind(original_id).bind(now-86_398_000).bind(hex::encode(Sha256::digest(token))).bind(now+2_000)).await.unwrap();
    json!({"link":{"delegation":{"id":"short-lived"}},"token":token})
}

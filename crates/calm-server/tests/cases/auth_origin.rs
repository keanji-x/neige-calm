//! #1780 S1: cookie-authenticated writes and WS upgrades must come from one of calm's own origins.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::AuthState;
use calm_server::routes;
use http_body_util::BodyExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

use super::auth::{fresh_state, live_auth_state};

const HOST: &str = "192.168.1.5:4040";

async fn app_and_cookie(auth: AuthState) -> (axum::Router, String) {
    let app = routes::application_router(fresh_state().await, auth);
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"alice","password":"pw"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    (app, cookie)
}

async fn post_area(app: &axum::Router, cookie: &str, origin: Option<&str>) -> StatusCode {
    let mut request = Request::post("/api/areas")
        .header(header::HOST, HOST)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    let resp = app
        .clone()
        .oneshot(
            request
                .body(Body::from(r##"{"name":"n","color":"#abc"}"##))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    if status == StatusCode::FORBIDDEN {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("cross-origin request rejected"), "{body}");
    }
    status
}

#[tokio::test]
async fn cross_port_origin_post_with_cookie_is_forbidden() {
    let (app, cookie) = app_and_cookie(live_auth_state("alice", "pw")).await;
    let status = post_area(&app, &cookie, Some("http://192.168.1.5:4050")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn null_origin_post_with_cookie_is_forbidden() {
    let (app, cookie) = app_and_cookie(live_auth_state("alice", "pw")).await;
    assert_eq!(
        post_area(&app, &cookie, Some("null")).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn same_origin_absent_origin_and_allowed_origin_posts_succeed() {
    let auth = live_auth_state("alice", "pw").with_allowed_origin("http://localhost:5175".into());
    let (app, cookie) = app_and_cookie(auth).await;
    for origin in [
        Some("http://192.168.1.5:4040"),
        Some("https://192.168.1.5:4040"),
        None,
        Some("http://localhost:5175"),
    ] {
        assert_eq!(
            post_area(&app, &cookie, origin).await,
            StatusCode::CREATED,
            "{origin:?}"
        );
    }
}

/// Dev autologin is ambient authority without any cookie, so it gets the same fence.
#[tokio::test]
async fn dev_autologin_cross_port_post_is_forbidden() {
    let auth = AuthState::new(calm_server::auth::AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "Owner".into(),
    });
    let app = routes::application_router(fresh_state().await, auth);
    let status = post_area(&app, "", Some("http://192.168.1.5:4050")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn safe_methods_ignore_foreign_origin() {
    let (app, cookie) = app_and_cookie(live_auth_state("alice", "pw")).await;
    let resp = app
        .oneshot(
            Request::get("/api/areas")
                .header(header::HOST, HOST)
                .header(header::ORIGIN, "http://192.168.1.5:4050")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn ws_upgrade_rejects_foreign_origin_and_accepts_same_origin() {
    let (app, cookie) = app_and_cookie(live_auth_state("alice", "pw")).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let handshake = |origin: String| {
        let mut request = format!("ws://{addr}/api/events")
            .into_client_request()
            .unwrap();
        let headers = request.headers_mut();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        headers.insert(header::ORIGIN, origin.parse().unwrap());
        tokio_tungstenite::connect_async(request)
    };
    match handshake(format!("http://127.0.0.1:{}", addr.port() + 1)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), StatusCode::FORBIDDEN)
        }
        other => panic!("foreign-origin upgrade must be refused: {other:?}"),
    }
    handshake(format!("http://{addr}"))
        .await
        .expect("same-origin upgrade");
}

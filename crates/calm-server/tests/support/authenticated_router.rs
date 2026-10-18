//! Password-login fixture using the production application boundary. Requests
//! keep their explicit cookies; older helpers inherit the actual issued cookie.
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::middleware::Next;
use calm_server::auth::{AuthConfig, AuthState};
use calm_server::state::AppState;
use serde_json::json;
use tower::ServiceExt;

pub async fn application(state: AppState) -> axum::Router {
    let app = calm_server::routes::application_router(
        state,
        AuthState::new(AuthConfig {
            username: Some("fixture-owner".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        }),
    );
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .extension(ConnectInfo(
                    "127.0.0.1:4444".parse::<std::net::SocketAddr>().unwrap(),
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username":"fixture-owner","password":"fixture-password"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .parse::<axum::http::HeaderValue>()
        .unwrap();
    app.layer(axum::middleware::from_fn(
        move |mut request: Request<Body>, next: Next| {
            let cookie = cookie.clone();
            async move {
                if !request.headers().contains_key(header::COOKIE) {
                    request.headers_mut().insert(header::COOKIE, cookie);
                }
                next.run(request).await
            }
        },
    ))
}

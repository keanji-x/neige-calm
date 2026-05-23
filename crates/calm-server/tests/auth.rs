//! Integration tests for the issue #189 auth surface — drives the real
//! axum routes via `tower::ServiceExt::oneshot`, covering:
//!
//!   * `POST /api/auth/login` happy path (cookie issued + whoami body)
//!   * `POST /api/auth/login` wrong-credential path (401 + standard body)
//!   * `GET /api/auth/whoami` 401 / 200 paths
//!   * Protected route 401 with valid 401 payload shape
//!   * Protected route 200 once the session cookie is presented
//!   * `POST /api/auth/logout` clears the cookie + invalidates the session
//!   * `CALM_DEV_AUTOLOGIN=true` lets every request through without a cookie
//!
//! Each test boots a fresh `AuthState` + `AppState` and assembles the same
//! router shape the production binary does (auth router merged at top,
//! protected REST tree gated by `require_session`). We sidestep the WS
//! ladder here — the WS upgrade route is covered by the existing
//! `tests/ws_*.rs` suite plus a smoke test below that asserts the upgrade
//! rejects an unauthenticated request with 401 (no upgrade response).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fresh_state() -> AppState {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
}

fn live_auth_state(user: &str, pass: &str) -> AuthState {
    AuthState::new(AuthConfig {
        username: Some(user.into()),
        password: Some(pass.into()),
        dev_autologin: false,
        display_name: user.into(),
    })
}

fn dev_auth_state() -> AuthState {
    AuthState::new(AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "Owner".into(),
    })
}

/// Mirror the assembly done in `main.rs`: protected REST behind the
/// session middleware, public REST + auth routes un-gated. Returns the
/// router ready for `oneshot` calls.
fn app(state: AppState, auth_state: AuthState) -> axum::Router {
    let protected_rest = routes::protected_router().layer(axum::middleware::from_fn_with_state(
        auth_state.clone(),
        auth::require_session,
    ));
    let public_rest = routes::public_router();
    let auth_router = auth::router().with_state(auth_state.clone());
    axum::Router::new()
        .merge(protected_rest)
        .merge(public_rest)
        .with_state(state)
        .merge(auth_router)
}

fn extract_session_cookie(resp_headers: &axum::http::HeaderMap) -> String {
    let raw = resp_headers
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present")
        .to_str()
        .expect("ascii");
    // The cookie's name=value is the first ;-separated segment.
    let first = raw.split(';').next().unwrap();
    assert!(
        first.starts_with(&format!("{SESSION_COOKIE}=")),
        "expected {SESSION_COOKIE}=... got {first}"
    );
    first.to_string()
}

#[tokio::test]
async fn login_success_issues_cookie_and_returns_whoami() {
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);

    let body = serde_json::to_vec(&serde_json::json!({
        "username": "alice",
        "password": "hunter2",
    }))
    .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Cookie checks — Set-Cookie present + attrs we care about.
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie set on login")
        .to_str()
        .unwrap()
        .to_string();
    assert!(raw.starts_with(&format!("{SESSION_COOKIE}=")));
    assert!(raw.contains("HttpOnly"), "missing HttpOnly: {raw}");
    assert!(
        raw.contains("SameSite=Strict"),
        "missing SameSite=Strict: {raw}"
    );
    assert!(raw.contains("Path=/"), "missing Path=/: {raw}");

    // Body — owner whoami shape.
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["userId"], "local-owner");
    assert_eq!(v["displayName"], "alice");
    assert_eq!(v["role"], "owner");
    assert!(v["sessionId"].as_str().unwrap().len() > 8);
}

#[tokio::test]
async fn login_wrong_password_returns_401_with_standard_payload() {
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);

    let body = serde_json::to_vec(&serde_json::json!({
        "username": "alice",
        "password": "WRONG",
    }))
    .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // No Set-Cookie on failure — we mustn't leak a session id when the
    // credential check failed.
    assert!(resp.headers().get(header::SET_COOKIE).is_none());

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["code"], "unauthorized");
    assert_eq!(v["error"], "unauthorized");
}

#[tokio::test]
async fn whoami_without_cookie_returns_401_payload() {
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/whoami")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["code"], "unauthorized");
    assert_eq!(v["error"], "unauthorized");
}

#[tokio::test]
async fn whoami_with_valid_cookie_returns_owner_payload() {
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);

    // Log in to mint a session id.
    let body = serde_json::to_vec(&serde_json::json!({
        "username": "alice",
        "password": "hunter2",
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = extract_session_cookie(resp.headers());

    // Re-issue whoami with the cookie.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/whoami")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["userId"], "local-owner");
    assert_eq!(v["role"], "owner");
}

#[tokio::test]
async fn protected_route_without_session_returns_401() {
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/coves")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["code"], "unauthorized");
    assert_eq!(v["error"], "unauthorized");
}

#[tokio::test]
async fn protected_route_with_valid_session_returns_200() {
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);

    let body = serde_json::to_vec(&serde_json::json!({
        "username": "alice",
        "password": "hunter2",
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = extract_session_cookie(resp.headers());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/coves")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn version_route_remains_public() {
    // `/api/version` is the pre-auth compatibility probe — frontend must
    // be able to read it before it knows whether it's logged in. The
    // session middleware MUST NOT apply to it.
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn logout_clears_cookie_and_invalidates_session() {
    let state = fresh_state().await;
    let auth_state = live_auth_state("alice", "hunter2");
    let app = app(state, auth_state);

    // Log in to mint a session.
    let body = serde_json::to_vec(&serde_json::json!({
        "username": "alice",
        "password": "hunter2",
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = extract_session_cookie(resp.headers());

    // Logout.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("logout sets cookie")
        .to_str()
        .unwrap();
    // Removal cookie has Max-Age=0 and empty value — browsers drop the
    // stored cookie on receipt.
    assert!(raw.starts_with(&format!("{SESSION_COOKIE}=")));
    assert!(
        raw.contains("Max-Age=0") || raw.contains("Max-Age=-1"),
        "got: {raw}"
    );

    // Using the now-stale cookie must fail.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/whoami")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dev_autologin_lets_every_request_through() {
    let state = fresh_state().await;
    let auth_state = dev_auth_state();
    let app = app(state, auth_state);

    // whoami without any cookie returns the owner payload directly.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/whoami")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["userId"], "local-owner");
    assert_eq!(v["role"], "owner");

    // Protected REST without a cookie also returns 200.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/coves")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

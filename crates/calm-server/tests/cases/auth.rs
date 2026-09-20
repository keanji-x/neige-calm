//! Integration tests for the auth surface, driving the production router assembly via `oneshot`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, Principal, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

pub(super) async fn fresh_state() -> AppState {
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
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
}

pub(super) fn live_auth_state(user: &str, pass: &str) -> AuthState {
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

/// Exercise the production assembly rather than copying its security layers.
fn app(state: AppState, auth_state: AuthState) -> axum::Router {
    routes::application_router(state, auth_state)
}

fn connect_info(addr: &str) -> axum::extract::ConnectInfo<SocketAddr> {
    axum::extract::ConnectInfo(addr.parse().unwrap())
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

struct HookBoot {
    app: axum::Router,
    repo: Arc<dyn Repo>,
    claude_card_id: String,
    codex_card_id: String,
}

async fn hook_boot() -> HookBoot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "hook-auth".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "hook auth track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let claude_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "claude".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let codex_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    sqlx::query("UPDATE cards SET role = 'worker' WHERE id IN (?1, ?2)")
        .bind(claude_card.id.as_str())
        .bind(codex_card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(claude_card.id.clone(), CardRole::Worker, track.id.clone());
    card_role_cache.insert(codex_card.id.clone(), CardRole::Worker, track.id.clone());

    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

    let events = EventBus::new();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-hook-auth"),
            Vec::new(),
            events.clone(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );

    HookBoot {
        app: app(state, live_auth_state("alice", "hunter2")),
        repo: repo_dyn,
        claude_card_id: claude_card.id.to_string(),
        codex_card_id: codex_card.id.to_string(),
    }
}

async fn assert_hook_event(repo: &Arc<dyn Repo>, card_id: &str, want_kind: &str) {
    let rows = repo.events_since(0, i64::MAX).await.unwrap();
    assert!(
        rows.iter().any(|(_, _, _, ev)| match ev {
            Event::ClaudeHook {
                card_id: id, kind, ..
            }
            | Event::CodexHook {
                card_id: id, kind, ..
            } => id.as_str() == card_id && kind == want_kind,
            _ => false,
        }),
        "expected hook event {want_kind} for card {card_id}; rows: {rows:?}"
    );
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
    // No Set-Cookie on failure — must not leak a session id.
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
                .uri("/api/areas")
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
async fn internal_worker_hooks_bypass_session_gate_but_protected_rest_does_not() {
    let boot = hook_boot().await;

    let protected_resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/areas")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        protected_resp.status(),
        StatusCode::UNAUTHORIZED,
        "protected user REST must still require a session"
    );

    let claude_resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/internal/claude/hook?card_id={}",
                    boot.claude_card_id
                ))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:claude")
                .extension(connect_info("127.0.0.1:12345"))
                .body(Body::from(json!({ "hook_event_name": "Stop" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        claude_resp.status(),
        StatusCode::OK,
        "Claude hook should be accepted without a session cookie"
    );
    assert_hook_event(&boot.repo, &boot.claude_card_id, "hook.claude.stop").await;

    let codex_resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/internal/codex/hook?card_id={}",
                    boot.codex_card_id
                ))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:codex")
                .extension(connect_info("127.0.0.1:12345"))
                .body(Body::from(json!({ "hook_event_name": "Stop" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        codex_resp.status(),
        StatusCode::NO_CONTENT,
        "Codex hook should keep its existing 204 success semantics without a session cookie"
    );
    assert_hook_event(&boot.repo, &boot.codex_card_id, "hook.codex.stop").await;
}

#[tokio::test]
async fn internal_worker_hook_rejects_non_loopback_peer() {
    let boot = hook_boot().await;

    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/internal/claude/hook?card_id={}",
                    boot.claude_card_id
                ))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:claude")
                .extension(connect_info("203.0.113.7:54321"))
                .body(Body::from(json!({ "hook_event_name": "Stop" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "internal hook must reject non-loopback peers"
    );
}

#[tokio::test]
async fn internal_worker_hook_rejects_missing_connect_info() {
    let boot = hook_boot().await;

    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/internal/claude/hook?card_id={}",
                    boot.claude_card_id
                ))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:claude")
                .body(Body::from(json!({ "hook_event_name": "Stop" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "internal hook must fail closed when ConnectInfo is missing"
    );
}

#[tokio::test]
async fn internal_worker_hook_allows_ipv6_loopback_and_rejects_ipv4_mapped_ipv6() {
    let boot = hook_boot().await;

    let ipv6_loopback_resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/internal/claude/hook?card_id={}",
                    boot.claude_card_id
                ))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:claude")
                .extension(connect_info("[::1]:12345"))
                .body(Body::from(json!({ "hook_event_name": "Stop" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        ipv6_loopback_resp.status(),
        StatusCode::OK,
        "internal hook should accept IPv6 loopback peers"
    );
    assert_hook_event(&boot.repo, &boot.claude_card_id, "hook.claude.stop").await;

    let mapped_resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/internal/claude/hook?card_id={}",
                    boot.claude_card_id
                ))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:claude")
                .extension(connect_info("[::ffff:127.0.0.1]:12345"))
                .body(Body::from(json!({ "hook_event_name": "Stop" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        mapped_resp.status(),
        StatusCode::FORBIDDEN,
        "internal hook must reject IPv4-mapped IPv6 peers"
    );
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
                .uri("/api/areas")
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
    // `/api/version` is the pre-auth compatibility probe; the session middleware must not apply to it.
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
    // Removal cookie has Max-Age=0 and an empty value.
    assert!(raw.starts_with(&format!("{SESSION_COOKIE}=")));
    assert!(
        raw.contains("Max-Age=0") || raw.contains("Max-Age=-1"),
        "got: {raw}"
    );

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

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/areas")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn dev_autologin_injects_principal_without_cookie() {
    async fn principal_required(_principal: Principal) -> StatusCode {
        StatusCode::OK
    }

    let auth_state = dev_auth_state();
    let app = axum::Router::new()
        .route(
            "/principal-required",
            axum::routing::get(principal_required),
        )
        .layer(axum::middleware::from_fn_with_state(
            auth_state,
            auth::require_session,
        ));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/principal-required")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

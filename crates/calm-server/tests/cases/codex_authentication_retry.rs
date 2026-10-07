//! Real protected HTTP authority and durable observation CAS; no credential login is performed.
use super::Boot;
use super::codex_provider_availability::{boot_with_account, codex_entry};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{AuthConfig, AuthState, SESSION_COOKIE, SessionAuthority};
use calm_server::codex_appserver::Notification;
use serde_json::{Value, json};
use tower::ServiceExt;
const URI: &str = "/api/agent-providers/codex/retry";
fn fail(boot: &Boot) {
    boot.state.shared_codex_appserver.emit_notification_for_test(Notification::Other {method:"error".into(),params:json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used."}})});
}
fn protected(boot: &Boot) -> (axum::Router, String) {
    let auth = AuthState::new(AuthConfig {
        username: Some("test-owner".into()),
        password: Some("test-password".into()),
        dev_autologin: false,
        display_name: "Test owner".into(),
    });
    let token = auth.sessions.create(SessionAuthority::PasswordLogin);
    (
        calm_server::routes::application_router(boot.state.clone(), auth),
        format!("{SESSION_COOKIE}={token}"),
    )
}
async fn post(
    app: axum::Router,
    cookie: Option<&str>,
    origin: &str,
    actor: &str,
    revision: &str,
) -> (StatusCode, Value) {
    let mut request = Request::post(URI)
        .header(header::HOST, "127.0.0.1:4040")
        .header(header::ORIGIN, origin)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-calm-actor", actor);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let response = app
        .oneshot(
            request
                .body(Body::from(
                    json!({"expected_revision":revision}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    if status == StatusCode::OK {
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    let body =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    (status, body)
}
#[tokio::test]
async fn retry_requires_owner_session_human_actor_and_same_origin() {
    let boot =
        boot_with_account(json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true})).await;
    fail(&boot);
    let revision = codex_entry(&boot).await["authentication_notice"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let (app, cookie) = protected(&boot);
    assert_eq!(
        post(
            app.clone(),
            None,
            "http://127.0.0.1:4040",
            "user",
            &revision
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            app.clone(),
            Some(&cookie),
            "https://other.example.invalid",
            "user",
            &revision
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(
            app,
            Some(&cookie),
            "http://127.0.0.1:4040",
            "ai:another-card",
            &revision
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(
        boot.state
            .shared_codex_appserver
            .authentication_hold()
            .is_some()
    );
}
#[tokio::test]
async fn owner_retry_has_one_persistent_receipt_and_replay_cannot_rearm_again() {
    let boot =
        boot_with_account(json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true})).await;
    fail(&boot);
    let revision = codex_entry(&boot).await["authentication_notice"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let (app, cookie) = protected(&boot);
    let (status, receipt) = post(
        app.clone(),
        Some(&cookie),
        "http://127.0.0.1:4040",
        "user",
        &revision,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["status"], "retry_requested");
    let entry = codex_entry(&boot).await;
    assert_eq!(entry["status"], "ready");
    assert_eq!(entry["authentication_notice"]["kind"], "retry_requested");
    assert_eq!(
        entry["authentication_notice"]["revision"],
        receipt["requested_revision"]
    );
    assert_eq!(
        post(
            app,
            Some(&cookie),
            "http://127.0.0.1:4040",
            "user",
            &revision
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let recovered = calm_server::shared_codex_appserver::SharedCodexAppServer::new(
        &super::cfg(&boot._tmp),
        boot.home.clone(),
        boot.repo.clone(),
    );
    assert_eq!(
        recovered.authentication_notice().unwrap().kind,
        calm_server::codex_authentication::AuthenticationNoticeKind::RetryRequested
    );
    assert_eq!(recovered.authentication_hold(), None);
}

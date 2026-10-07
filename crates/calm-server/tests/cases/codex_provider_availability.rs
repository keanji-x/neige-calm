//! #1817: the Codex entry of `GET /api/agent-providers`, driven through the real
//! `SharedCodexAppServer` and the fake `codex app-server` child of `models_endpoint.rs` (its
//! `account/read` answer is the `<sock>.account-read` sidecar).

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{Boot, boot};

async fn codex_entry(boot: &Boot) -> Value {
    let request = Request::builder()
        .uri("/api/agent-providers")
        .header("x-calm-actor", "user")
        .body(Body::empty())
        .unwrap();
    let response = boot.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    body.as_array()
        .expect("an array")
        .iter()
        .find(|entry| entry["provider"] == "codex")
        .cloned()
        .unwrap_or_else(|| panic!("no codex entry: {body}"))
}

async fn boot_with_account(account: Value) -> Boot {
    boot(true, |sock| {
        std::fs::write(sock.with_extension("account-read"), account.to_string()).unwrap();
    })
    .await
}

#[tokio::test]
async fn codex_without_a_running_daemon_is_unavailable_with_the_supervisor_reason() {
    let boot = boot(false, |_sock| {}).await;
    let codex = codex_entry(&boot).await;
    assert_eq!(codex["status"], "unavailable", "{codex}");
    assert!(
        codex["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("shared codex app-server is not running")),
        "{codex}"
    );
    assert!(
        !boot.methods_seen().contains(&"account/read".to_string()),
        "no daemon, nothing asked"
    );
}

#[tokio::test]
async fn codex_logged_in_by_an_account_or_without_openai_auth_is_ready() {
    for account in [
        json!({"account": {"type": "apiKey"}, "requiresOpenaiAuth": true}),
        json!({
            "account": {"type": "chatgpt", "email": "owner@example.invalid", "planType": "plus"},
            "requiresOpenaiAuth": true,
        }),
        json!({"account": null, "requiresOpenaiAuth": false}),
    ] {
        let boot = boot_with_account(account.clone()).await;
        let codex = codex_entry(&boot).await;
        assert_eq!(codex["status"], "ready", "{account}: {codex}");
        assert_eq!(codex["reason"], Value::Null, "{account}: {codex}");
        let text = codex.to_string();
        assert!(
            !text.contains("owner@example.invalid") && !text.contains("plus"),
            "{text}"
        );
        assert!(boot.methods_seen().contains(&"account/read".to_string()));
    }
}

#[tokio::test]
async fn codex_without_an_account_that_needs_one_is_unavailable() {
    let boot = boot_with_account(json!({"account": null, "requiresOpenaiAuth": true})).await;
    let codex = codex_entry(&boot).await;
    assert_eq!(codex["status"], "unavailable", "{codex}");
    let reason = codex["reason"].as_str().expect("a reason");
    assert!(
        reason.starts_with("codex is not logged in — run `codex login` with CODEX_HOME="),
        "{reason}"
    );
}

/// Codex create is not gated (#293): an unavailable Codex still answers a create with 201.
#[tokio::test]
async fn an_unavailable_codex_still_creates_a_track() {
    let boot = boot_with_account(json!({"account": null, "requiresOpenaiAuth": true})).await;
    assert_eq!(codex_entry(&boot).await["status"], "unavailable");
    let area = boot
        .repo
        .area_create(calm_server::model::NewArea {
            name: "codex down".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/api/tracks")
        .header("x-calm-actor", "user")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "planner_provider": "codex",
                "area_id": area.id,
                "title": "still created",
                "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
            })
            .to_string(),
        ))
        .unwrap();
    let response = boot.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

#[tokio::test]
async fn an_undecodable_account_read_is_unavailable_never_a_guess() {
    let boot = boot_with_account(json!({"account": null})).await;
    let codex = codex_entry(&boot).await;
    assert_eq!(codex["status"], "unavailable", "{codex}");
    assert!(
        codex["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("could not ask the shared codex app-server")),
        "{codex}"
    );
}

/// An account is still present after its refresh credential stops working (#2314).
#[tokio::test]
async fn a_refresh_token_reuse_refusal_overrides_cached_account_presence() {
    let boot = boot(true, |sock| {
        std::fs::write(
            sock.with_extension("account-read"),
            json!({
                "account": {"type":"chatgpt"}, "requiresOpenaiAuth":true
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            sock.with_extension("turn-start-refusal"),
            "Your access token could not be refreshed because your refresh token was already used. \
             Sign in again. fixture-private-value",
        )
        .unwrap();
    })
    .await;
    assert_eq!(codex_entry(&boot).await["status"], "ready");
    let failure = boot
        .state
        .shared_codex_appserver
        .turn_start(
            "auth-regression-thread",
            vec![calm_server::codex_appserver::InputItem::text(
                "auth regression",
            )],
            &calm_server::planner_model::TurnModelSelection::inherit(),
            None,
        )
        .await;
    assert!(
        failure.is_err(),
        "positive control: the native provider refused the request"
    );
    assert!(
        boot.methods_seen()
            .iter()
            .any(|method| method == "turn/start")
    );
    let status = codex_entry(&boot).await;
    assert_eq!(
        status["status"], "unavailable",
        "cached account presence must not hide a known authentication failure"
    );
    let reason = status["reason"].as_str().expect("an actionable reason");
    assert!(reason.to_lowercase().contains("sign in"), "{reason}");
    assert!(
        !reason.contains("fixture-private-value"),
        "native sensitive detail must not reach the owner"
    );
    assert_eq!(
        boot.methods_seen()
            .iter()
            .filter(|method| method.as_str() == "turn/start")
            .count(),
        1,
        "status reads do not replay model work"
    );
}

#[tokio::test]
async fn authentication_failure_survives_recheck_and_clears_only_on_confirmed_login() {
    let boot =
        boot_with_account(json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true})).await;
    assert_eq!(codex_entry(&boot).await["status"], "ready");
    let daemon = &boot.state.shared_codex_appserver;
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method:"error".into(), params:json!({"error":{"message":"Your access token could not be refreshed because your refresh token has expired. private-fixture-detail"}}),
    });
    assert_eq!(codex_entry(&boot).await["status"], "unavailable");
    for params in [
        json!({"success":false,"error":"failed"}),
        json!({"success":true}),
        json!({"success":"true","error":null}),
    ] {
        daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
            method: "account/login/completed".into(),
            params,
        });
        assert_eq!(codex_entry(&boot).await["status"], "unavailable");
    }
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    let request = Request::builder()
        .uri("/api/agent-providers?refresh=true")
        .header("x-calm-actor", "user")
        .body(Body::empty())
        .unwrap();
    let response = boot.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    let codex = body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["provider"] == "codex")
        .unwrap();
    assert_eq!(
        codex["status"], "unavailable",
        "account presence and ordinary rechecks are not repair proof"
    );
    assert!(
        !codex["reason"]
            .as_str()
            .unwrap()
            .contains("private-fixture-detail")
    );
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "account/login/completed".into(),
        params: json!({"loginId":"fixture-login","success":true,"error":null}),
    });
    assert_eq!(codex_entry(&boot).await["status"], "ready");
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::TurnCompleted {
        thread_id:"fixture-thread".into(),turn:json!({"id":"fixture-turn","status":"failed","error":{"message":"Your access token could not be refreshed because your refresh token was revoked."}}),
    });
    assert_eq!(
        codex_entry(&boot).await["status"],
        "unavailable",
        "a later episode must remain visible"
    );
}

#[tokio::test]
async fn authentication_refusal_explains_sign_in_instead_of_changing_the_model() {
    let boot = boot(true, |sock| {
        std::fs::write(
            sock.with_extension("turn-start-refusal"),
            "Your access token could not be refreshed because your refresh token was revoked. private-fixture-detail",
        )
        .unwrap();
    })
    .await;
    let backend: calm_server::harness::backend::PlannerBackend =
        boot.state.shared_codex_appserver.clone().into();
    let failure = backend
        .turn_start(
            "auth-reader-thread",
            vec![calm_server::codex_appserver::InputItem::text(
                "auth regression",
            )],
            &calm_server::planner_model::TurnModelSelection::inherit(),
            "fixture-client-id",
            None,
        )
        .await
        .expect_err("a native authentication refusal");
    let calm_server::harness::backend::TurnStartFailure::Refused { error, reader } = failure else {
        panic!("a definitive authentication refusal must not be transient");
    };
    assert!(reader.to_lowercase().contains("sign in"));
    assert!(!reader.contains("try another") && !reader.contains("private-fixture-detail"));
    assert!(reader.contains("Your message is still queued"));
    assert!(!error.to_string().contains("private-fixture-detail"));
}

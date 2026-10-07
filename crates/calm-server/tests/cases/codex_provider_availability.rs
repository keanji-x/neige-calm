//! #1817: the Codex entry of `GET /api/agent-providers`, driven through the real
//! `SharedCodexAppServer` and the fake `codex app-server` child of `models_endpoint.rs` (its
//! `account/read` answer is the `<sock>.account-read` sidecar).

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{Boot, boot};

pub(super) async fn codex_entry(boot: &Boot) -> Value {
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

pub(super) async fn boot_with_account(account: Value) -> Boot {
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
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::TurnStarted {
        thread_id: "fixture-thread".into(),
        turn: json!({"id":"fixture-turn"}),
    });
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
    let calm_server::harness::backend::TurnStartFailure::AwaitingRecovery { error, reader } =
        failure
    else {
        panic!("a definitive authentication refusal must not be transient");
    };
    assert!(reader.to_lowercase().contains("sign in"));
    assert!(!reader.contains("try another") && !reader.contains("private-fixture-detail"));
    assert!(reader.contains("Your message is still queued"));
    assert!(!error.to_string().contains("private-fixture-detail"));
}

#[tokio::test]
async fn a_pre_login_rpc_refusal_cannot_overwrite_a_verified_login() {
    let wait_path = std::sync::Mutex::new(None);
    let boot = boot(true, |sock| {
        let path = sock.with_extension("turn-start-refusal-wait");
        std::fs::write(&path, "wait").unwrap();
        *wait_path.lock().unwrap() = Some(path);
        std::fs::write(
            sock.with_extension("turn-start-refusal"),
            "Your access token could not be refreshed because your refresh token was already used.",
        )
        .unwrap();
    })
    .await;
    let daemon = boot.state.shared_codex_appserver.clone();
    let request = tokio::spawn(async move {
        daemon
            .turn_start(
                "old-auth-request",
                vec![calm_server::codex_appserver::InputItem::text("auth")],
                &calm_server::planner_model::TurnModelSelection::inherit(),
                None,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !boot
            .methods_seen()
            .iter()
            .any(|method| method == "turn/start")
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the old request reached the native transport");
    boot.state
        .shared_codex_appserver
        .emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
            method: "account/login/completed".into(),
            params: json!({"success":true,"error":null}),
        });
    std::fs::remove_file(wait_path.lock().unwrap().take().unwrap()).unwrap();
    assert!(request.await.unwrap().is_err());
    assert_eq!(
        codex_entry(&boot).await["status"],
        "ready",
        "an old failure cannot replace newer login proof"
    );
}

#[tokio::test]
async fn cached_authentication_warning_keeps_its_check_timestamp() {
    let boot =
        boot_with_account(json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true})).await;
    boot.state.shared_codex_appserver.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method:"error".into(),params:json!({"error":{"message":"Your access token could not be refreshed because your refresh token has expired."}}),
    });
    let first = codex_entry(&boot).await;
    let methods = boot.methods_seen();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let second = codex_entry(&boot).await;
    assert_eq!(first["status"], "unavailable");
    assert_eq!(first["checked_at_ms"], second["checked_at_ms"]);
    assert_eq!(methods, boot.methods_seen());
}

#[tokio::test]
async fn a_pre_login_turn_error_cannot_overwrite_a_verified_login() {
    let boot =
        boot_with_account(json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true})).await;
    let daemon = &boot.state.shared_codex_appserver;
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::TurnStarted {
        thread_id: "old-thread".into(),
        turn: json!({"id":"old-turn"}),
    });
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "account/login/completed".into(),
        params: json!({"success":true,"error":null}),
    });
    let error = json!({"message":"Your access token could not be refreshed because your refresh token was already used."});
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "error".into(),
        params: json!({"threadId":"old-thread","turnId":"old-turn","error":error}),
    });
    assert_eq!(codex_entry(&boot).await["status"], "ready");
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::TurnCompleted {
        thread_id: "old-thread".into(),
        turn: json!({"id":"old-turn","status":"failed","error":error}),
    });
    assert_eq!(codex_entry(&boot).await["status"], "ready");
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::TurnStarted {
        thread_id: "old-thread".into(),
        turn: json!({"id":"new-turn"}),
    });
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "error".into(),
        params: json!({"threadId":"old-thread","turnId":"new-turn","error":error}),
    });
    assert_eq!(codex_entry(&boot).await["status"], "unavailable");
}

#[tokio::test]
async fn stderr_refresh_failure_is_visible_without_claiming_current_login_failed() {
    use std::io::Write;
    let boot =
        boot_with_account(json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true})).await;
    assert_eq!(codex_entry(&boot).await["status"], "ready");
    let log = boot
        ._tmp
        .path()
        .join("logs/shared-codex-appserver/stderr.log");
    let mut file = std::fs::OpenOptions::new().append(true).open(log).unwrap();
    writeln!(file, "2026-10-07T01:00:00Z ERROR codex_core::auth: Failed to refresh token: \
    Your access token could not be refreshed because your refresh token was already used. private-fixture-detail").unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let entry = codex_entry(&boot).await;
        if entry["authentication_notice"]["kind"] == "refresh_error_reported" {
            assert_eq!(
                entry["status"], "ready",
                "an unattributed log is not a current login verdict"
            );
            assert!(!entry.to_string().contains("private-fixture-detail"));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "stderr auth failure was silent: {entry}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn confirmed_authentication_failure_survives_server_reconstruction() {
    let boot = boot(true, |sock| {
        std::fs::write(
            sock.with_extension("turn-start-refusal"),
            "Your access token could not be refreshed because your refresh token was already used.",
        )
        .unwrap();
    })
    .await;
    let _ = boot
        .state
        .shared_codex_appserver
        .turn_start(
            "fixture-thread",
            vec![calm_server::codex_appserver::InputItem::text("queued")],
            &calm_server::planner_model::TurnModelSelection::inherit(),
            None,
        )
        .await;
    assert!(
        boot.state
            .shared_codex_appserver
            .authentication_failure()
            .is_some()
    );
    let recovered = calm_server::shared_codex_appserver::SharedCodexAppServer::new(
        &super::cfg(&boot._tmp),
        boot.home.clone(),
        boot.repo.clone(),
    );
    assert!(
        recovered.authentication_failure().is_some(),
        "a server restart lost the confirmed authentication failure"
    );
}

#[tokio::test]
async fn rotated_stderr_still_observes_the_daemons_open_log_file() {
    use std::io::Write;
    let boot =
        boot_with_account(json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true})).await;
    let log = boot
        ._tmp
        .path()
        .join("logs/shared-codex-appserver/stderr.log");
    // Like the daemon's inherited stderr descriptor, this remains attached to the original inode.
    let mut producer = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    std::fs::rename(&log, log.with_extension("old")).unwrap();
    std::fs::write(&log, "").unwrap();
    writeln!(
        producer,
        "2026-10-07T00:00:00Z ERROR codex_core::auth: Failed to refresh token: \
    Your access token could not be refreshed because your refresh token was already used."
    )
    .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let entry = codex_entry(&boot).await;
        if entry["authentication_notice"]["kind"] == "refresh_error_reported" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "rotation hid the daemon's stderr: {entry}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_hold_arriving_during_connection_acquisition_prevents_native_dispatch() {
    use std::future::Future;
    use std::task::Poll;
    let boot =
        boot_with_account(json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true})).await;
    let daemon = &boot.state.shared_codex_appserver;
    let guard = daemon.hold_connection_read_for_test().await;
    let selection = calm_server::planner_model::TurnModelSelection::inherit();
    let mut call = std::pin::pin!(daemon.turn_start(
        "held-thread",
        vec![calm_server::codex_appserver::InputItem::text(
            "must stay queued"
        )],
        &selection,
        None
    ));
    std::future::poll_fn(|cx| {
        assert!(
            call.as_mut().poll(cx).is_pending(),
            "the real connection read is held"
        );
        Poll::Ready(())
    })
    .await;
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {method:"error".into(),params:json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used."}})});
    drop(guard);
    assert!(
        call.await.is_err(),
        "a newly held request cannot dispatch after obtaining the client"
    );
    assert!(
        !boot
            .methods_seen()
            .iter()
            .any(|method| method == "turn/start"),
        "no native request is sent through the admission gap"
    );
}

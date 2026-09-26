//! #1817: the Codex entry of `GET /api/agent-providers`, driven through the real
//! `SharedCodexAppServer` and the fake `codex app-server` child of `models_endpoint.rs` (its
//! `account/read` answer is the `<sock>.account-read` sidecar).

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
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

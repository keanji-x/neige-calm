//! Native ACP authentication stays independent of the shared Codex daemon.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_does_not_inherit_codex_authentication_hold_or_notice() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let daemon = &stack.state.shared_codex_appserver;
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "error".into(), params: json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used.","codexErrorInfo":"unauthorized"}}),
    });
    assert!(daemon.authentication_hold().is_some());
    let (_, card) = create(&stack).await;
    assert_eq!(
        turn(&stack, &card, "independent native authentication", 1).await["status"],
        "completed"
    );
    let (status, providers) = stack.send("GET", "/api/agent-providers", None).await;
    assert_eq!(status, StatusCode::OK);
    let providers = providers.as_array().unwrap();
    let acp = providers
        .iter()
        .find(|p| p["provider"] == "opencode")
        .unwrap();
    let codex = providers.iter().find(|p| p["provider"] == "codex").unwrap();
    assert!(acp["authentication_notice"].is_null());
    assert!(!codex["authentication_notice"].is_null());
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

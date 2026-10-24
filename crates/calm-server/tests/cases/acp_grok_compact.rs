use super::acp_compact::{compact, human_state, snapshot, turn, wait_phase};
use super::*;

pub(super) async fn boot_grok(root: &Root) -> Stack {
    boot_as(root, "grok", json!({"ACP_FIXTURE_IDENTITY":"meta", "ACP_FIXTURE_VERSION":"1.0.70", "ACP_FIXTURE_AUTH_METHODS":"cached_token", "ACP_FIXTURE_VENDOR":"xai"}), "1.0.62").await
}
pub(super) async fn create_grok(stack: &Stack) -> (String, String) {
    stack
        .create_claude_track_with(
            json!({"planner_provider":"grok", "template_id":null, "attach_folder":false}),
        )
        .await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_success_preserves_inputs_and_settles_once() {
    super::acp_compact::success(true).await;
}

async fn unconfirmed(scenario: &str) {
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (_, card) = create_grok(&stack).await;
    turn(&stack, &card, "first", 1).await;
    let humans = human_state(&stack, &card).await;
    let before = snapshot(&stack, &card).await;
    let runtime = stack.runtime(&card).await;
    std::fs::write(root.path().join("scenario"), scenario).unwrap();
    let (status, body) = compact(&stack, &card).await;
    assert_eq!(status, StatusCode::OK, "{scenario}: {body}");
    wait_phase(&stack, &runtime.id, "wedged").await;
    let after = snapshot(&stack, &card).await;
    assert_eq!(
        after["compact_outcome"]["status"], "unconfirmed",
        "{scenario}: {after}"
    );
    assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
    assert_eq!(human_state(&stack, &card).await, humans);
    assert_eq!(stack.outcomes(&card).await.len(), 1);
    assert_eq!(stack.runtime(&card).await.session_id, runtime.session_id);
    assert!(!compact(&stack, &card).await.0.is_success());
    assert_eq!(requests(&root, "_x.ai/compact_conversation").len(), 1);
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
    let stack = boot_grok(&root).await;
    assert_eq!(
        stack.post_input(&card, "pending after restart").await.0,
        StatusCode::OK
    );
    wait_phase(&stack, &runtime.id, "wedged").await;
    assert_ne!(
        snapshot(&stack, &card).await["compact_outcome"]["status"],
        "completed"
    );
    assert_eq!(requests(&root, "_x.ai/compact_conversation").len(), 1);
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_compact_failed_never_completes() {
    for s in [
        "grok-compact-failed",
        "grok-compact-failed-live",
        "grok-compact-unknown",
        "grok-compact-failed-mixed",
    ] {
        unconfirmed(s).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_requires_live_completion() {
    for s in [
        "grok-compact-empty",
        "grok-compact-summary",
        "grok-compact-standard",
        "grok-compact-invalid-response",
        "grok-compact-invalid-response-mixed",
    ] {
        unconfirmed(s).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_replay_only_unconfirmed() {
    for s in ["grok-compact-replay", "grok-compact-replay-method"] {
        unconfirmed(s).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_wrong_session_unconfirmed() {
    for s in [
        "grok-compact-wrong-session",
        "grok-compact-wrong-session-then-valid",
    ] {
        unconfirmed(s).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_lost_response_and_death_restart_unconfirmed() {
    for s in [
        "grok-compact-lost-response",
        "grok-compact-death",
        "grok-compact-lost-response-mixed",
        "grok-compact-death-mixed",
    ] {
        unconfirmed(s).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_cleanup_failure_unconfirmed() {
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (_, card) = create_grok(&stack).await;
    turn(&stack, &card, "first", 1).await;
    let runtime = stack.runtime(&card).await;
    let cleanup = calm_server::acp_planner::test_seams::pause_after_fence(&runtime.id);
    std::fs::write(root.path().join("scenario"), "grok-compact-success").unwrap();
    assert_eq!(compact(&stack, &card).await.0, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(20), cleanup.entered.notified())
        .await
        .unwrap();
    let pool = stack.repo().sqlite_db().unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_grok_revoke BEFORE UPDATE OF mcp_token_hash ",
        "ON worker_sessions WHEN NEW.mcp_token_hash IS NULL ",
        "BEGIN SELECT RAISE(FAIL,'fixture revoke failure'); END",
    ))
    .execute(pool.writer_pool())
    .await
    .unwrap();
    cleanup.release.notify_one();
    wait_phase(&stack, &runtime.id, "wedged").await;
    assert_eq!(
        snapshot(&stack, &card).await["compact_outcome"]["status"],
        "unconfirmed"
    );
    sqlx::query("DROP TRIGGER refuse_grok_revoke")
        .execute(pool.writer_pool())
        .await
        .unwrap();
    assert_eq!(requests(&root, "_x.ai/compact_conversation").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_grok_compact_historical_assistant_refuses() {
    historical_assistant_refuses().await;
}
pub(super) async fn historical_assistant_refuses() {
    use axum::http::Request;
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (track, _) = super::acp_grok_provider::create_grok(&stack).await;
    std::fs::write(root.path().join("scenario"), "assistant-mcp").unwrap();
    let (status, row) = stack
        .send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", "grok-compact-assistant"),
            Some(json!({"provider":"grok","text":"first"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    stack.wait_outcomes(card, 1).await;
    wait_phase(&stack, &stack.runtime(card).await.id, "turn_completed").await;
    let (status, run) = stack
        .send("GET", &format!("/api/cards/{card}/planner/run"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["manual_compact_supported"], false, "{run}");
    let (status, refusal) = compact(&stack, card).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refusal}");
    assert!(requests(&root, "_x.ai/compact_conversation").is_empty());
    for request in requests(&root, "initialize") {
        assert_eq!(request["params"]["clientCapabilities"], json!({}));
    }
    stack.shutdown().await;
}

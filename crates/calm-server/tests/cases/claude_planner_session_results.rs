//! Drive result optionality through the real session, using only fake CLI output.

use super::claude_planner_session_fixture::{completed_turn, until_completed};
use super::claude_planner_stack_fixture::{Root, Stack};

async fn result_turn(scenario: &str, status: &str, error: Option<&str>) {
    let root = Root::new(scenario);
    let stack = Stack::boot(&root).await;
    let (_track, card) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card).await;
    let session = stack
        .harness(&runtime.id)
        .claude_session_for_test()
        .unwrap();
    let mut rx = session.subscribe_events();
    let (code, body) = stack.post_input(&card, "hello").await;
    assert_eq!(code, axum::http::StatusCode::OK, "{body}");
    let completed = completed_turn(&until_completed(&mut rx).await);
    let turn = completed["id"].as_str().expect("completed turn ID");
    assert!(!turn.is_empty());
    assert_eq!(completed["status"], status, "{completed}");
    match error {
        Some(message) => assert_eq!(completed["error"]["message"], message),
        None => assert!(completed["error"].is_null(), "{completed}"),
    }
    stack.wait_phase(&runtime.id, "turn_completed").await;
    let outcomes = stack.outcomes(&card).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["id"], turn);
    assert_eq!(outcomes[0]["status"], status);
    assert_eq!(
        session.active_turn_id_for_thread(runtime.thread_id.as_deref().expect("bound thread")),
        None
    );
    assert_eq!(
        stack
            .harness(&runtime.id)
            .snapshot()
            .await
            .unwrap()
            .last_turn_id
            .as_deref(),
        Some(turn)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_terminal_reason_completes() {
    result_turn("result-no-reason", "completed", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn null_terminal_reason_completes() {
    result_turn("result-null-reason", "completed", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn is_error_without_terminal_reason_still_fails() {
    result_turn(
        "result-error-no-reason",
        "failed",
        Some("fake result error"),
    )
    .await;
}

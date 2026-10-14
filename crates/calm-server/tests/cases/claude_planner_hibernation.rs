use super::claude_planner_stack_fixture::{Root, Stack};
use axum::http::StatusCode;
use calm_server::claude_planner::stop::{
    clear_claude_planner_stop_failure_for_test, fail_claude_planner_stop_for_test,
};
use serde_json::json;

async fn sleep_and_wake(fail: bool, reopen: bool) {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track, card) = stack.create_claude_track().await;
    let (outcome, _) = stack.run_turn(&root, &card, "exit", "before sleep").await;
    assert_eq!(outcome["status"], "completed");
    let runtime = stack.runtime(&card).await;
    let (status, body) = stack
        .send(
            "PATCH",
            &format!("/api/tracks/{track}"),
            Some(json!({"closed":true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    if fail {
        fail_claude_planner_stop_for_test(&runtime.id);
    }
    calm_server::terminal_sweeper::sweep(&stack.state)
        .await
        .unwrap();
    let remains = stack.state.harness.get(&runtime.id).is_some();
    clear_claude_planner_stop_failure_for_test(&runtime.id);
    assert!(
        !remains,
        "provider release failure must not leave an input-closed owner registered"
    );
    if reopen {
        let (status, body) = stack
            .send(
                "PATCH",
                &format!("/api/tracks/{track}"),
                Some(json!({"closed":false})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(stack.state.harness.get(&runtime.id).is_some());
    }
    let (outcome, _) = stack.run_turn(&root, &card, "exit", "after sleep").await;
    assert_eq!(outcome["status"], "completed");
    let awakened = stack.runtime(&card).await;
    assert_eq!(awakened.id, runtime.id);
    assert_eq!(awakened.thread_id, runtime.thread_id);
    assert_eq!(stack.outcomes(&card).await.len(), 2);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_hibernat_claude_release_failure_then_send() {
    sleep_and_wake(true, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_hibernat_claude_release_failure_then_reopen() {
    sleep_and_wake(true, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_hibernat_claude_sleep_then_send() {
    sleep_and_wake(false, false).await;
}

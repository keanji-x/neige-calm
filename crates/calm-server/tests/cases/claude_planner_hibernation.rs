use super::claude_planner_stack_fixture::{Root, Stack};
use axum::http::StatusCode;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_hibernat_claude_sleep_then_reopen() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track, card) = stack.create_claude_track().await;
    let (outcome, _) = stack.run_turn(&root, &card, "exit", "before sleep").await;
    assert_eq!(outcome["status"], "completed");
    let runtime = stack.runtime(&card).await;
    // A legacy closed row still uses idle hibernation. An explicit close request
    // now retires the Planner through the strict lifecycle tested below.
    stack
        .repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&track))
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&stack.state)
        .await
        .unwrap();
    let remains = stack.state.harness.get(&runtime.id).is_some();
    assert!(
        !remains,
        "provider release failure must not leave an input-closed owner registered"
    );
    let (status, body) = stack
        .send(
            "PATCH",
            &format!("/api/tracks/{track}"),
            Some(json!({"closed":false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(stack.state.harness.get(&runtime.id).is_some());
    let (outcome, _) = stack.run_turn(&root, &card, "exit", "after sleep").await;
    assert_eq!(outcome["status"], "completed");
    let awakened = stack.runtime(&card).await;
    assert_eq!(awakened.id, runtime.id);
    assert_eq!(awakened.thread_id, runtime.thread_id);
    assert_eq!(stack.outcomes(&card).await.len(), 2);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_cancels_running_claude_planner_and_reopen_waits_for_new_input() {
    use super::claude_planner_session_fixture::alive;
    use super::claude_planner_wiring::wait_file;
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (track, card) = stack.create_claude_track().await;
    assert_eq!(
        stack.post_input(&card, "keep working").await.0,
        StatusCode::OK
    );
    wait_file(&root, "stdin").await;
    let pid: i32 = root.read_fake("pid").unwrap().trim().parse().unwrap();
    assert!(alive(pid));
    let runtime = stack.runtime(&card).await;
    let (status, body) = stack
        .send(
            "PATCH",
            &format!("/api/tracks/{track}"),
            Some(json!({"closed":true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    stack.wait_provider_clear(&runtime.id).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let pending: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1",
            )
            .bind(&runtime.id)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
            if pending == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!alive(pid));
    assert!(stack.state.harness.get(&runtime.id).is_none());
    let stopped_pid = root.read_fake("pid");
    let stopped_input = root.read_fake("stdin");
    let outcomes = stack.outcomes(&card).await;
    assert_eq!(outcomes.len(), 1);
    let (status, body) = stack
        .send(
            "PATCH",
            &format!("/api/tracks/{track}"),
            Some(json!({"closed":false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Reopen may restore control. It must not restart the old process or input.
    let reopened = stack.runtime(&card).await;
    assert_eq!(reopened.id, runtime.id);
    assert_eq!(reopened.thread_id, runtime.thread_id);
    assert_eq!(root.read_fake("pid"), stopped_pid);
    assert_eq!(root.read_fake("stdin"), stopped_input);
    assert_eq!(stack.outcomes(&card).await, outcomes);
    assert!(!alive(pid));
    let (outcome, _) = stack
        .run_turn(&root, &card, "exit", "Fresh Claude work after Close")
        .await;
    assert_eq!(outcome["status"], "completed");
    assert_eq!(stack.outcomes(&card).await.len(), 2);
    stack.shutdown().await;
}

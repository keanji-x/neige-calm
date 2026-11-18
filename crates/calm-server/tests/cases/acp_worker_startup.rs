use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_new_reply_releases_only_after_live_sibling_and_generation_retire() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (sibling_track, sibling_planner) = declare(&stack, payload("sibling", "read_only")).await;
    let sibling = wait_task(&stack, &sibling_track, "sibling", TaskStatus::Running).await;
    std::fs::write(root.path().join("worker-drop-new"), "drop").unwrap();
    let (track, _) = declare(&stack, payload("lost-new", "read_only")).await;
    wait_file(&root, "worker-new-lost").await;
    let task = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if let Some(task) = stack
                .repo()
                .task_current_get(&track, "lost-new")
                .await
                .unwrap()
                && task.worker_card_id.is_some()
            {
                let runtime = stack.runtime(task.worker_card_id.as_deref().unwrap()).await;
                let revoked: bool = sqlx::query_scalar(
                    "SELECT mcp_token_hash IS NULL FROM worker_sessions WHERE id=?1",
                )
                .bind(runtime.id)
                .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
                .await
                .unwrap();
                if revoked {
                    break task;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("setup failure revokes the credential");
    assert_eq!(held(&stack, &task).await, 1);
    assert!(!state(&stack, &task).await.cleanup_confirmed);
    assert_eq!(state(&stack, &task).await.native_session_id, None);
    assert_eq!(held(&stack, &sibling).await, 1);
    assert_eq!(
        stack
            .repo()
            .task_get(&sibling.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    cancel(&root, &stack, &sibling_planner, &sibling).await;
    wait_released(&stack, &task).await;
    assert!(state(&stack, &task).await.cleanup_confirmed);
    assert!(!state(&stack, &task).await.goal_submitted);
    assert_eq!(
        requests(&root, "session/prompt").len(),
        1,
        "a lost attachment never replays the goal"
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn denied_permission_silent_end_projects_worker_permission_diagnostic() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-permissions-silent").await;
    let (track, _) = declare(&stack, payload("permission-diagnostic", "read_only")).await;
    let task = wait_task(&stack, &track, "permission-diagnostic", TaskStatus::Failed).await;
    wait_released(&stack, &task).await;
    let rows = stack
        .repo()
        .worker_flow_item_list_by_card(task.worker_card_id.as_deref().unwrap(), 0, 100, false)
        .await
        .unwrap();
    let ended = rows
        .iter()
        .filter_map(|row| serde_json::from_str::<Value>(&row.payload).ok())
        .find(|item| item["type"] == "turnEnded")
        .expect("native terminal activity");
    assert_eq!(
        ended["provider_extra"]["turn"]["error"]["code"], "acp_permission_rejected",
        "{ended}"
    );
    stack.shutdown().await;
}

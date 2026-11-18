use super::*;

async fn release_events(stack: &Stack, task: &Task) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind='workspace.released'
         AND json_extract(payload,'$.card_id')=?1",
    )
    .bind(task.worker_card_id.as_deref().unwrap())
    .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap()
}

async fn native_release_before_sweep(restore_marker: bool) {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("cancel-cleanup", "read_only")).await;
    let task = wait_task(&stack, &track, "cancel-cleanup", TaskStatus::Running).await;
    let session = stack.runtime(task.worker_card_id.as_deref().unwrap()).await;
    // Block the production cleanup sweep, including the cancel tool's immediate poke.
    // Native settlement does not need this barrier and must release the lease first.
    let drive = stack
        .state
        .operation_runtime
        .lock_for_track_delete_for_test()
        .await;
    call(
        &stack,
        identity(&stack, &planner, CardRole::Planner).await,
        "neige_task_cancel",
        json!({"key":task.key,"message":"settle before scheduler cleanup"}),
    )
    .await
    .unwrap();
    let marker = state(&stack, &task).await.timeout_cleanup.unwrap();
    assert_eq!(marker.task_id, task.id);
    assert_eq!(held(&stack, &task).await, 1);
    settle(&root, &stack, &task).await;
    assert_eq!(held(&stack, &task).await, 0);

    if restore_marker {
        // Reproduce an already-released lease with a durable retry marker (e.g. an
        // older native finalizer). The scheduler must commit without release events.
        stack
            .repo()
            .sqlite_db()
            .unwrap()
            .writer()
            .execute(
                sqlx::query(
                    "UPDATE worker_sessions SET handle_state_json =
                     json_set(handle_state_json, '$.timeout_cleanup', json(?2)) WHERE id=?1",
                )
                .bind(&session.id)
                .bind(serde_json::to_string(&marker).unwrap()),
            )
            .await
            .unwrap();
        assert!(state(&stack, &task).await.timeout_cleanup.is_some());
    } else {
        assert!(
            state(&stack, &task).await.timeout_cleanup.is_none(),
            "native release must atomically retire its exact attempt's cleanup marker"
        );
    }
    let releases = release_events(&stack, &task).await;
    assert_eq!(releases, 1);
    let task_before = stack.repo().task_get(&task.id).await.unwrap();
    assert_eq!(task_before.as_ref().unwrap().status, TaskStatus::Canceled);
    drop(drive);

    let scheduler = stack.state.scheduler_for_test();
    for _ in 0..2 {
        scheduler.sweep_all().await;
        assert!(
            state(&stack, &task).await.timeout_cleanup.is_none(),
            "cleanup must converge even when native settlement already released the lease"
        );
        assert_eq!(held(&stack, &task).await, 0);
        assert_eq!(release_events(&stack, &task).await, releases);
        assert_eq!(stack.repo().task_get(&task.id).await.unwrap(), task_before);
    }
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canceled_native_release_retires_timeout_cleanup_before_sweep() {
    native_release_before_sweep(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_cleanup_retires_marker_after_native_lease_release() {
    native_release_before_sweep(true).await;
}

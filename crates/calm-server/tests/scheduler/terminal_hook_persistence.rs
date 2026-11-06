use super::*;

#[tokio::test]
async fn completion_error_returns_to_caller_and_can_be_retried() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "hook-write-error", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    seed_worker_op_target(&boot, "terminal-worker", &task_id, card_id.as_str()).await;
    let db = boot.repo.sqlite_db().unwrap();
    db.writer()
        .execute(sqlx::query(&format!(
            "CREATE TRIGGER reject_hook BEFORE UPDATE OF status ON tasks \
             WHEN NEW.id='{task_id}' AND NEW.status='done' \
             BEGIN SELECT RAISE(ABORT,'reject task completion'); END"
        )))
        .await
        .unwrap();
    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    let error = hook
        .on_terminal_exit(&terminal_id, Some(0), false, "done", false)
        .await
        .expect_err("the completion transaction failed");
    assert!(matches!(error, calm_server::error::CalmError::Db(_)));
    assert_eq!(
        task_row(&boot, "hook-write-error").await.status,
        TaskStatus::Running
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    db.writer()
        .execute(sqlx::query("DROP TRIGGER reject_hook"))
        .await
        .unwrap();
    hook.on_terminal_exit(&terminal_id, Some(0), false, "done", false)
        .await
        .unwrap();
    assert_eq!(
        task_row(&boot, "hook-write-error").await.status,
        TaskStatus::Done
    );
    assert_eq!(event_rows(&boot, "task.completed").await.len(), 1);
}

#[tokio::test]
async fn pipe_lookup_error_is_not_a_successful_noop() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "hook-pipe-error", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    let (_card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    let db = boot.repo.sqlite_db().unwrap();
    // Make the real SELECT fail; no repository wrapper synthesizes the result.
    db.writer()
        .execute(sqlx::query(
            "ALTER TABLE operations RENAME TO unavailable_operations",
        ))
        .await
        .unwrap();
    let hook = TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone());
    let result = hook
        .on_terminal_exit(&terminal_id, Some(0), false, "", false)
        .await;
    db.writer()
        .execute(sqlx::query(
            "ALTER TABLE unavailable_operations RENAME TO operations",
        ))
        .await
        .unwrap();
    assert!(matches!(result, Err(calm_server::error::CalmError::Db(_))));
    assert_eq!(
        task_row(&boot, "hook-pipe-error").await.status,
        TaskStatus::Running
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
}

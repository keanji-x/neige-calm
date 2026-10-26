//! Exercise both Legacy completion entry points against a durable Pipe preparation shape.
use super::*;

#[tokio::test]
async fn w_terminal_worker_sweep_ignores_pipe_shape() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "pipe-shape", TaskKind::Terminal, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    let (card_id, terminal_id) = seed_terminal_worker(&boot, &task_id).await;
    task.worker_card_id = Some(card_id.to_string());
    seed_task(&boot, task).await;
    seed_worker_op_target(&boot, "terminal-worker", &task_id, card_id.as_str()).await;
    // A stale terminal must not grant authority over the Pipe task, even if it records exit 0.
    let mut output = TxOutput::new("card", Some(card_id.to_string()), json!({"id":card_id}));
    output.data = json!({"terminal_worker_pipe":{"log_path":"/not-a-terminal"}});
    sqlx::query(
        "UPDATE operations SET tx_output_json=?1,phase='succeeded',completed_at_ms=1 \
         WHERE kind='terminal-worker' AND idempotency_key=?2",
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&task_id)
    .execute(boot.repo.sqlite_db().unwrap().writer_pool())
    .await
    .unwrap();
    boot.repo
        .terminal_set_exit_with_output(&terminal_id, Some(0), false, "stale", false)
        .await
        .unwrap();
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "terminal-worker",
            card_id: card_id.to_string(),
        })],
    );
    scheduler.sweep_all().await;
    TerminalTaskHook::new(boot.repo.clone(), boot.events.clone(), boot.write.clone())
        .on_terminal_exit(&terminal_id, Some(0), false, "stale", false)
        .await;
    assert_eq!(
        task_row(&boot, "pipe-shape").await.status,
        TaskStatus::Running
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
}

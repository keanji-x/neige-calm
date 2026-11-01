use super::*;

#[tokio::test]
async fn sweep_boot_resumes_dispatched_without_waiting_for_capacity() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "recovering", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    seed_task(&boot, task).await;
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = capacity.clone().acquire_owned().await.unwrap();
    let (_runtime, scheduler) = build_scheduler_unbooted(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        capacity,
    );
    scheduler.mark_context_sweep_boot_complete();

    tokio::time::timeout(std::time::Duration::from_secs(2), scheduler.sweep_boot())
        .await
        .expect("boot must complete while a recovered task waits for capacity");
    assert_eq!(
        task_row(&boot, "recovering").await.status,
        TaskStatus::Dispatched
    );

    drop(permit);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while task_row(&boot, "recovering").await.status != TaskStatus::Running {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the dispatched task must still resume after capacity becomes available");
}

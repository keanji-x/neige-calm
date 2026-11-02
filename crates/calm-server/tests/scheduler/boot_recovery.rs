use super::*;

#[tokio::test]
async fn sweep_boot_resumes_dispatched_without_waiting_for_capacity() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "recovering", TaskKind::Codex, &[]);
    task.status = TaskStatus::Dispatched;
    let task_id = task.id.clone();
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
    let busy = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "scheduler-resume-capacity-busy",
        &task_id,
        busy.clone(),
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), scheduler.sweep_boot())
        .await
        .expect("boot must complete while a recovered task waits for capacity");
    tokio::time::timeout(std::time::Duration::from_secs(2), busy.entered.notified())
        .await
        .expect("the background recovery encountered the occupied capacity");
    assert_eq!(
        task_row(&boot, "recovering").await.status,
        TaskStatus::Dispatched
    );

    busy.release.notify_one();
    drop(permit);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while task_row(&boot, "recovering").await.status != TaskStatus::Running {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the dispatched task must still resume after capacity becomes available");
}

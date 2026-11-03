//! Recovery must remain observable when worker startup capacity is exhausted.
use super::*;

#[tokio::test]
async fn steady_sweep_does_not_wait_for_exhausted_start_capacity() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "held-start", TaskKind::Codex, &[]);
    task.status = TaskStatus::Pending;
    seed_projected_task(&boot, task).await;
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = capacity.clone().acquire_owned().await.unwrap();
    let (_runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        capacity,
    );

    let swept = tokio::time::timeout(Duration::from_secs(2), scheduler.sweep_all()).await;
    let status_while_held = task_row(&boot, "held-start").await.status;
    // Release before asserting so a red run leaves no blocked background work.
    drop(permit);
    assert!(
        swept.is_ok(),
        "steady reconciliation must not wait for startup capacity"
    );
    assert_eq!(status_while_held, TaskStatus::Pending);

    tokio::time::timeout(Duration::from_secs(5), async {
        while task_row(&boot, "held-start").await.status != TaskStatus::Running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the original attempt still starts after capacity returns");
}

#[tokio::test]
async fn blocked_operation_does_not_hold_admission_or_track_lock() {
    let boot = boot().await;
    let mut first = plan_task(&boot.track_id, "first", TaskKind::Codex, &[]);
    first.access = calm_server::model::TaskAccess::ReadOnly;
    seed_projected_task(&boot, first).await;
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let (runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
        capacity.clone(),
    );
    // A real operation drive fence, not a copy of scheduler behavior.
    let drive = runtime.lock_for_track_delete_for_test().await;
    scheduler.poke(boot.track_id.clone());
    wait_status(&boot, "first", TaskStatus::Dispatched).await;
    let available = tokio::time::timeout(Duration::from_secs(2), capacity.acquire()).await;
    let freed = available.is_ok();
    drop(available);
    let mut second = plan_task(&boot.track_id, "second", TaskKind::Codex, &[]);
    second.access = calm_server::model::TaskAccess::ReadOnly;
    seed_projected_task(&boot, second).await;
    scheduler.poke(boot.track_id.clone());
    let second_claimed = tokio::time::timeout(Duration::from_secs(2), async {
        while task_row(&boot, "second").await.status == TaskStatus::Pending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    drop(drive);
    wait_status(&boot, "first", TaskStatus::Running).await;
    wait_status(&boot, "second", TaskStatus::Running).await;
    assert!(
        freed,
        "operation driving must not consume Planner/admission capacity"
    );
    assert!(
        second_claimed,
        "operation driving must not hold the Track scheduling lock"
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 2);
}

#[tokio::test]
async fn sweep_pokes_coalesce_while_admission_is_exhausted() {
    let boot = boot().await;
    let task = plan_task(&boot.track_id, "coalesced-start", TaskKind::Codex, &[]);
    seed_projected_task(&boot, task).await;
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = capacity.clone().acquire_owned().await.unwrap();
    let (_runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
        capacity,
    );
    let metrics = tokio::runtime::Handle::current().metrics();
    scheduler.sweep_all().await;
    tokio::task::yield_now().await;
    let initial = metrics.num_alive_tasks();
    let mut peak = initial;
    for _ in 0..64 {
        scheduler.sweep_all().await;
        tokio::task::yield_now().await;
        peak = peak.max(metrics.num_alive_tasks());
    }
    // Release before assertions so the red run also drains its waiting work.
    drop(permit);
    wait_status(&boot, "coalesced-start", TaskStatus::Running).await;
    assert!(
        peak <= initial + 1,
        "repeated recovery must keep background work bounded: initial={initial}, peak={peak}"
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 1);
}

#[tokio::test]
async fn poke_after_final_drain_still_starts_its_task() {
    let boot = boot().await;
    let mut first = plan_task(&boot.track_id, "before-drain", TaskKind::Codex, &[]);
    first.access = calm_server::model::TaskAccess::ReadOnly;
    seed_projected_task(&boot, first).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
    );
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "scheduler-admissions-drained",
        boot.track_id.as_str(),
        pause.clone(),
    );
    scheduler.poke(boot.track_id.clone());
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .expect("the first pass drained its triggers");
    wait_status(&boot, "before-drain", TaskStatus::Running).await;
    let mut second = plan_task(&boot.track_id, "after-drain", TaskKind::Codex, &[]);
    second.access = calm_server::model::TaskAccess::ReadOnly;
    seed_projected_task(&boot, second).await;
    scheduler.poke(boot.track_id.clone());
    pause.release.notify_one();
    wait_status(&boot, "after-drain", TaskStatus::Running).await;
    assert_eq!(operation_count(&boot, "codex-worker").await, 2);
}

use super::*;

#[tokio::test]
async fn progress_obligation_reverse_scan_notifies_overdue_child_without_live_event() {
    let boot = boot().await;
    let (task_id, child_id) = seed_child_parent(&boot, "overdue-bootstrap", false, None).await;
    let db = boot.repo.sqlite_db().unwrap();
    db.writer()
        .execute(
            sqlx::query("UPDATE tasks SET status='dispatched',updated_at_ms=?1 WHERE id=?2")
                .bind(now_ms() - 24 * 3600 * 1000)
                .bind(&task_id),
        )
        .await
        .unwrap();
    let (_runtime, scheduler) =
        build_scheduler_unbooted(&boot, vec![], Arc::new(tokio::sync::Semaphore::new(0)));
    scheduler.mark_boot_sweep_complete();
    scheduler.sweep_all().await;
    let wakes = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(
        wakes.len(),
        1,
        "an overdue child handoff must leave a persisted parent notice"
    );
    assert_eq!(wakes[0].1["track_id"], boot.track_id.as_str());
    assert_eq!(wakes[0].1["source"], "progress-obligation");
    assert_eq!(
        wakes[0].1["key"],
        format!("{task_id}:child-track-bootstrap")
    );
    let task = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Dispatched);
    assert_eq!(
        task.status_detail.as_deref(),
        Some("child-track-bootstrap-overdue")
    );
    assert!(
        boot.repo
            .track_get(&child_id)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    let (_runtime2, scheduler2) =
        build_scheduler_unbooted(&boot, vec![], Arc::new(tokio::sync::Semaphore::new(0)));
    scheduler2.mark_boot_sweep_complete();
    scheduler2.sweep_all().await;
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
}

async fn claim_child(boot: &Boot, key: &str, now: i64) -> String {
    let mut task = plan_task(&boot.track_id, key, TaskKind::Codex, &[]);
    task.spawn = calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE.into();
    let id = task.id.clone();
    seed_task(boot, task).await;
    let claimed = id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            assert_eq!(
                calm_server::db::sqlite::task_claim_pending_tx(tx, &claimed, now, &[], false)
                    .await?,
                1
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    id
}

async fn due_ids_at(boot: &Boot, now: i64) -> Vec<String> {
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            Ok(
                calm_server::db::sqlite::progress_obligation::progress_obligations_due_tx(
                    tx,
                    calm_server::db::sqlite::task_progress::CHILD_BOOTSTRAP_PURPOSE,
                    now,
                )
                .await?
                .into_iter()
                .map(|row| row.execution_id)
                .collect(),
            )
        })
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn progress_obligation_claim_and_enrollment_rollback_together() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "atomic-claim", TaskKind::Codex, &[]);
    task.spawn = calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE.into();
    let id = task.id.clone();
    seed_task(&boot, task).await;
    let claimed = id.clone();
    let result: CalmResult<()> =
        calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
            Box::pin(async move {
                assert_eq!(
                    calm_server::db::sqlite::task_claim_pending_tx(tx, &claimed, 1, &[], false)
                        .await?,
                    1
                );
                let due =
                    calm_server::db::sqlite::progress_obligation::progress_obligations_due_tx(
                        tx,
                        calm_server::db::sqlite::task_progress::CHILD_BOOTSTRAP_PURPOSE,
                        i64::MAX,
                    )
                    .await?;
                assert_eq!(due.len(), 1);
                assert_eq!(due[0].execution_id, claimed);
                Err(calm_server::error::CalmError::Conflict(
                    "controlled rollback".into(),
                ))
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(calm_server::error::CalmError::Conflict(_))
    ));
    assert_eq!(
        boot.repo.task_get(&id).await.unwrap().unwrap().status,
        TaskStatus::Pending
    );
    assert!(due_ids_at(&boot, i64::MAX).await.is_empty());
    let worker = plan_task(&boot.track_id, "ordinary-worker", TaskKind::Claude, &[]);
    let worker_id = worker.id.clone();
    seed_task(&boot, worker).await;
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            calm_server::db::sqlite::task_claim_pending_tx(tx, &worker_id, 1, &[], false).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    assert!(due_ids_at(&boot, i64::MAX).await.is_empty());
}

#[tokio::test]
async fn progress_obligation_notification_failure_rolls_back_and_retries() {
    let boot = boot().await;
    let id = claim_child(&boot, "rollback-notice", now_ms() - 24 * 3600 * 1000).await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let db = boot.repo.sqlite_db().unwrap();
    db.writer()
        .execute(sqlx::query(
            "CREATE TRIGGER reject_progress_notice BEFORE INSERT ON events \
         WHEN NEW.kind='track.wake_requested' BEGIN \
         SELECT RAISE(ABORT,'notice unavailable'); END",
        ))
        .await
        .unwrap();
    assert!(scheduler.sweep_progress_obligations().await.is_err());
    assert_eq!(due_ids_at(&boot, now_ms()).await, vec![id.clone()]);
    assert!(
        boot.repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status_detail
            .is_none()
    );
    db.writer()
        .execute(sqlx::query("DROP TRIGGER reject_progress_notice"))
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        scheduler.sweep_progress_obligations(),
        scheduler.sweep_progress_obligations()
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
}

#[tokio::test]
async fn progress_obligation_late_success_releases_without_killing_child() {
    let boot = boot().await;
    let id = claim_child(&boot, "late-success", now_ms() - 24 * 3600 * 1000).await;
    let db = boot.repo.sqlite_db().unwrap();
    db.writer()
        .execute(
            sqlx::query(
                "UPDATE tasks SET updated_at_ms=?1,child_track_id='late-child' WHERE id=?2",
            )
            .bind(now_ms())
            .bind(&id),
        )
        .await
        .unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_progress_obligations().await.unwrap();
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
    let stamp_id = id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            assert_eq!(
                calm_server::db::sqlite::task_mark_sub_track_running_tx(tx, &stamp_id, now_ms())
                    .await?,
                1
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    let task = boot.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Running);
    assert_eq!(task.status_detail, None);
    scheduler.sweep_progress_obligations().await.unwrap();
    assert!(due_ids_at(&boot, i64::MAX).await.is_empty());
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
}

#[tokio::test]
async fn progress_obligation_obsolete_attempts_do_not_wake_parent() {
    let boot = boot().await;
    let db = boot.repo.sqlite_db().unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    for status in ["running", "failed", "canceled", "done"] {
        let id = claim_child(&boot, &format!("obsolete-{status}"), 1).await;
        db.writer()
            .execute(
                sqlx::query("UPDATE tasks SET status=?1 WHERE id=?2")
                    .bind(status)
                    .bind(&id),
            )
            .await
            .unwrap();
    }
    let deleted = claim_child(&boot, "deleted", 1).await;
    db.writer()
        .execute(sqlx::query("DELETE FROM tasks WHERE id=?1").bind(&deleted))
        .await
        .unwrap();
    let superseded = claim_child(&boot, "superseded", 1).await;
    db.writer()
        .execute(sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1").bind(&superseded))
        .await
        .unwrap();
    let origin = calm_types::task_recovery::TaskAttemptOrigin::Recovery {
        previous_attempt_id: superseded.clone(),
        idempotency_key: "historical-recovery".into(),
        request_fingerprint: "fingerprint".into(),
        reason: "historical allocation fence".into(),
        actor: ActorId::Kernel,
        constraint: calm_types::task_recovery::TaskRecoveryConstraint::V1 {
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
            refs: vec![TaskContextRef {
                track_id: boot.track_id.clone(),
                block_id: "root".into(),
                rev: 1,
                hash: "0".repeat(64),
                is_root: true,
            }],
        },
    };
    db.writer()
        .execute(
            sqlx::query(
                "INSERT INTO task_attempt_allocations \
         (attempt_id,track_id,key,generation,origin_json,created_at_ms) \
         VALUES('successor',?1,'superseded',2,?2,2)",
            )
            .bind(boot.track_id.as_str())
            .bind(serde_json::to_string(&origin).unwrap()),
        )
        .await
        .unwrap();
    db.writer()
        .execute(sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1").bind(&superseded))
        .await
        .unwrap();
    scheduler.sweep_progress_obligations().await.unwrap();
    assert!(event_rows(&boot, "track.wake_requested").await.is_empty());
    assert!(due_ids_at(&boot, i64::MAX).await.is_empty());
}

#[tokio::test]
async fn progress_obligation_arms_missing_without_notification_or_renewal() {
    let boot = boot().await;
    let (id, _) = seed_child_parent(&boot, "young-bootstrap", false, None).await;
    let db = boot.repo.sqlite_db().unwrap();
    let anchor = now_ms();
    db.writer()
        .execute(
            sqlx::query("UPDATE tasks SET status='dispatched',updated_at_ms=?1 WHERE id=?2")
                .bind(anchor)
                .bind(&id),
        )
        .await
        .unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_progress_obligations().await.unwrap();
    let deadline = anchor + calm_server::db::sqlite::task_progress::CHILD_BOOTSTRAP_WINDOW_MS;
    assert!(due_ids_at(&boot, deadline - 1).await.is_empty());
    assert_eq!(due_ids_at(&boot, deadline).await, vec![id.clone()]);
    db.writer()
        .execute(
            sqlx::query("UPDATE tasks SET updated_at_ms=?1 WHERE id=?2")
                .bind(anchor + 3600 * 1000)
                .bind(&id),
        )
        .await
        .unwrap();
    scheduler.sweep_progress_obligations().await.unwrap();
    assert_eq!(due_ids_at(&boot, deadline).await, vec![id]);
    assert!(event_rows(&boot, "track.wake_requested").await.is_empty());
}

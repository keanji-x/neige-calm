use super::*;

#[tokio::test]
async fn reclaim_recovery_batch_of_blocked_writers_admits_one_execution_at_a_time() {
    use crate::track_report::{ReportDocOp, ReportEditTarget};
    let h = Harness::new(&format!("printf A >> ran; {WAIT_FOR_GO}")).await;
    let runtime = Arc::new(h.runtime.clone());
    let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
    let scheduler = crate::scheduler::Scheduler::new(
        h.db.clone(),
        h.ctx.events.clone(),
        write.clone(),
        Arc::downgrade(&runtime),
        crate::per_card_lock::new_per_card_locks(),
        Arc::new(tokio::sync::Semaphore::new(1)),
        h.root.path().join("gates"),
        crate::scheduler::WorkerLiveness::DEFAULT,
        crate::scheduler::WorkerIdleWake::new(
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(h.db.clone()),
            crate::scheduler::WORKER_IDLE_TURN_GRACE,
            crate::scheduler::WORKER_IDLE_PROBE_TIMEOUT,
        ),
    );
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    let fence =
        crate::per_card_lock::lock_key(runtime.track_locks_for_test(), &h.task.track_id).await;
    runtime.drive().await.unwrap();
    let rev: i64 = sqlx::query_scalar(
        "SELECT json_extract(payload,'$.docRev') FROM cards WHERE track_id=?1 AND kind='track-report'",
    ).bind(&h.task.track_id).fetch_one(h.db.pool()).await.unwrap();
    let declaration = json!({"key":"second","kind":"terminal", "ready":true,
        "declared_by":"user", "command":format!("printf B >> ran; {WAIT_FOR_GO}")});
    crate::track_report::write::rest_user_block_op(
        h.db.as_ref(),
        &h.ctx.events,
        &write,
        ReportEditTarget::resolve(h.db.as_ref(), &h.task.track_id)
            .await
            .unwrap(),
        ReportDocOp::UpsertBlock {
            id: None,
            kind: "task".into(),
            content: calm_types::report_blocks::render_fence("task", &declaration),
            if_rev: None,
            if_doc_rev: Some(rev.try_into().unwrap()),
            position: None,
        },
    )
    .await
    .unwrap();
    scheduler
        .schedule_track(h.task.track_id.clone().into())
        .await;
    let second =
        h.db.task_current_get(&h.task.track_id, "second")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);
    assert_eq!(second.status, crate::model::TaskStatus::Dispatched);
    let second_op = runtime
        .find_by_kind_and_idempotency("terminal-worker", &second.id)
        .await
        .unwrap()
        .unwrap();
    for op in [h.op().await, second_op.clone()] {
        assert!(
            crate::operation::pending_blocker(&op).unwrap().is_some(),
            "phase={:?} detail={:?} error={:?}",
            op.phase,
            op.phase_detail,
            op.last_error
        );
        assert!(op.tx_output.is_none());
    }
    drop(fence);
    // The ordinary driver can resume its writer after expiry. The scheduler
    // retains ownership of the other writer even with an expired deadline.
    h.db.db().writer().execute(sqlx::query(
        "UPDATE operations SET lease_until_ms=0 WHERE id IN (?1,?2) AND phase='pending' AND lease_owner IS NULL",
    ).bind(&h.id).bind(&second_op.id)).await.unwrap();
    runtime.drive().await.unwrap();
    let deferred = runtime
        .find_by_kind_and_idempotency("terminal-worker", &second.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deferred.phase, Phase::Pending);
    assert!(deferred.tx_output.is_none());
    tokio::time::timeout(Duration::from_secs(8), async {
        while !h.root.path().join("ran").exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("ran"))
            .unwrap()
            .len(),
        1,
        "one writer must remain blocked while the other's command is live"
    );
    let admissions: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(admissions, 1);
    std::fs::write(h.root.path().join("go"), b"").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            scheduler.sweep_all().await;
            if h.status().await == crate::model::TaskStatus::Done
                && h.db.task_get(&second.id).await.unwrap().unwrap().status
                    == crate::model::TaskStatus::Done
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let output = std::fs::read_to_string(h.root.path().join("ran")).unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(output.matches('A').count(), 1);
    assert_eq!(output.matches('B').count(), 1);
    assert_eq!(h.op().await.id, h.id);
    assert_eq!(
        runtime
            .find_by_kind_and_idempotency("terminal-worker", &second.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        second_op.id
    );
}

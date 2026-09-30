//! Task declaration and execution helpers shared by the task-history read tests.

use serde_json::json;

use crate::mcp_track_report::{Boot, planner_identity, upsert_block};
use calm_server::db::sqlite::{
    TaskReporter, begin_immediate_tx, task_claim_pending_tx, task_fail_from_worker_tx,
    task_report_success_from_worker_tx,
};
use calm_server::model::Task;
use calm_server::task_context::TaskContextMonitor;
use serde_json::Value;

pub(super) fn declaration(key: &str, dependencies: &[&str]) -> Value {
    json!({"key": key, "kind": "terminal", "command": "true", "depends_on": dependencies,
        "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "ready": true})
}

pub(super) async fn declare(boot: &Boot, payload: Value) -> (String, u64) {
    let out = upsert_block(
        boot,
        planner_identity(boot),
        json!({"kind": "task", "payload": payload}),
    )
    .await
    .unwrap();
    (
        out["id"].as_str().unwrap().into(),
        out["rev"].as_u64().unwrap(),
    )
}

pub(super) async fn finish(boot: &Boot, task: &Task, success: bool) {
    let monitor = TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    );
    let closure = monitor
        .resolve_task_closure(task.track_id.as_str(), &task.key)
        .await
        .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert_eq!(
        task_claim_pending_tx(
            &mut tx,
            &task.id,
            10,
            &closure.refs,
            closure.closure_truncated
        )
        .await
        .unwrap(),
        1
    );
    if success {
        assert!(matches!(
            task_report_success_from_worker_tx(
                &mut tx,
                &task.id,
                &task.track_id,
                TaskReporter::Kernel,
                11
            )
            .await
            .unwrap(),
            calm_server::db::sqlite::SuccessReportFlip::Done
        ));
    } else {
        assert_eq!(
            task_fail_from_worker_tx(
                &mut tx,
                &task.id,
                &task.track_id,
                TaskReporter::Kernel,
                "spawn-failed: controlled preparation failure",
                11
            )
            .await
            .unwrap(),
            1
        );
    }
    tx.commit().await.unwrap();
}

pub(super) async fn current(boot: &Boot, key: &str) -> Task {
    boot.repo
        .task_current_get(boot.track_id.as_str(), key)
        .await
        .unwrap()
        .unwrap()
}

pub(super) fn ordinary_codex_declaration(key: &str) -> Value {
    json!({"key": key, "kind": "codex", "goal": format!("do {key}"), "depends_on": [],
        "no_gate_reason": "ordinary worker timeout fixture",
        "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "ready": true})
}

/// For a lease row the caller already wrote (a base-recording one through
/// `test_seams::acquire_based_workspace_lease_for_test`): claim the current
/// attempt of `key` onto `boot.worker_card_id`, mark it running, hit it with
/// the scheduler's liveness timeout and release the lease row. Returns the
/// failed row.
pub(super) async fn time_out_claimed_worker_holding_lease(
    boot: &Boot,
    key: &str,
    lease_id: &str,
) -> Task {
    let task = current(boot, key).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let card_id = boot.worker_card_id.as_str().to_string();
    let track_id = boot.track_id.as_str().to_string();
    let now = calm_server::model::now_ms();
    let monitor = TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    );
    let closure = monitor.resolve_task_closure(&track_id, key).await.unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert_eq!(
        task_claim_pending_tx(
            &mut tx,
            &task.id,
            10,
            &closure.refs,
            closure.closure_truncated
        )
        .await
        .unwrap(),
        1
    );
    sqlx::query("UPDATE tasks SET status='running', worker_card_id=?1 WHERE id=?2")
        .bind(&card_id)
        .bind(&task.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        task_fail_from_worker_tx(
            &mut tx,
            &task.id,
            &track_id,
            TaskReporter::Kernel,
            "worker-timeout",
            now + 1,
        )
        .await
        .unwrap(),
        1
    );
    sqlx::query(
        "UPDATE workspace_leases SET state='released', released_at_ms=?1 WHERE lease_id=?2",
    )
    .bind(now + 1)
    .bind(lease_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let failed = current(boot, key).await;
    assert_eq!(failed.id, task.id);
    assert_eq!(
        failed
            .status_detail
            .as_deref()
            .map(calm_server::db::sqlite::status_detail_class),
        Some("worker-timeout")
    );
    failed
}

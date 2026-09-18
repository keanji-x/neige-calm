//! Task continuity through the public Planner entry points.

use crate::mcp_track_report::{boot, call_tool, planner_identity, worker_identity};
use serde_json::json;

#[tokio::test]
async fn task_recovery_planner_surface_is_available_and_worker_is_forbidden() {
    let boot = boot().await;
    let args = json!({"key": "b", "expected_attempt_id": "unknown",
        "idempotency_key": "recover-b", "reason": "retry failed work"});
    let planner = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        args.clone(),
    )
    .await
    .expect_err("unknown attempt must be rejected");
    assert_ne!(planner.code, -32601, "Planner recovery tool must exist");
    let worker = call_tool(&boot, "calm.plan.recover", worker_identity(&boot), args)
        .await
        .expect_err("worker cannot request recovery");
    assert!(
        worker.message.contains("role") || worker.code == -32403,
        "{worker:?}"
    );
}

use crate::mcp_track_report::Boot;
use calm_server::db::sqlite::{
    TaskReporter, begin_immediate_tx, task_claim_pending_tx, task_fail_from_worker_tx,
    task_report_success_from_worker_tx,
};
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::model::{Task, TaskStatus};
use calm_server::task_context::TaskContextMonitor;
use serde_json::Value;

pub(super) fn declaration(key: &str, dependencies: &[&str]) -> Value {
    json!({"key": key, "kind": "terminal", "command": "true", "depends_on": dependencies,
        "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "ready": true})
}

pub(super) async fn declare(boot: &Boot, payload: Value) -> (String, u64) {
    let report = call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), json!({}))
        .await
        .unwrap();
    let out = call_tool(
        boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(boot),
        json!({"kind": "task", "payload": payload, "if_doc_rev": report["docRev"]}),
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

pub(super) fn recovery_args(task: &Task, request_key: &str) -> Value {
    json!({"key": task.key, "expected_attempt_id": task.id, "idempotency_key": request_key, "reason": "Recover the failed task"})
}

async fn recover(boot: &Boot, task: &Task, request_key: &str) -> Value {
    call_tool(
        boot,
        "calm.plan.recover",
        planner_identity(boot),
        recovery_args(task, request_key),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn task_recovery_preserves_sibling_history_and_dependency_keys() {
    let boot = boot().await;
    declare(&boot, declaration("a", &[])).await;
    declare(&boot, declaration("b", &[])).await;
    declare(&boot, declaration("c", &["a", "b"])).await;
    let a = current(&boot, "a").await;
    let b = current(&boot, "b").await;
    let c = current(&boot, "c").await;
    finish(&boot, &a, true).await;
    finish(&boot, &b, false).await;
    let a_before = boot.repo.task_get(&a.id).await.unwrap().unwrap();
    let b_before = boot.repo.task_get(&b.id).await.unwrap().unwrap();
    let receipt = recover(&boot, &b, "request-b").await;
    assert_eq!(receipt["key"], "b");
    assert_eq!(receipt["generation"], 2);
    let replacement = current(&boot, "b").await;
    assert_ne!(replacement.id, b.id);
    assert_eq!(receipt["attempt_id"], replacement.id);
    assert_eq!(replacement.status, TaskStatus::Pending);
    assert_eq!(
        serde_json::to_value(boot.repo.task_get(&a.id).await.unwrap()).unwrap(),
        serde_json::to_value(Some(a_before)).unwrap()
    );
    assert_eq!(
        serde_json::to_value(boot.repo.task_get(&b.id).await.unwrap()).unwrap(),
        serde_json::to_value(Some(b_before)).unwrap()
    );
    assert_eq!(current(&boot, "c").await.depends_on_json, c.depends_on_json);
    finish(&boot, &replacement, true).await;
    let plan = boot
        .repo
        .tasks_by_track(boot.track_id.as_str())
        .await
        .unwrap();
    let ready = calm_server::scheduler::compute_ready(&plan, 1);
    assert_eq!(
        ready
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec![c.id.as_str()]
    );
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let b = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "b")
        .unwrap();
    assert_eq!(b["attempt_id"], replacement.id);
    assert_eq!(b["generation"], 2);
    assert_eq!(
        boot.repo
            .task_history_by_key(boot.track_id.as_str(), "b")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn task_recovery_concurrent_replay_returns_original_receipt_after_completion() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let (left, right) = tokio::join!(
        recover(&boot, &b, "request-b"),
        recover(&boot, &b, "request-b")
    );
    assert_eq!(left, right);
    let replacement = current(&boot, "b").await;
    finish(&boot, &replacement, true).await;
    assert_eq!(recover(&boot, &b, "request-b").await, left);
    let mut conflict = recovery_args(&b, "request-b");
    conflict["reason"] = json!("different request");
    let error = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        conflict,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32409);
    assert!(error.message.contains("different request"));
    assert_eq!(
        boot.repo
            .task_history_by_key(boot.track_id.as_str(), "b")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn task_recovery_planner_limit_and_late_worker_result_are_fenced() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    recover(&boot, &b, "first").await;
    let second = current(&boot, "b").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert!(matches!(
        task_report_success_from_worker_tx(&mut tx, &b.id, &b.track_id, TaskReporter::Kernel, 20)
            .await
            .unwrap(),
        calm_server::db::sqlite::SuccessReportFlip::None
    ));
    assert!(
        calm_server::operation::refuse_if_context_stale(&mut tx, Some(&b.id))
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    assert_eq!(current(&boot, "b").await.status, TaskStatus::Pending);
    finish(&boot, &second, false).await;
    let error = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&second, "second"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32403);
    assert!(error.message.contains("limit reached"));
}

#[tokio::test]
async fn task_recovery_contract_change_is_rejected_before_allocation() {
    let boot = boot().await;
    let (id, rev) = declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let mut changed = declaration("b", &[]);
    changed["command"] = json!("false");
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"id": id, "kind": "task", "payload": changed, "if_rev": rev}),
    )
    .await
    .unwrap();
    let error = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&b, "changed"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32409);
    assert!(error.message.contains("contract changed"));
    assert_eq!(current(&boot, "b").await.id, b.id);
}

#[tokio::test]
async fn task_recovery_rest_user_can_authorize_another_attempt_and_history_is_gate_free() {
    use axum::{
        Extension,
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    recover(&boot, &b, "planner-first").await;
    let second = current(&boot, "b").await;
    finish(&boot, &second, false).await;
    let app = calm_server::routes::task_recovery::router()
        .with_state(crate::task_projection_acceptance::route_state(&boot).await)
        .layer(Extension(crate::task_projection_acceptance::principal()))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/tracks/{}/tasks/b/attempts", boot.track_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let view: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(view["current"]["attempt_id"], second.id);
    assert_eq!(view["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(view["recovery"]["allowed"], true);
    let request = json!({"expected_attempt_id": second.id, "idempotency_key": "user-second", "reason": "Recover task"});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/tasks/b/recover", boot.track_id))
                .header("content-type", "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let receipt: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(receipt["generation"], 3);
    for attempt in view["attempts"].as_array().unwrap() {
        assert!(attempt.get("gate").is_none());
        assert!(attempt.get("gate_json").is_none());
        assert!(attempt.get("gate_result").is_none());
    }
}

#[tokio::test]
async fn task_recovery_unknown_freeze_and_unsettled_operation_fail_closed() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) VALUES('uncertain','uncertain','terminal-worker',?1,'h','card','{}','{}','spawn_started',1,1)")
        .bind(&b.id).execute(&pool).await.unwrap();
    let error = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&b, "request"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32409);
    assert!(error.message.contains("uncertain external effects"));
    sqlx::query("UPDATE operations SET phase='failed' WHERE id='uncertain'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET claim_context_json=NULL WHERE id=?1")
        .bind(&b.id)
        .execute(&pool)
        .await
        .unwrap();
    let error = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&b, "request"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32409);
    assert!(error.message.contains("no frozen contract"));
    assert_eq!(current(&boot, "b").await.id, b.id);
}

#[tokio::test]
async fn task_recovery_terminal_track_and_declared_wait_never_grant_planner_retry() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    for lifecycle in ["done", "canceled", "failed"] {
        sqlx::query("UPDATE tracks SET lifecycle=?1 WHERE id=?2")
            .bind(lifecycle)
            .bind(boot.track_id.as_str())
            .execute(&pool)
            .await
            .unwrap();
        let error = call_tool(
            &boot,
            "calm.plan.recover",
            planner_identity(&boot),
            recovery_args(&b, lifecycle),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32409, "{lifecycle}: {error:?}");
        assert!(error.message.contains("track is blocked or terminal"));
    }
    sqlx::query(
        "UPDATE tracks SET lifecycle='reviewing',automation_policy='declare-and-wait' WHERE id=?1",
    )
    .bind(boot.track_id.as_str())
    .execute(&pool)
    .await
    .unwrap();
    let error = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&b, "wait"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32403);
    assert!(error.message.contains("explicit User recovery"));
}

#[tokio::test]
async fn task_recovery_pending_rebuild_keeps_identity_and_changed_contract_cannot_spawn() {
    let boot = boot().await;
    let (id, rev) = declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    recover(&boot, &b, "request").await;
    let recovered = current(&boot, "b").await;
    let mut payload = declaration("b", &[]);
    payload["ready"] = json!(false);
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"id": id, "kind": "task", "payload": payload, "if_rev": rev}),
    )
    .await
    .unwrap();
    assert!(
        boot.repo
            .task_current_get(boot.track_id.as_str(), "b")
            .await
            .unwrap()
            .is_none()
    );
    let view = calm_server::task_recovery::task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "b",
        calm_server::ids::ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap();
    assert_eq!(view.current.as_ref().unwrap().attempt_id, recovered.id);
    assert_eq!(view.current.as_ref().unwrap().status, "awaiting_projection");
    payload["ready"] = json!(true);
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"id": id, "kind": "task", "payload": payload, "if_rev": out["rev"]}),
    )
    .await
    .unwrap();
    assert_eq!(current(&boot, "b").await.id, recovered.id);
    payload["command"] = json!("false");
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"id": id, "kind": "task", "payload": payload, "if_rev": out["rev"]}),
    )
    .await
    .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    // Emulate a boot-resumed prepared operation: admission must recheck the
    // original recovery contract, even if a mutable pending projection drifted.
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
        .bind(&recovered.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        calm_server::operation::refuse_if_context_stale(&mut tx, Some(&recovered.id))
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn task_recovery_spawn_rechecks_current_declaration_permission_after_claim() {
    let boot = boot().await;
    let (block_id, revision) = declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    recover(&boot, &b, "request").await;
    let recovered = current(&boot, "b").await;
    let closure = TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
    .resolve_task_closure(&recovered.track_id, &recovered.key)
    .await
    .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    task_claim_pending_tx(&mut tx, &recovered.id, 20, &closure.refs, false)
        .await
        .unwrap();
    assert!(
        calm_server::operation::refuse_if_context_stale(&mut tx, Some(&recovered.id))
            .await
            .is_ok()
    );
    tx.commit().await.unwrap();
    let mut withdrawn = declaration("b", &[]);
    withdrawn["ready"] = json!(false);
    call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({"id":block_id,"kind":"task","payload":withdrawn,"if_rev":revision}),
    )
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert!(
        calm_server::operation::refuse_if_context_stale(&mut tx, Some(&recovered.id))
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn task_recovery_cancel_targets_current_attempt_and_old_receipt_replay_is_authenticated() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let receipt = recover(&boot, &b, "request").await;
    call_tool(
        &boot,
        "calm.plan.cancel",
        planner_identity(&boot),
        json!({"key": "b", "message": "withdraw pending recovery"}),
    )
    .await
    .unwrap();
    assert_eq!(current(&boot, "b").await.status, TaskStatus::Canceled);
    assert_eq!(
        boot.repo.task_get(&b.id).await.unwrap().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(recover(&boot, &b, "request").await, receipt);
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?1")
        .bind(planner_identity(&boot).session_id)
        .execute(&pool)
        .await
        .unwrap();
    let denied = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&b, "request"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        denied.code, -32403,
        "an old receipt never bypasses current authentication"
    );
}

#[tokio::test]
async fn task_recovery_blocked_track_resumes_in_same_transaction() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tracks SET lifecycle='blocked' WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let view = calm_server::task_recovery::task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "b",
        planner_identity(&boot).to_actor_id(),
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap();
    assert!(view.recovery.allowed, "{}", view.recovery.reason);
    recover(&boot, &b, "resume-blocked").await;
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        calm_server::model::TrackLifecycle::Working
    );
    assert_eq!(current(&boot, "b").await.status, TaskStatus::Pending);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_attempt_allocations WHERE track_id=?1 AND key='b'",
    )
    .bind(boot.track_id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn task_recovery_rebuild_uses_authoritative_crdt_when_payload_cache_diverges() {
    use calm_server::track_report_doc::ReportDoc;
    use calm_types::report_blocks::render_fence;
    for replace_root in [false, true] {
        let boot = boot().await;
        let (block_id, _) = declare(&boot, declaration("b", &[])).await;
        let b = current(&boot, "b").await;
        finish(&boot, &b, false).await;
        recover(&boot, &b, "request").await;
        let recovered = current(&boot, "b").await;
        let pool = boot.repo.sqlite_pool().unwrap();
        let bytes: Vec<u8> = sqlx::query_scalar("SELECT body_crdt FROM cards WHERE id=?1")
            .bind(boot.report_card_id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        let mut changed = declaration("b", &[]);
        if replace_root {
            doc.delete_block(&block_id).unwrap();
            // IDs are minted from content and position. Move the recreated
            // declaration to a distinct position to exercise a truly new ID.
            doc.upsert_block(None, "prose", "replacement position")
                .unwrap();
            let (replacement_id, _) = doc
                .upsert_block(None, "task", &render_fence("task", &changed))
                .unwrap();
            assert_ne!(
                replacement_id, block_id,
                "fixture must replace root identity"
            );
        } else {
            changed["command"] = json!("false");
            doc.upsert_block(Some(&block_id), "task", &render_fence("task", &changed))
                .unwrap();
        }
        // Deliberately preserve the old derived JSON cache. Projection's declared
        // command and its recovery comparison must both use the CRDT authority.
        sqlx::query("UPDATE cards SET body_crdt=?1 WHERE id=?2")
            .bind(doc.to_bytes())
            .bind(boot.report_card_id.as_str())
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        calm_server::track_report::tasks_rebuild_tx(&mut tx, boot.track_id.as_str())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(
            boot.repo
                .task_current_get(boot.track_id.as_str(), "b")
                .await
                .unwrap()
                .is_none(),
            "a changed authoritative command or root identity must not retain an executable recovery projection"
        );
        let allocation = calm_server::db::write_in_tx_typed(boot.repo.as_ref(), {
            let id = recovered.id.clone();
            move |tx| {
                Box::pin(
                    async move { Ok(calm_server::db::sqlite::task_attempt_get_tx(tx, &id).await?) },
                )
            }
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            allocation.attempt_id, recovered.id,
            "the admitted generation remains recorded"
        );
        assert_eq!(
            boot.repo.task_get(&b.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
    }
}

#[tokio::test]
async fn task_recovery_terminal_leader_exit_never_proves_descendant_write_stop() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("INSERT INTO terminals(id,card_id,program,cwd,env,theme_fg,theme_bg,created_at) VALUES('recovery-terminal',?1,'/bin/sh','/tmp','{}','#ffffff','#000000',1)")
        .bind(boot.worker_card_id.as_str()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE id=?2")
        .bind(boot.worker_card_id.as_str())
        .bind(&b.id)
        .execute(&pool)
        .await
        .unwrap();
    finish(&boot, &b, false).await;
    for (exit_code, signalled) in [(Some(-1), false), (Some(1), false), (None, true)] {
        boot.repo
            .terminal_set_exit("recovery-terminal", exit_code, signalled)
            .await
            .unwrap();
        let denied = call_tool(
            &boot,
            "calm.plan.recover",
            planner_identity(&boot),
            recovery_args(&b, "known-exit"),
        )
        .await
        .unwrap_err();
        assert_eq!(denied.code, -32409);
        assert!(denied.message.contains("no stop proof"));
        let view = calm_server::task_recovery::task_recovery_view(
            boot.repo.as_ref(),
            boot.track_id.as_str(),
            &b.key,
            calm_server::ids::ActorId::User,
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .unwrap();
        assert!(!view.recovery.allowed);
        assert_eq!(view.recovery.code, "predecessor_not_quiescent");
        assert!(view.recovery.reason.contains("new task"));
    }
}

/// A legacy (non-isolated) attempt is recovered onto the same legacy adapter
/// the scheduler routes it through; the response must state that route, never
/// the isolated Codex envelope.
#[tokio::test]
async fn task_recovery_of_legacy_attempt_states_its_actual_executor() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let receipt = recover(&boot, &b, "request-b").await;
    let replacement = current(&boot, "b").await;
    assert_eq!(receipt["attempt_id"], replacement.id);
    let (operation_kind, _) = calm_server::scheduler::build_worker_payload(&replacement).unwrap();
    assert_eq!(operation_kind, "terminal-worker");
    assert_eq!(
        receipt["executor_environment"],
        json!({
            "executor": "terminal",
            "note": "recovery re-runs on the same executor as the failed attempt; its environment is unchanged",
        })
    );
    let changes = receipt["recover_changes"].as_str().unwrap();
    assert!(changes.contains("same executor as the failed attempt"));
    assert!(changes.contains("missing capability"));
    assert!(!changes.contains("only the workspace is new"));
    // Replays carry the same statement.
    let replay = recover(&boot, &b, "request-b").await;
    assert_eq!(replay, receipt);
}

fn ordinary_codex_declaration(key: &str) -> Value {
    json!({"key": key, "kind": "codex", "goal": format!("do {key}"), "depends_on": [],
        "no_gate_reason": "ordinary worker timeout fixture",
        "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "ready": true})
}

/// The current attempt of `key` is claimed, prepared on `boot.worker_card_id`
/// with a held workspace lease, then hit by the scheduler's liveness timeout
/// (the same `task_fail_from_worker_tx(.., Kernel, "worker-timeout", ..)`
/// `fail_task_liveness_timeout` issues), which releases the lease but keeps
/// the directory. Returns the failed row and the lease path; the returned
/// directory keeps the path alive.
async fn time_out_prepared_ordinary_worker(
    boot: &Boot,
    key: &str,
) -> (Task, String, tempfile::TempDir) {
    let task = current(boot, key).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let card_id = boot.worker_card_id.as_str().to_string();
    let track_id = boot.track_id.as_str().to_string();
    let lease_dir = tempfile::Builder::new()
        .prefix("neige-1727-lease-")
        .tempdir()
        .unwrap();
    let lease_path = lease_dir.path().display().to_string();
    let now = calm_server::model::now_ms();
    let lease_id = format!("lease-1727-{}", task.id);
    sqlx::query(
        "INSERT INTO workspace_leases (lease_id, card_id, track_id, path, state, lease_owner, \
         lease_until_ms, boot_id, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, 'held', 'test-owner', ?5, NULL, ?6, ?6)",
    )
    .bind(&lease_id)
    .bind(&card_id)
    .bind(&track_id)
    .bind(&lease_path)
    .bind(now + 60_000)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
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
    .bind(&lease_id)
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
    (failed, lease_path, lease_dir)
}

fn listed_entry(list: &Value, key: &str) -> Value {
    list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["key"] == key)
        .unwrap()
        .clone()
}

async fn append_worktree_event(boot: &Boot, event: calm_server::event::Event) {
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = pool.begin().await.unwrap();
    calm_server::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &calm_server::ids::ActorId::KernelDispatcher,
        &calm_server::event::EventScope::Card {
            card: boot.worker_card_id.clone(),
            track: boot.track_id.clone(),
            area: boot.area_id.clone(),
        },
        None,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// #1727 S3 — an ordinary (shared) codex worker that timed out after it was
/// prepared: the view names the way out, and `calm.plan.list` (MCP only)
/// carries `recovery.guidance` with the retained worktree. The REST
/// `TaskRecoveryView` stays `{allowed, code, reason}`.
#[tokio::test]
async fn task_recovery_timed_out_ordinary_codex_worker_is_guided_to_a_new_task() {
    let boot = boot().await;
    declare(&boot, ordinary_codex_declaration("b")).await;
    let card_id = boot.worker_card_id.as_str().to_string();
    let track_id = boot.track_id.as_str().to_string();
    let (_failed, lease_path, _lease_dir) = time_out_prepared_ordinary_worker(&boot, "b").await;

    let view = calm_server::task_recovery::task_recovery_view(
        boot.repo.as_ref(),
        &track_id,
        "b",
        calm_server::ids::ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap();
    assert!(!view.recovery.allowed);
    assert_eq!(view.recovery.code, "predecessor_not_quiescent");
    assert!(
        view.recovery.reason.contains("new task"),
        "{}",
        view.recovery.reason
    );

    let entry = |list: &Value| listed_entry(list, "b");
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let recovery = &entry(&list)["recovery"];
    assert_eq!(recovery["allowed"], false);
    assert_eq!(recovery["code"], "predecessor_not_quiescent");
    let guidance = &recovery["guidance"];
    assert_eq!(guidance["supported_continuation"], "new_task");
    assert!(
        guidance["blocking_condition"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{guidance}"
    );
    assert_eq!(guidance["retained"]["workspace_path"], lease_path);
    assert_eq!(
        guidance["retained"]["branch"],
        format!("neige/{track_id}/{card_id}"),
        "no commit recorded: the branch is the lease's slice branch name"
    );
    assert!(
        guidance["retained"].get("last_commit").is_none(),
        "{guidance}"
    );

    // Summary mode carries the same guidance paths.
    let summary = call_tool(
        &boot,
        "calm.plan.list",
        planner_identity(&boot),
        json!({"detail": "summary", "key": "b"}),
    )
    .await
    .unwrap();
    let summary_guidance = &entry(&summary)["recovery"]["guidance"];
    assert_eq!(summary_guidance["supported_continuation"], "new_task");
    assert_eq!(summary_guidance["retained"]["workspace_path"], lease_path);

    // A kernel-recorded commit for that card names the retained commit + branch.
    append_worktree_event(
        &boot,
        calm_server::event::Event::WorktreeCommitted {
            track_id: boot.track_id.clone(),
            card_id: boot.worker_card_id.clone(),
            commit_sha: "abc123def".into(),
            branch: "neige/recorded-branch".into(),
        },
    )
    .await;
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let retained = &entry(&list)["recovery"]["guidance"]["retained"];
    assert_eq!(retained["workspace_path"], lease_path);
    assert_eq!(retained["last_commit"], "abc123def");
    assert_eq!(retained["branch"], "neige/recorded-branch");

    // REST serialisation of the wire type is unchanged: no `guidance` key.
    let rest =
        crate::task_recovery_reads::rest_attempts(&boot, "b", axum::http::StatusCode::OK).await;
    assert_eq!(rest["recovery"]["code"], "predecessor_not_quiescent");
    assert_eq!(
        rest["recovery"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["allowed", "code", "reason"]
    );
}

/// #1727 S3 fix G1 — admission checks the actor-dependent policy before the
/// actor-independent predecessor fence. A generation-2 auto-declare Planner
/// task whose recovered ordinary worker timed out is refused to the Planner
/// with `recovery_limit_reached`, but a User recovery would hit the permanent
/// fence next: guidance must advertise `new_task`, naming both conditions,
/// never a `user_recovery` the kernel refuses.
#[tokio::test]
async fn task_recovery_guidance_does_not_advertise_a_user_recovery_the_predecessor_fence_refuses() {
    let boot = boot().await;
    declare(&boot, ordinary_codex_declaration("b")).await;
    let first = current(&boot, "b").await;
    finish(&boot, &first, false).await;
    recover(&boot, &first, "planner-first").await;
    let (second, lease_path, _lease_dir) = time_out_prepared_ordinary_worker(&boot, "b").await;
    assert_ne!(second.id, first.id);

    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let entry = listed_entry(&list, "b");
    assert_eq!(entry["generation"], 2);
    let recovery = &entry["recovery"];
    assert_eq!(recovery["allowed"], false);
    assert_eq!(recovery["code"], "recovery_limit_reached", "{recovery}");
    let guidance = &recovery["guidance"];
    assert_eq!(guidance["supported_continuation"], "new_task", "{guidance}");
    let condition = guidance["blocking_condition"].as_str().unwrap();
    assert!(
        condition.contains("User recovery") && condition.contains("no stop proof"),
        "{condition}"
    );
    assert_eq!(guidance["retained"]["workspace_path"], lease_path);

    // The User continuation the policy refusal alone would suggest is what
    // the kernel refuses next, independently of the actor.
    let user_view = calm_server::task_recovery::task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "b",
        calm_server::ids::ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap();
    assert!(!user_view.recovery.allowed);
    assert_eq!(user_view.recovery.code, "predecessor_not_quiescent");
}

/// #1727 S3 fix G2 — `retained` follows the kernel's worktree events: a
/// `worktree.removed` newer than the last `worktree.provisioned` means the
/// directory and slice branch are gone (`git branch -D`), so only `removed`
/// and the commit object survive; a later re-provision brings the path back.
#[tokio::test]
async fn task_recovery_guidance_retained_follows_worktree_removal_and_reprovision() {
    use calm_server::event::Event;
    let boot = boot().await;
    declare(&boot, ordinary_codex_declaration("b")).await;
    let (_failed, lease_path, _lease_dir) = time_out_prepared_ordinary_worker(&boot, "b").await;
    let retained = || async {
        let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
            .await
            .unwrap();
        listed_entry(&list, "b")["recovery"]["guidance"]["retained"].clone()
    };
    let track_id = boot.track_id.as_str().to_string();
    let card_id = boot.worker_card_id.as_str().to_string();
    let slice_branch = format!("neige/{track_id}/{card_id}");
    assert_eq!(
        retained().await,
        json!({"workspace_path": lease_path, "branch": slice_branch})
    );

    // Removed with no provision recorded: nothing on disk is advertised.
    append_worktree_event(
        &boot,
        Event::WorktreeRemoved {
            track_id: boot.track_id.clone(),
            card_id: boot.worker_card_id.clone(),
            path: lease_path.clone(),
        },
    )
    .await;
    assert_eq!(retained().await, json!({"removed": true}));

    // Re-provisioned after the removal (provisioned id > removed id).
    append_worktree_event(
        &boot,
        Event::WorktreeProvisioned {
            track_id: boot.track_id.clone(),
            card_id: boot.worker_card_id.clone(),
            path: lease_path.clone(),
        },
    )
    .await;
    assert_eq!(
        retained().await,
        json!({"workspace_path": lease_path, "branch": slice_branch})
    );

    // A kernel-recorded commit, then removal again: the object survives.
    append_worktree_event(
        &boot,
        Event::WorktreeCommitted {
            track_id: boot.track_id.clone(),
            card_id: boot.worker_card_id.clone(),
            commit_sha: "abc123def".into(),
            branch: "neige/recorded-branch".into(),
        },
    )
    .await;
    assert_eq!(
        retained().await,
        json!({"workspace_path": lease_path, "branch": "neige/recorded-branch", "last_commit": "abc123def"})
    );
    append_worktree_event(
        &boot,
        Event::WorktreeRemoved {
            track_id: boot.track_id.clone(),
            card_id: boot.worker_card_id.clone(),
            path: lease_path.clone(),
        },
    )
    .await;
    assert_eq!(
        retained().await,
        json!({"removed": true, "last_commit": "abc123def"})
    );
}

/// #1727 S3 fix G4/G5 — a prepared terminal worker has no workspace lease:
/// `retained` is `{}` in full mode and survives as `{}` in summary mode, and
/// the reason never mentions a worktree it does not have.
#[tokio::test]
async fn task_recovery_summary_keeps_an_empty_retained_object_for_a_terminal_worker() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE id=?2")
        .bind(boot.worker_card_id.as_str())
        .bind(&b.id)
        .execute(&pool)
        .await
        .unwrap();
    finish(&boot, &b, false).await;
    for detail in ["full", "summary"] {
        let list = call_tool(
            &boot,
            "calm.plan.list",
            planner_identity(&boot),
            json!({"detail": detail, "key": "b"}),
        )
        .await
        .unwrap();
        let recovery = &listed_entry(&list, "b")["recovery"];
        assert_eq!(recovery["code"], "predecessor_not_quiescent", "{detail}");
        assert!(
            !recovery["reason"].as_str().unwrap().contains("worktree"),
            "{detail}: {}",
            recovery["reason"]
        );
        let guidance = &recovery["guidance"];
        assert_eq!(guidance["supported_continuation"], "new_task", "{detail}");
        assert_eq!(guidance["retained"], json!({}), "{detail}: {guidance}");
        // The guidance sentence is the refusal's reason, verbatim.
        assert_eq!(
            guidance["blocking_condition"], recovery["reason"],
            "{detail}: {guidance}"
        );
        assert!(
            guidance["blocking_condition"]
                .as_str()
                .unwrap()
                .starts_with("an ordinary worker was prepared"),
            "{detail}: {guidance}"
        );
    }
}

/// #1727 S3 fix G5 — verification effects without a worker card: the reason
/// and the guidance say so instead of claiming a worker was prepared.
#[tokio::test]
async fn task_recovery_guidance_names_verification_effects_when_no_worker_was_prepared() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET gate_attempt=1 WHERE id=?1")
        .bind(&b.id)
        .execute(&pool)
        .await
        .unwrap();
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let recovery = &listed_entry(&list, "b")["recovery"];
    assert_eq!(recovery["code"], "predecessor_not_quiescent");
    assert!(
        recovery["reason"]
            .as_str()
            .unwrap()
            .starts_with("verification effects were recorded"),
        "{}",
        recovery["reason"]
    );
    let guidance = &recovery["guidance"];
    assert_eq!(guidance["supported_continuation"], "new_task");
    let condition = guidance["blocking_condition"].as_str().unwrap();
    assert!(
        !condition.contains("worker was prepared")
            && condition.starts_with("verification effects were recorded"),
        "{condition}"
    );
    assert_eq!(guidance["retained"], json!({}));
}

fn isolated_codex_declaration(key: &str) -> Value {
    let mut declaration = ordinary_codex_declaration(key);
    declaration["context"] =
        json!({"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}});
    declaration
}

/// A keyed `codex-isolated-worker` operation row with a preparation receipt
/// (`tx_output_json` set) in `phase`, the way the predecessor fence finds
/// one after normal card/session cleanup. Returns its id.
async fn insert_isolated_operation(boot: &Boot, task: &Task, phase: &str) -> String {
    let id = format!("isolated-{phase}-{}", task.id);
    sqlx::query(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,\
         target_type,target_id,target_json,payload_json,phase,tx_output_json,\
         created_at_ms,updated_at_ms) VALUES(?1,?1,'codex-isolated-worker',?2,'h',\
         'track',?3,'{}','{}',?4,'{}',1,1)",
    )
    .bind(&id)
    .bind(&task.id)
    .bind(boot.track_id.as_str())
    .bind(phase)
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    id
}

/// #1727 S3 fix 2 (K2) — the continuation follows the recorded predecessor,
/// not the declaration's context shape. A task declared with an
/// isolated-shaped `neige_execution` context whose recorded predecessor is
/// an ordinary `codex-worker` (the shape
/// `isolated_selection_does_not_reinterpret_a_recorded_legacy_worker`
/// schedules) times out after preparation: admission refuses on the
/// ordinary fence, and guidance says `new_task`, never `wait_for_settlement`.
#[tokio::test]
async fn task_recovery_guidance_follows_the_recorded_ordinary_predecessor_not_the_context_shape() {
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let boot = boot().await;
    declare(&boot, isolated_codex_declaration("b")).await;
    let task = current(&boot, "b").await;
    // Historical pre-feature Operation: the context was opaque to its legacy producer.
    let payload = serde_json::to_value(
        calm_server::operation::codex_adapter::CodexWorkerOperationPayload {
            actor: calm_server::ids::ActorId::KernelDispatcher,
            track_id: task.track_id.clone(),
            idempotency_key: task.id.clone(),
            goal: task.goal.clone(),
            cwd: None,
            context: serde_json::from_str(&task.context_json).unwrap(),
            acceptance_criteria: task.acceptance_criteria.clone(),
        },
    )
    .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let op = SqlxOperationRepo::new(pool.clone())
        .insert_operation(
            "codex-worker",
            OperationKey {
                operation_key: "historical-worker".into(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let output = calm_server::operation::TxOutput::new(
        "card",
        Some(boot.worker_card_id.to_string()),
        json!({"id":boot.worker_card_id}),
    );
    sqlx::query("UPDATE operations SET phase='succeeded',target_type='card',target_id=?1,tx_output_json=?2 WHERE id=?3")
        .bind(boot.worker_card_id.as_str()).bind(serde_json::to_string(&output).unwrap()).bind(&op).execute(&pool).await.unwrap();
    let (failed, lease_path, _lease_dir) = time_out_prepared_ordinary_worker(&boot, "b").await;
    assert!(calm_server::isolated_codex::selected(&failed).unwrap());

    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let recovery = &listed_entry(&list, "b")["recovery"];
    assert_eq!(recovery["allowed"], false);
    assert_eq!(recovery["code"], "predecessor_not_quiescent", "{recovery}");
    let guidance = &recovery["guidance"];
    assert_eq!(guidance["supported_continuation"], "new_task", "{guidance}");
    assert_eq!(guidance["blocking_condition"], recovery["reason"]);
    assert!(
        recovery["reason"]
            .as_str()
            .unwrap()
            .starts_with("an ordinary worker was prepared"),
        "{recovery}"
    );
    assert_eq!(guidance["retained"]["workspace_path"], lease_path);
}

/// #1727 S3 fix 2 (K2) — behind the Planner retry limit, the actor-independent
/// re-check reaches the isolated fence: the predecessor is still parked, so
/// the continuation is `wait_for_settlement` and the sentence carries both
/// conditions. The refusal code stays what admission returned.
#[tokio::test]
async fn task_recovery_guidance_waits_for_settlement_behind_the_planner_limit() {
    let boot = boot().await;
    declare(&boot, isolated_codex_declaration("b")).await;
    let first = current(&boot, "b").await;
    finish(&boot, &first, false).await;
    recover(&boot, &first, "planner-first").await;
    let second = current(&boot, "b").await;
    assert_ne!(second.id, first.id);
    finish(&boot, &second, false).await;
    // Still running its stop (`parked` itself is CHECK-bound to a real run
    // record; `planner_observes_failure_then_settled_isolated_recovery`
    // reaches the same site with one).
    insert_isolated_operation(&boot, &second, "spawn_succeeded").await;

    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let entry = listed_entry(&list, "b");
    assert_eq!(entry["generation"], 2);
    let recovery = &entry["recovery"];
    assert_eq!(recovery["code"], "recovery_limit_reached", "{recovery}");
    let guidance = &recovery["guidance"];
    assert_eq!(
        guidance["supported_continuation"], "wait_for_settlement",
        "{guidance}"
    );
    let condition = guidance["blocking_condition"].as_str().unwrap();
    assert!(
        condition.starts_with(recovery["reason"].as_str().unwrap())
            && condition.contains("User recovery")
            && condition.contains(". Independently of who asks: ")
            && condition.contains("is in phase spawn_succeeded, not failed")
            && condition.contains("the settlement that records its stop has not completed"),
        "{condition}"
    );
}

/// #1727 S3 fix 2 (K2) — behind a Track that does not schedule, the re-check
/// reaches the ordinary fence: `new_task`, both conditions named, the
/// lifecycle in the policy half.
#[tokio::test]
async fn task_recovery_guidance_names_the_new_task_behind_a_track_that_does_not_schedule() {
    let boot = boot().await;
    declare(&boot, ordinary_codex_declaration("b")).await;
    let (_failed, lease_path, _lease_dir) = time_out_prepared_ordinary_worker(&boot, "b").await;
    sqlx::query("UPDATE tracks SET lifecycle='done' WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let recovery = &listed_entry(&list, "b")["recovery"];
    assert_eq!(recovery["code"], "track_not_ready", "{recovery}");
    let guidance = &recovery["guidance"];
    assert_eq!(guidance["supported_continuation"], "new_task", "{guidance}");
    let condition = guidance["blocking_condition"].as_str().unwrap();
    assert!(
        condition.starts_with(recovery["reason"].as_str().unwrap())
            && condition.contains("(lifecycle done)")
            && condition.contains(". Independently of who asks: an ordinary worker was prepared")
            && condition.contains("no stop proof"),
        "{condition}"
    );
    assert_eq!(guidance["retained"]["workspace_path"], lease_path);
}

/// #1727 S3 fix 2 (K2) — a user-owned task whose declaration was withdrawn
/// (`ready: false`): the Planner is refused with
/// `user_authorization_required`, but the re-check finds nothing current to
/// honour for any actor, so the continuation is `none` and the sentence
/// names the withdrawn declaration rather than promising a User recovery.
#[tokio::test]
async fn task_recovery_guidance_has_no_continuation_for_a_withdrawn_user_owned_declaration() {
    let boot = boot().await;
    let (block_id, _) = declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    // The Planner may not author a user-owned block: rewrite the CRDT
    // authority directly (the bypass
    // `task_recovery_rebuild_uses_authoritative_crdt_when_payload_cache_diverges`
    // uses) and the frozen row's author with it.
    let mut declared = declaration("b", &[]);
    declared["declared_by"] = json!("user");
    declared["ready"] = json!(false);
    let pool = boot.repo.sqlite_pool().unwrap();
    let (card_id, bytes): (String, Vec<u8>) =
        sqlx::query_as("SELECT id, body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(boot.track_id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut doc = calm_server::track_report_doc::ReportDoc::from_bytes(&bytes).unwrap();
    doc.upsert_block(
        Some(&block_id),
        "task",
        &calm_types::report_blocks::render_fence("task", &declared),
    )
    .unwrap();
    sqlx::query("UPDATE cards SET body_crdt=?1 WHERE id=?2")
        .bind(doc.to_bytes())
        .bind(&card_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET declared_by='user' WHERE id=?1")
        .bind(&b.id)
        .execute(&pool)
        .await
        .unwrap();

    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let recovery = &listed_entry(&list, "b")["recovery"];
    assert_eq!(
        recovery["code"], "user_authorization_required",
        "{recovery}"
    );
    let guidance = &recovery["guidance"];
    assert_eq!(guidance["supported_continuation"], "none", "{guidance}");
    let condition = guidance["blocking_condition"].as_str().unwrap();
    assert!(
        condition.starts_with(recovery["reason"].as_str().unwrap())
            && condition.contains(
                ". Independently of who asks: task declaration `b` was withdrawn (ready is false)"
            ),
        "{condition}"
    );
    assert_eq!(guidance["retained"], json!({}));
}

/// #1727 S3 fix 2 (K2) — a permanent isolated denial (the prepared isolated
/// execution shares its key with verification effects): no settlement
/// briefing re-opens it, so the continuation is `none` and the sentence says
/// so instead of telling the Planner to wait.
#[tokio::test]
async fn task_recovery_guidance_has_no_continuation_for_a_permanent_isolated_denial() {
    let boot = boot().await;
    declare(&boot, isolated_codex_declaration("b")).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    insert_isolated_operation(&boot, &b, "failed").await;
    sqlx::query("UPDATE tasks SET gate_attempt=1 WHERE id=?1")
        .bind(&b.id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let recovery = &listed_entry(&list, "b")["recovery"];
    assert_eq!(recovery["code"], "predecessor_not_quiescent", "{recovery}");
    let guidance = &recovery["guidance"];
    assert_eq!(guidance["supported_continuation"], "none", "{guidance}");
    assert_eq!(guidance["blocking_condition"], recovery["reason"]);
    let condition = guidance["blocking_condition"].as_str().unwrap();
    assert!(
        condition.contains("permanently unavailable")
            && condition.contains("no settlement briefing re-opens it")
            && !condition.contains("wait"),
        "{condition}"
    );
    assert_eq!(guidance["retained"], json!({}));
}

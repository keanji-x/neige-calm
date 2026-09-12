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
        assert!(denied.message.contains("descendant write fence"));
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
        assert!(view.recovery.reason.contains("before worker preparation"));
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

//! #1501 regressions through the real projection and execution write helpers.
use super::*;
use crate::db::RepoRead;
use crate::model::TaskStatus;
use calm_types::event::TaskContextRef;
use calm_types::ids::ActorId;
use calm_types::report_blocks::tasks::{
    PLANNER_DECLARATION_AUTHOR, TaskDeclaration, project_task_declarations,
};
use calm_types::report_links::format_track_destination;
use calm_types::task_recovery::{
    TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE, TaskRecoveryReceipt, task_root_hash_preimage,
};
use calm_types::task_recovery::{TaskRecoveryConstraint, TaskRecoveryRequest};
use calm_types::track_report::ReportBlock;
use serde_json::json;
use sha2::{Digest, Sha256};

async fn setup() -> SqlxRepo {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('area','a','#000',0,'user',0,0)")
        .execute(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,lifecycle,created_at,updated_at,require_task_gates) VALUES('w','area','w',0,'working',0,0,0)")
        .execute(repo.pool()).await.unwrap();
    repo
}

fn block(key: &str, depends_on: &[&str]) -> ReportBlock {
    ReportBlock {
        id: format!("b_{key}"),
        kind: "task".into(),
        rev: 0,
        payload: json!({"key":key,"kind":"terminal","command":"true","ready":true,
            "declared_by":PLANNER_DECLARATION_AUTHOR,"depends_on":depends_on}),
    }
}

async fn project(repo: &SqlxRepo, blocks: &[ReportBlock]) -> Vec<TaskDeclaration> {
    let (declarations, diagnostics) = project_task_declarations(blocks);
    // Invalid edits must reach the real projector with their parser diagnostics.
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO cards(id,track_id,kind,sort,payload,title,deletable,created_at,updated_at,role) VALUES('report','w','track-report',0,?1,'report',0,0,0,'reportcard') ON CONFLICT(id) DO UPDATE SET payload=excluded.payload")
        .bind(json!({"blocks":blocks,"docRev":0}).to_string()).execute(&mut *tx).await.unwrap();
    project_tasks_tx(&mut tx, "w", &declarations, &diagnostics)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    declarations
}

#[tokio::test]
async fn task_recovery_initial_registration_survives_pending_projection_deletion() {
    let repo = setup().await;
    let mut task = block("b", &[]);
    project(&repo, std::slice::from_ref(&task)).await;
    let initial = task_attempt_current_pool(repo.pool(), "w", "b")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.attempt_id, "w:b");
    assert_eq!(initial.generation, 1);
    task.payload["ready"] = json!(false);
    project(&repo, std::slice::from_ref(&task)).await;
    assert!(repo.task_get("w:b").await.unwrap().is_none());
    assert_eq!(
        task_attempt_current_pool(repo.pool(), "w", "b")
            .await
            .unwrap(),
        Some(initial.clone())
    );
    task.payload["ready"] = json!(true);
    project(&repo, &[task]).await;
    assert_eq!(
        repo.tasks_by_track("w").await.unwrap()[0].id,
        initial.attempt_id
    );
}

fn constraint(block: &ReportBlock) -> TaskRecoveryConstraint {
    TaskRecoveryConstraint::V1 {
        refs: vec![TaskContextRef {
            track_id: "w".into(),
            block_id: block.id.clone(),
            rev: i64::from(block.rev),
            hash: format!(
                "{:x}",
                Sha256::digest(task_root_hash_preimage(&block.payload))
            ),
            is_root: true,
        }],
        spawn: TASK_IN_TRACK_ROUTE.into(),
        declared_by: PLANNER_DECLARATION_AUTHOR.into(),
    }
}

fn request() -> TaskRecoveryRequest {
    TaskRecoveryRequest {
        expected_attempt_id: "w:b".into(),
        idempotency_key: "request-1".into(),
        reason: "retry unchanged work".into(),
    }
}

async fn fail(repo: &SqlxRepo, block: &ReportBlock) {
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let id = task_attempt_current_tx(&mut tx, "w", block.payload["key"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap()
        .attempt_id;
    assert_eq!(
        task_claim_pending_tx(&mut tx, &id, 1, constraint(block).refs(), false)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        task_fail_from_worker_tx(
            &mut tx,
            &id,
            "w",
            TaskReporter::Kernel,
            "worker-reported: controlled failure",
            2
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
}

async fn recover(repo: &SqlxRepo, block: &ReportBlock) -> TaskRecoveryReceipt {
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let receipt = task_recovery_allocate_tx(
        &mut tx,
        "w",
        "b",
        &request(),
        "fingerprint-1",
        &constraint(block),
        &ActorId::Kernel,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    receipt
}

#[tokio::test]
async fn task_recovery_reprojects_same_key_preserving_failed_attempt_sibling_and_dependencies() {
    let repo = setup().await;
    let blocks = [block("a", &[]), block("b", &[]), block("c", &["a", "b"])];
    project(&repo, &blocks).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    task_claim_pending_tx(&mut tx, "w:a", 1, constraint(&blocks[0]).refs(), false)
        .await
        .unwrap();
    task_complete_from_worker_tx(&mut tx, "w:a", "w", TaskReporter::Kernel, 2)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    fail(&repo, &blocks[1]).await;
    let before_a = repo.task_get("w:a").await.unwrap().unwrap();
    let before_b = repo.task_get("w:b").await.unwrap().unwrap();
    let before_c = repo.task_get("w:c").await.unwrap().unwrap();
    let receipt = recover(&repo, &blocks[1]).await;
    assert!(
        repo.task_current_get("w", "b").await.unwrap().is_none(),
        "allocation may precede projection"
    );
    project(&repo, &blocks).await;
    let current = repo.task_current_get("w", "b").await.unwrap().unwrap();
    assert_eq!(current.id, receipt.attempt_id);
    assert_eq!(current.status, TaskStatus::Pending);
    assert_eq!(current.key, "b");
    assert_eq!(repo.task_get("w:a").await.unwrap().unwrap(), before_a);
    assert_eq!(repo.task_get("w:b").await.unwrap().unwrap(), before_b);
    assert_eq!(repo.task_get("w:c").await.unwrap().unwrap(), before_c);
    assert_eq!(
        repo.task_history_by_key("w", "b").await.unwrap(),
        vec![before_b, current.clone()]
    );
    assert_eq!(repo.tasks_by_track("w").await.unwrap().len(), 3);
    let snapshot = repo.tasks_by_track("w").await.unwrap();
    project(&repo, &blocks).await;
    assert_eq!(
        repo.tasks_by_track("w").await.unwrap(),
        snapshot,
        "rebuild must not rewrite executions"
    );
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    task_claim_pending_tx(
        &mut tx,
        &current.id,
        3,
        constraint(&blocks[1]).refs(),
        false,
    )
    .await
    .unwrap();
    task_complete_from_worker_tx(&mut tx, &current.id, "w", TaskReporter::Kernel, 4)
        .await
        .unwrap();
    assert_eq!(
        task_complete_from_worker_tx(&mut tx, "w:b", "w", TaskReporter::Kernel, 5)
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    let ready = repo.tasks_by_track("w").await.unwrap();
    assert!(
        ready
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .any(|t| t.key == "b")
    );
    assert_eq!(
        repo.task_get("w:c").await.unwrap().unwrap().depends_on(),
        vec!["a", "b"]
    );
}

#[tokio::test]
async fn task_recovery_pending_recreation_keeps_allocation_and_rejects_contract_drift() {
    let repo = setup().await;
    let original = block("b", &[]);
    project(&repo, std::slice::from_ref(&original)).await;
    fail(&repo, &original).await;
    let receipt = recover(&repo, &original).await;
    project(&repo, std::slice::from_ref(&original)).await;
    for field in ["ready", "command", "refs", "spawn", "declared_by"] {
        let mut changed = original.clone();
        changed.payload[field] = match field {
            "ready" => json!(false),
            "command" => json!("echo changed"),
            "refs" => json!([format_track_destination("w", Some("b_0002"))]),
            "spawn" => json!(TASK_CHILD_TRACK_ROUTE),
            "declared_by" => json!("user"),
            _ => unreachable!(),
        };
        project(&repo, &[changed]).await;
        assert!(
            repo.task_current_get("w", "b").await.unwrap().is_none(),
            "{field} cannot retain an executable projection"
        );
        assert_eq!(
            task_attempt_current_pool(repo.pool(), "w", "b")
                .await
                .unwrap()
                .unwrap()
                .attempt_id,
            receipt.attempt_id
        );
        project(&repo, std::slice::from_ref(&original)).await;
        assert_eq!(
            repo.task_current_get("w", "b").await.unwrap().unwrap().id,
            receipt.attempt_id
        );
    }
    assert_eq!(
        repo.task_get("w:b").await.unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn task_recovery_receipt_replays_after_completion_and_rejects_other_requests() {
    let repo = setup().await;
    let b = block("b", &[]);
    project(&repo, std::slice::from_ref(&b)).await;
    fail(&repo, &b).await;
    let receipt = recover(&repo, &b).await;
    project(&repo, std::slice::from_ref(&b)).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    task_claim_pending_tx(
        &mut tx,
        &receipt.attempt_id,
        3,
        constraint(&b).refs(),
        false,
    )
    .await
    .unwrap();
    task_complete_from_worker_tx(&mut tx, &receipt.attempt_id, "w", TaskReporter::Kernel, 4)
        .await
        .unwrap();
    let replay = task_recovery_allocate_tx(
        &mut tx,
        "w",
        "b",
        &request(),
        "fingerprint-1",
        &constraint(&b),
        &ActorId::Kernel,
    )
    .await
    .unwrap();
    assert_eq!(replay, receipt);
    assert!(
        task_recovery_lookup_tx(&mut tx, "w", "b", "request-1", "different")
            .await
            .is_err()
    );
    assert!(
        task_recovery_lookup_tx(&mut tx, "w", "other", "request-1", "fingerprint-1")
            .await
            .is_err()
    );
    let mut new_request = request();
    new_request.idempotency_key = "request-2".into();
    assert!(
        task_recovery_allocate_tx(
            &mut tx,
            "w",
            "b",
            &new_request,
            "fp2",
            &constraint(&b),
            &ActorId::Kernel
        )
        .await
        .is_err()
    );
    new_request.expected_attempt_id = receipt.attempt_id;
    assert!(
        task_recovery_allocate_tx(
            &mut tx,
            "w",
            "b",
            &new_request,
            "fp2",
            &constraint(&b),
            &ActorId::Kernel
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM task_attempt_allocations")
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        2
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn task_recovery_requires_complete_evidence_and_transaction_rolls_back_allocation() {
    let repo = setup().await;
    let b = block("b", &[]);
    project(&repo, std::slice::from_ref(&b)).await;
    fail(&repo, &b).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let invalid = TaskRecoveryConstraint::V1 {
        refs: vec![],
        spawn: TASK_IN_TRACK_ROUTE.into(),
        declared_by: PLANNER_DECLARATION_AUTHOR.into(),
    };
    assert!(
        task_recovery_allocate_tx(
            &mut tx,
            "w",
            "b",
            &request(),
            "fp1",
            &invalid,
            &ActorId::Kernel
        )
        .await
        .is_err()
    );
    assert_eq!(
        task_attempt_current_tx(&mut tx, "w", "b")
            .await
            .unwrap()
            .unwrap()
            .generation,
        1
    );
    let allocated = task_recovery_allocate_tx(
        &mut tx,
        "w",
        "b",
        &request(),
        "fp1",
        &constraint(&b),
        &ActorId::Kernel,
    )
    .await
    .unwrap();
    assert_eq!(
        task_recovery_constraint_tx(&mut tx, &allocated.attempt_id)
            .await
            .unwrap(),
        Some(constraint(&b))
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        task_attempt_current_pool(repo.pool(), "w", "b")
            .await
            .unwrap()
            .unwrap()
            .generation,
        1
    );
}

#[tokio::test]
async fn task_recovery_concurrent_requests_create_one_successor() {
    let repo = setup().await;
    let b = block("b", &[]);
    project(&repo, std::slice::from_ref(&b)).await;
    fail(&repo, &b).await;
    let (first, second) = tokio::join!(recover(&repo, &b), recover(&repo, &b));
    assert_eq!(first, second);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM task_attempt_allocations")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        repo.task_get("w:b").await.unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn task_recovery_gate_log_resolves_current_attempt_and_preserves_role_gate() {
    use crate::model::CardRole;
    use crate::track_fs_view::TrackFsView;
    let repo = setup().await;
    let mut b = block("b", &[]);
    b.payload["gate"] = json!({"steps":[{"name":"check","cmd":"true"}]});
    project(&repo, std::slice::from_ref(&b)).await;
    fail(&repo, &b).await;
    let receipt = recover(&repo, &b).await;
    project(&repo, std::slice::from_ref(&b)).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    task_claim_pending_tx(
        &mut tx,
        &receipt.attempt_id,
        3,
        constraint(&b).refs(),
        false,
    )
    .await
    .unwrap();
    task_start_verifying_from_worker_tx(&mut tx, &receipt.attempt_id, "w", TaskReporter::Kernel, 4)
        .await
        .unwrap();
    assert_eq!(
        task_gate_attempt_bump_tx(&mut tx, &receipt.attempt_id, 1, 5)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let logs = tempfile::tempdir().unwrap();
    std::fs::write(logs.path().join("w:b-g1.log"), "old log").unwrap();
    std::fs::write(
        logs.path().join(format!("{}-g1.log", receipt.attempt_id)),
        "current log",
    )
    .unwrap();
    let write = crate::state::WriteContext::new(
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    );
    let track = repo.track_get("w").await.unwrap().unwrap();
    let view =
        TrackFsView::new(&repo, &write).with_gate_log_access(CardRole::Planner, logs.path().into());
    assert_eq!(
        view.cat(&track, "plan/b/gate.log").await.unwrap().content,
        "current log"
    );
    let worker =
        TrackFsView::new(&repo, &write).with_gate_log_access(CardRole::Worker, logs.path().into());
    assert!(worker.cat(&track, "plan/b/gate.log").await.is_err());
}

#[tokio::test]
async fn task_recovery_domain_deletion_removes_allocations_after_rows() {
    use crate::db::RepoSyncDomainRaw;
    for delete_area in [false, true] {
        let repo = setup().await;
        let b = block("b", &[]);
        project(&repo, std::slice::from_ref(&b)).await;
        fail(&repo, &b).await;
        recover(&repo, &b).await;
        project(&repo, &[b]).await;
        if delete_area {
            let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
            area_delete_tx(&mut tx, "area").await.unwrap();
            tx.commit().await.unwrap();
        } else {
            repo.track_delete("w").await.unwrap();
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM task_attempt_allocations")
                .fetch_one(repo.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tasks")
                .fetch_one(repo.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pragma_foreign_key_check")
                .fetch_one(repo.pool())
                .await
                .unwrap(),
            0
        );
    }
}

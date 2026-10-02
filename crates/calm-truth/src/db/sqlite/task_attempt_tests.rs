use super::*;
use crate::db::RepoRead;
use calm_types::event::TaskContextRef;
use calm_types::ids::ActorId;
use calm_types::report_blocks::tasks::{
    PLANNER_DECLARATION_AUTHOR, TaskDeclaration, project_task_declarations,
};
use calm_types::task_recovery::{
    TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint, task_root_hash_preimage,
};
use calm_types::track_report::ReportBlock;
use serde_json::json;
use sha2::{Digest, Sha256};

async fn setup() -> SqlxRepo {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('area','a','#000',0,'user',0,0)")
        .execute(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at,require_task_gates) VALUES('w','area','w',0,0,0,0)")
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

/// #1893 S4: a block that still selects the deleted isolated path is kept, diagnosed on its
/// blocking `payload` path and never allocated or projected; its ordinary sibling still is.
#[tokio::test]
async fn neige_execution_context_is_not_projected() {
    let repo = setup().await;
    let mut retired = block("retired", &[]);
    retired.payload = json!({"key":"retired","kind":"codex","goal":"research","ready":true,
        "declared_by":PLANNER_DECLARATION_AUTHOR,"no_gate_reason":"report-reviewed",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}});
    let blocks = [retired, block("ordinary", &[])];
    let (_, diagnostics) = project_task_declarations(&blocks);
    assert_eq!(
        diagnostics[0]
            .iter()
            .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.path.as_str()))
            .collect::<Vec<_>>(),
        [("neige_execution_retired", "payload")]
    );
    assert!(diagnostics[1].is_empty());
    project(&repo, &blocks).await;
    assert_eq!(
        repo.tasks_by_track("w")
            .await
            .unwrap()
            .iter()
            .map(|task| task.key.as_str())
            .collect::<Vec<_>>(),
        ["ordinary"]
    );
    assert!(
        task_attempt_current_pool(repo.pool(), "w", "retired")
            .await
            .unwrap()
            .is_none()
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

/// A released recovery allocation, inserted the way 4140 stores it: no code writes one any more.
async fn recover(repo: &SqlxRepo, block: &ReportBlock) -> String {
    let attempt_id = "w:b:recovered".to_string();
    let origin = TaskAttemptOrigin::Recovery {
        previous_attempt_id: "w:b".into(),
        idempotency_key: "request-1".into(),
        request_fingerprint: "fingerprint-1".into(),
        reason: "retry unchanged work".into(),
        actor: ActorId::Kernel,
        constraint: constraint(block),
    };
    sqlx::query(
        "INSERT INTO task_attempt_allocations \
         (attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES (?1,'w','b',2,?2,0)",
    )
    .bind(&attempt_id)
    .bind(serde_json::to_string(&origin).unwrap())
    .execute(repo.pool())
    .await
    .unwrap();
    attempt_id
}

#[tokio::test]
async fn task_recovery_gate_log_reads_the_recovered_attempt() {
    use crate::track_fs_view::TrackFsView;
    let repo = setup().await;
    let mut b = block("b", &[]);
    b.payload["gate"] = json!({"steps":[{"name":"check","cmd":"true"}]});
    project(&repo, std::slice::from_ref(&b)).await;
    fail(&repo, &b).await;
    let attempt_id = recover(&repo, &b).await;
    project(&repo, std::slice::from_ref(&b)).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    task_claim_pending_tx(&mut tx, &attempt_id, 3, constraint(&b).refs(), false)
        .await
        .unwrap();
    task_start_verifying_from_worker_tx(&mut tx, &attempt_id, "w", TaskReporter::Kernel, 4)
        .await
        .unwrap();
    assert_eq!(
        task_gate_attempt_bump_tx(&mut tx, &attempt_id, 1, 5)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let logs = tempfile::tempdir().unwrap();
    std::fs::write(logs.path().join("w:b-g1.log"), "old log").unwrap();
    std::fs::write(
        logs.path().join(format!("{}-g1.log", attempt_id)),
        "current log",
    )
    .unwrap();
    let write = crate::state::WriteContext::new(
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    );
    let track = repo.track_get("w").await.unwrap().unwrap();
    let view = TrackFsView::new(&repo, &write).with_gate_log_access(logs.path().into());
    let current = format!("runs/{attempt_id}/gates/1.log");
    assert_eq!(
        view.cat(&track, &current).await.unwrap().content,
        "current log"
    );
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

mod followup;

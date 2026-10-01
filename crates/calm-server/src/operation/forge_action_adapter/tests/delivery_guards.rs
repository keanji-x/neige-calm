//! Kernel delivery must take over the checkout after every producer writer stops.
use super::*;
use crate::git_candidate::delivery::{
    AttemptOutcome, delivery_latest_for_attempt_tx, submit_delivery,
};
use crate::operation::workspace_lease::execution_guard::{
    acquire_execution_write_tx, release_stopped_execution_tx,
};
use crate::operation::workspace_lease::{
    DeliveryPolicy, LeaseBase, ReleaseDelivery, WorkerLeasePlan, acquire_workspace_lease_tx, base,
    release_workspace_lease_for_card_tx,
};

fn git_at(dir: &Path, args: &[&str]) -> String {
    let output = crate::workspace_materialize::neige_git_command()
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn writer_delivery_waits_for_stop(kind: &str) {
    let fx = forge_runtime_fixture().await;
    let branch =
        crate::operation::workspace_lease::track_worktree::track_branch_for(&fx.track_id).unwrap();
    git_at(fx.cwd.path(), &["init", "-q", "-b", &branch]);
    git_at(
        fx.cwd.path(),
        &["config", "user.email", "delivery@example.test"],
    );
    git_at(fx.cwd.path(), &["config", "user.name", "Delivery Fence"]);
    std::fs::write(fx.cwd.path().join("initial"), "initial").unwrap();
    git_at(fx.cwd.path(), &["add", "initial"]);
    git_at(fx.cwd.path(), &["commit", "-q", "-m", "initial"]);
    let head = git_at(fx.cwd.path(), &["rev-parse", "HEAD"]);
    let card = crate::db::RepoSyncDomainRaw::card_create(
        fx.repo.as_ref(),
        crate::model::NewCard {
            track_id: crate::ids::TrackId::from(fx.track_id.clone()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        },
    )
    .await
    .unwrap()
    .id
    .to_string();
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    sqlx::query(
        r#"
INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms)
VALUES('delivery-attempt',?1,'delivery-task',1,'{"kind":"initial"}',1)
"#,
    )
    .bind(&fx.track_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(r#"
INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,created_at_ms,updated_at_ms)
VALUES('delivery-attempt',?1,'delivery-task','codex','implementation','{}','done',?2,1,1)
"#).bind(&fx.track_id).bind(&card).execute(&mut *tx).await.unwrap();
    let plan = WorkerLeasePlan {
        path: fx.cwd.path().to_path_buf(),
        branch: branch.clone(),
        superseded: Vec::new(),
        access_mode: calm_types::workspace_access::WorkspaceAccess::ReadWrite,
        base: LeaseBase {
            base_sha: head.clone(),
            base_source: base::BaseSource::Head,
            base_attempt_id: None,
            canonical_path: fx.cwd.path().canonicalize().unwrap(),
            git_common_dir: fx.cwd.path().join(".git").canonicalize().unwrap(),
        },
    };
    let (lease, _) =
        acquire_workspace_lease_tx(&mut tx, &card, &fx.track_id, "worker-owner", &plan)
            .await
            .unwrap();
    assert_eq!(lease.delivery_policy, Some(DeliveryPolicy::Kernel));
    let _writer = acquire_execution_write_tx(
        &mut tx,
        &fx.track_id,
        &card,
        "live-writer",
        kind,
        fx.cwd.path(),
    )
    .await
    .unwrap();
    release_workspace_lease_for_card_tx(
        &mut tx,
        &card,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .unwrap();
    let delivery = delivery_latest_for_attempt_tx(&mut tx, "delivery-attempt")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    std::fs::write(fx.cwd.path().join("worker.txt"), "before the writer stops").unwrap();
    let submission = submit_delivery(&fx.runtime, fx.results.path(), &delivery, &lease, &branch)
        .await
        .unwrap();
    let before_stop = fx
        .runtime
        .operation_result(&submission.op_id)
        .await
        .unwrap();
    let before_phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
        .bind(&submission.op_id)
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    let before_head = git_at(fx.cwd.path(), &["rev-parse", "HEAD"]);
    // Always release the test's durable reference before assertions so even the red test leaves no parked process.
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    release_stopped_execution_tx(&mut tx, kind, "live-writer")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    std::fs::write(
        fx.cwd.path().join("worker.txt"),
        "final stopped writer output",
    )
    .unwrap();
    let retry = submit_delivery(&fx.runtime, fx.results.path(), &delivery, &lease, &branch)
        .await
        .unwrap();
    assert_eq!(
        retry.op_id, submission.op_id,
        "resume the persisted delivery operation key"
    );
    let result = tokio::time::timeout(Duration::from_secs(5), fx.runtime.wait(&retry.op_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before_phase, "pending",
        "the delivery must wait before spawning while any writer remains"
    );
    assert!(
        before_stop.is_none(),
        "busy delivery must remain retryable, got {before_stop:?}"
    );
    assert_eq!(
        before_head, head,
        "the report must not commit while a producer writer is still held"
    );
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{result:?}"
    );
    assert_ne!(git_at(fx.cwd.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git_at(fx.cwd.path(), &["show", "HEAD:worker.txt"]),
        "final stopped writer output"
    );
}

#[tokio::test]
async fn kernel_delivery_waits_for_native_producer_stop_and_retries_same_key() {
    writer_delivery_waits_for_stop("native").await;
}

#[tokio::test]
async fn kernel_delivery_waits_for_terminal_producer_stop_and_retries_same_key() {
    writer_delivery_waits_for_stop("terminal").await;
}

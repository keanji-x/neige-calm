//! #1830 S2 — how each attempt in the track's checkout ends
//! (`docs/architecture/1830-s2-worker-in-track-worktree.md` §3, §6 T6–T11): a managed track's
//! worker on `main` in its directory, the release a timeout flip, a boot reclaim,
//! a failed Codex interrupt and a stuck owner make. The world is [`crate::track_worker_cwd`]'s.
use std::path::Path;

use calm_server::model::TaskStatus;
use calm_server::session_projection_repo::AgentProvider;
use serde_json::json;

use crate::git_delivery::git;
use crate::mcp_track_report::{call_tool, planner_identity};
use crate::task_recovery::current;
use crate::track_worker_cwd::{
    candidate_commit, declare_task, delivery_outcome, lease_state, wait_running, worktree_entries,
    world,
};

/// T6 (D1, D4) — a managed track's worker runs in `workspace.path`, and its attempt is committed
/// on `main` there; no worktree is registered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_managed_track_worker_commits_on_main_in_its_directory() {
    let w = world().await;
    let fx = &w.fx;
    let path = calm_server::workspace_materialize::managed_workspace_path(
        &fx.workspace_root,
        fx.boot.area_id.as_str(),
        fx.track(),
    );
    sqlx::query(
        "UPDATE tracks SET workspace_kind = 'managed', workspace_path = ?1, \
         workspace_worktree_path = NULL WHERE id = ?2",
    )
    .bind(path.to_str().unwrap())
    .bind(fx.track())
    .execute(&fx.pool())
    .await
    .unwrap();
    // Materialize it the way a worker's prepare does, so the commit identity can be set first.
    let mut tx = fx.pool().begin().await.unwrap();
    calm_server::test_seams::prepare_worker_lease_for_test(&mut tx, fx.track(), &fx.workspace_root)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    git(&path, &["config", "user.email", "managed@example.test"]);
    git(&path, &["config", "user.name", "Managed Test"]);

    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    assert_eq!(a.cwd, path, "the worker runs in the managed directory");
    std::fs::write(a.cwd.join("a.txt"), "managed\n").unwrap();
    a.complete(fx).await;

    let commit = candidate_commit(fx, &a.task.id).await;
    assert_eq!(git(&path, &["rev-parse", "refs/heads/main"]), commit);
    assert_eq!(worktree_entries(&path), 1, "no worktree is registered");
}

/// T8 (D7, `mark_running_timeout_cleanup_tx`) — the worker's session already `exited` when its
/// liveness deadline passes: one reconcile sweep fails the task, and the flip itself releases the
/// lease with `outcome = 'failed'`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exited_worker_is_released_by_its_timeout_flip() {
    let w = world().await;
    let fx = &w.fx;
    let worker = fx.new_worker("a", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("a", "codex", &worker.card_id, json!({}))
        .await;
    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE card_id = ?1")
        .bind(&worker.card_id)
        .execute(&fx.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET running_deadline_ms = 1 WHERE id = ?1")
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let scheduler = fx.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();

    scheduler.sweep_all().await;

    assert_eq!(current(&fx.boot, "a").await.status, TaskStatus::Failed);
    assert_eq!(lease_state(fx, &lease.lease_id).await, "released");
    assert_eq!(
        delivery_outcome(fx, &task.id).await.unwrap().0,
        "failed",
        "the flip transaction wrote the first delivery row"
    );
}

/// T9 (D7 boot reclaim) — a lease from an older machine boot: the reboot fails its attempt with
/// the dead-worker detail and releases it with `outcome = 'interrupted'`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lease_from_an_older_boot_fails_its_attempt_and_is_released() {
    let mut w = world().await;
    let worker = w.fx.new_worker("a", AgentProvider::Codex).await;
    let lease = w.fx.kernel_lease(&worker.card_id).await;
    let task =
        w.fx.running_task("a", "codex", &worker.card_id, json!({}))
            .await;
    sqlx::query("UPDATE workspace_leases SET boot_id = 'stale-boot' WHERE lease_id = ?1")
        .bind(&lease.lease_id)
        .execute(&w.fx.pool())
        .await
        .unwrap();

    w.fx.reboot().await;

    let fx = &w.fx;
    assert_eq!(lease_state(fx, &lease.lease_id).await, "released");
    let a = current(&fx.boot, "a").await;
    assert_eq!(a.status, TaskStatus::Failed);
    assert!(
        a.status_detail
            .as_deref()
            .is_some_and(|detail| detail.starts_with("spawn-failed: ")),
        "{a:?}"
    );
    assert_eq!(
        delivery_outcome(fx, &task.id).await.unwrap().0,
        "interrupted"
    );
}

/// The cleanup marker of the card's worker session, if any.
async fn cleanup_marker(fx: &crate::git_delivery::Fx, card_id: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT json_extract(handle_state_json, '$.timeout_cleanup') FROM worker_sessions \
         WHERE card_id = ?1",
    )
    .bind(card_id)
    .fetch_one(&fx.pool())
    .await
    .unwrap()
}

async fn cancel(fx: &crate::git_delivery::Fx, key: &str) {
    call_tool(
        &fx.boot,
        "calm.plan.cancel",
        planner_identity(&fx.boot),
        json!({"key": key, "message": "stop"}),
    )
    .await
    .unwrap();
}

/// T10 (D7 interrupt) — a canceled running worker whose Codex interrupt fails keeps its lease and
/// its cleanup marker; once the interrupt goes through, the next sweep releases it with
/// `outcome = 'canceled'`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_codex_interrupt_keeps_the_lease_until_it_succeeds() {
    let w = world().await;
    let fx = &w.fx;
    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    let scheduler = fx.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    w.shared.fail_turn_interrupt_for_test(true);

    cancel(fx, "a").await;
    scheduler.sweep_all().await;

    assert_eq!(current(&fx.boot, "a").await.status, TaskStatus::Canceled);
    assert_eq!(lease_state(fx, &a.lease_id).await, "held");
    assert!(cleanup_marker(fx, &a.identity.card_id).await.is_some());

    w.shared.fail_turn_interrupt_for_test(false);
    scheduler.sweep_all().await;

    assert_eq!(lease_state(fx, &a.lease_id).await, "released");
    assert_eq!(
        delivery_outcome(fx, &a.task.id).await.unwrap().0,
        "canceled"
    );
}

/// `calm.plan.cancel` of a running worker commits what it left, as `canceled`, after the kill.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_canceled_worker_is_committed_as_canceled_after_the_kill() {
    let w = world().await;
    let fx = &w.fx;
    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    std::fs::write(a.cwd.join("partial.txt"), "half done\n").unwrap();

    cancel(fx, "a").await;

    let commit = candidate_commit(fx, &a.task.id).await;
    assert_eq!(
        delivery_outcome(fx, &a.task.id).await.unwrap().0,
        "canceled"
    );
    assert_eq!(lease_state(fx, &a.lease_id).await, "released");
    assert_eq!(git(&a.cwd, &["rev-parse", "HEAD"]), commit);
    let message = git(&a.cwd, &["log", "-1", "--format=%s"]);
    assert!(
        message.contains(&format!("attempt {} canceled", a.task.id)),
        "{message}"
    );
    assert!(Path::new(&a.cwd).join("partial.txt").is_file());
}

/// T11 (D5 stuck exception, D7 supersede) — `a`'s worker op is stuck and `a` failed, its lease
/// still held on a clean tree: `b` is claimed and runs in the worktree; `a`'s lease is released
/// with no delivery row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stuck_owner_lease_does_not_block_the_next_claim() {
    let w = world().await;
    let fx = &w.fx;
    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    sqlx::query(
        "UPDATE operations SET phase = 'stuck', \
         phase_detail_json = '{\"reason\":\"test stuck\",\"since\":1}' \
         WHERE kind = 'codex-worker' AND idempotency_key = ?1",
    )
    .bind(&a.task.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE tasks SET status = 'failed', status_detail = 'spawn-failed: stuck' WHERE id = ?1",
    )
    .bind(&a.task.id)
    .execute(&fx.pool())
    .await
    .unwrap();

    declare_task(fx, "b", json!({})).await;
    let b = wait_running(fx, "b").await;

    assert_eq!(b.cwd, fx.worktree);
    assert_eq!(lease_state(fx, &a.lease_id).await, "released");
    assert_eq!(
        delivery_outcome(fx, &a.task.id).await,
        None,
        "no delivery row"
    );
    assert_eq!(lease_state(fx, &b.lease_id).await, "held");
}

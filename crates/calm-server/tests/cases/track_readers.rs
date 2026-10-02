//! #1917 — read-only tasks share the track's checkout
//! (`docs/architecture/1917-readonly-tasks.md`): readers run beside each other, a task that
//! changes the checkout runs alone, a waiting writer holds back the readers after it, and a
//! reader's report releases its lease with no delivery.
//!
//! The world is #1830 S2's ([`crate::track_worker_cwd::world`]): the real `CodexWorkerAdapter` on a
//! fake Codex daemon, a live Dispatcher and scheduler; the test plays each worker.
use serde_json::json;

use calm_server::db::sqlite::TaskPendingReason;
use calm_server::model::TaskStatus;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;

use crate::git_delivery::Fx;
use crate::mcp_track_report::{planner_identity, upsert_block};
use crate::task_recovery::{current, declare};
use crate::track_worker_cwd::{
    Started, candidate_commit, declare_task, delivery_outcome, lease_state, wait_running,
    wait_task, world,
};

const IN_USE: &str = "Waiting for the track's checkout: another task is using it";
const WRITER_AHEAD: &str = "Waiting for the track's checkout: a task that changes it goes first";

/// Declare a read-only codex task, ready (no gate, no `no_gate_reason`).
async fn declare_reader(fx: &Fx, key: &str) {
    declare(
        &fx.boot,
        json!({
            "key": key, "kind": "codex", "goal": format!("review {key}"),
            "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "access": "read_only",
        }),
    )
    .await;
}

/// Run one scheduling pass; `key` stays pending, and the report read says why (`trackBusy`).
async fn pending_message(fx: &Fx, key: &str) -> String {
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    assert_eq!(current(&fx.boot, key).await.status, TaskStatus::Pending);
    let read = calm_server::track_report_read::load_report_read_snapshot(
        fx.boot.repo.as_ref(),
        fx.boot.report_card_id.as_str(),
    )
    .await
    .unwrap();
    let verdict = read
        .task_diagnostics
        .iter()
        .find(|verdict| verdict.key == key)
        .unwrap_or_else(|| panic!("no verdict for {key}"));
    match verdict.pending_reason.clone() {
        Some(TaskPendingReason::TrackBusy { message }) => message,
        other => panic!("{key}: expected trackBusy, got {other:?}"),
    }
}

async fn lease_access(fx: &Fx, started: &Started) -> (String, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT access_mode, base_sha, delivery_policy FROM workspace_leases WHERE lease_id = ?1",
    )
    .bind(&started.lease_id)
    .fetch_one(&fx.pool())
    .await
    .unwrap()
}

/// Two readers run in the checkout at once; a writer waits for both; each reader's report
/// releases its lease with no delivery row and the task ends `done`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_share_the_checkout_and_a_writer_waits_for_them() {
    let w = world().await;
    let fx = &w.fx;
    declare_reader(fx, "r1").await;
    let r1 = wait_running(fx, "r1").await;
    declare_reader(fx, "r2").await;
    let r2 = wait_running(fx, "r2").await;
    assert_eq!(r1.cwd, fx.worktree, "readers run in the track worktree");
    assert_eq!(r2.cwd, fx.worktree);
    for reader in [&r1, &r2] {
        assert_eq!(
            lease_access(fx, reader).await,
            ("read_only".into(), None, None),
            "a reader's lease records no base and no delivery policy"
        );
        let prompt: String =
            sqlx::query_scalar("SELECT json_extract(payload, '$.prompt') FROM cards WHERE id = ?1")
                .bind(&reader.identity.card_id)
                .fetch_one(&fx.pool())
                .await
                .unwrap();
        assert!(
            prompt.contains("This task is read-only: do not modify the checkout"),
            "{prompt}"
        );
    }

    declare_task(fx, "w", json!({})).await;
    assert_eq!(pending_message(fx, "w").await, IN_USE);

    r1.complete(fx).await;
    let done = wait_task(fx, "r1", |task| task.status == TaskStatus::Done).await;
    assert_eq!(done.status_detail, None);
    assert_eq!(lease_state(fx, &r1.lease_id).await, "released");
    assert_eq!(delivery_outcome(fx, &r1.task.id).await, None, "no delivery");
    assert!(
        fx.worktree_committed_events(&r1.identity.card_id)
            .await
            .is_empty()
    );
    assert_eq!(
        pending_message(fx, "w").await,
        IN_USE,
        "the writer still waits for r2"
    );

    r2.complete(fx).await;
    let writer = wait_running(fx, "w").await;
    assert_eq!(delivery_outcome(fx, &r2.task.id).await, None);
    assert_eq!(
        lease_access(fx, &writer).await.0,
        "read_write",
        "the writer's lease is a kernel-delivery lease"
    );
    assert!(lease_access(fx, &writer).await.1.is_some());
}

/// A reader waits for a running writer, then for the writer's delivery to land, and starts on
/// the writer's commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_waits_for_a_running_writer_and_its_delivery() {
    let w = world().await;
    let fx = &w.fx;
    let release = fx.track_root.parent().unwrap().join("release-commit");
    crate::git_delivery::write_executable(
        &fx.track_root.join(".git/hooks/pre-commit"),
        &format!(
            "#!/bin/sh\nwhile [ ! -e '{}' ]; do sleep 0.05; done\n",
            release.display()
        ),
    );
    declare_task(fx, "w", json!({})).await;
    let writer = wait_running(fx, "w").await;
    declare_reader(fx, "r").await;
    assert_eq!(pending_message(fx, "r").await, IN_USE);

    std::fs::write(writer.cwd.join("w.txt"), "w\n").unwrap();
    writer.complete(fx).await;
    wait_task(fx, "w", |task| task.status == TaskStatus::Done).await;
    assert_eq!(
        pending_message(fx, "r").await,
        IN_USE,
        "an unsettled delivery holds the reader"
    );

    std::fs::write(&release, "").unwrap();
    let commit = candidate_commit(fx, &writer.task.id).await;
    let reader = wait_running(fx, "r").await;
    assert_eq!(
        crate::git_delivery::git(&reader.cwd, &["rev-parse", "HEAD"]),
        commit
    );
}

/// No starvation: with r1 running, a deps-ready writer ahead of r2 in scheduler order waits for
/// r1 and holds r2 back; r2 runs after the writer's delivery lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_waiting_writer_holds_back_later_readers() {
    let w = world().await;
    let fx = &w.fx;
    declare_reader(fx, "r1").await;
    let r1 = wait_running(fx, "r1").await;
    declare_task(fx, "w", json!({})).await;
    declare_reader(fx, "r2").await;
    assert_eq!(pending_message(fx, "w").await, IN_USE);
    assert_eq!(pending_message(fx, "r2").await, WRITER_AHEAD);

    r1.complete(fx).await;
    let writer = wait_running(fx, "w").await;
    assert_eq!(pending_message(fx, "r2").await, IN_USE);
    writer.complete(fx).await;
    candidate_commit(fx, &writer.task.id).await;
    wait_running(fx, "r2").await;
}

/// The declaration rules reach the Planner's report write: a read-only task takes no gate, is a
/// codex or claude task, and runs in the track (each refusal names the valid choices).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_read_only_declarations_are_refused_at_the_write() {
    let boot = crate::mcp_track_report::boot().await;
    let base = json!({
        "key": "review", "kind": "codex", "goal": "review the change", "ready": true,
        "declared_by": PLANNER_DECLARATION_AUTHOR, "access": "read_only",
    });
    let with = |extra: serde_json::Value| {
        let mut payload = base.clone();
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        payload
    };
    for (payload, expected) in [
        (
            with(json!({"gate": {"steps": [{"name": "t", "cmd": "true"}]}})),
            "access: \"read_only\" takes no gate",
        ),
        (
            with(json!({"kind": "terminal", "goal": null, "command": "true"})),
            "access: \"read_only\" requires kind \"codex\" or \"claude\"",
        ),
        (
            with(json!({"spawn": calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE})),
            "access: \"read_only\" runs in the track's checkout",
        ),
        (
            with(json!({"access": "write"})),
            "access: must be one of \"read_only\" | \"read_write\"",
        ),
    ] {
        let mut payload = payload;
        payload.as_object_mut().unwrap().retain(|_, v| !v.is_null());
        let error = upsert_block(
            &boot,
            planner_identity(&boot),
            json!({"kind": "task", "payload": payload}),
        )
        .await
        .expect_err("the write is refused");
        assert!(error.message.contains(expected), "{error:?}");
    }
    upsert_block(
        &boot,
        planner_identity(&boot),
        json!({"kind": "task", "payload": base}),
    )
    .await
    .expect("a gate-less read-only codex task is a valid declaration");
}

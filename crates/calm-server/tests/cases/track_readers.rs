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
use crate::mcp_track_report::{call_tool, planner_identity, upsert_block};
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

/// #2554, the 4140 case: a writer waiting for the checkout has no worker yet. The Planner's message
/// and read by its attempt are refused with `task_pending`, and its task block still takes the edit
/// the refusal points to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_waiting_for_the_checkout_refuses_terminal_tools_and_takes_an_edit() {
    let w = world().await;
    let fx = &w.fx;
    let providers = calm_server::provider_registry::WorkerProviderRegistry::for_daemon(
        &std::sync::Arc::new(calm_server::state::DaemonClient::new_stub()),
        w.shared.clone(),
        fx.harness.clone(),
    );
    fx.boot
        .ctx
        .terminal_interaction
        .set(std::sync::Arc::new(
            calm_server::terminal_interaction::TerminalInteraction::new(
                fx.boot.ctx.repo.clone(),
                calm_server::terminal_renderer::TerminalRendererRegistry::new(),
                providers,
            ),
        ))
        .unwrap_or_else(|_| panic!("terminal interaction already set"));
    // The terminal tools authorize the caller by its card's live session, as a running Planner's.
    let planner = planner_identity(&fx.boot);
    (fx.db())
        .writer()
        .execute(
            sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                .bind(&planner.session_id)
                .bind(&planner.card_id),
        )
        .await
        .unwrap();
    declare_task(fx, "w1", json!({})).await;
    wait_running(fx, "w1").await;
    let mut w2 = json!({
        "key": "w2", "kind": "codex", "goal": "work on w2",
        "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "no_gate_reason": "fixture",
    });
    let (block, _) = declare(&fx.boot, w2.clone()).await;
    assert_eq!(pending_message(fx, "w2").await, IN_USE);
    let attempt = current(&fx.boot, "w2").await.id;
    let text = format!(
        "attempt {attempt} (task w2) has not started, so it has no worker yet; neige_task_ls shows \
         why it waits. To change what it will do, edit its task block: its worker starts with it"
    );
    for (tool, args) in [
        (
            "neige_terminal_input",
            json!({"attempt_id":attempt,"idempotency_key":"m",
                "action":{"type":"message","text":"narrow the scope"}}),
        ),
        (
            "neige_terminal_read",
            json!({"attempt_id":attempt,"wait_ms":50}),
        ),
    ] {
        let error = call_tool(&fx.boot, tool, planner.clone(), args)
            .await
            .expect_err(tool);
        assert_eq!(error.code, -32403, "{tool}: {error:?}");
        assert_eq!(error.message, format!("{tool}: {text}"));
        assert_eq!(
            error.data,
            Some(json!({"refusal":"task_pending"})),
            "{tool}"
        );
    }
    // The edit the refusal points to: the same block, a new goal, taken while still pending.
    w2["goal"] = json!("narrow the scope");
    upsert_block(
        &fx.boot,
        planner,
        json!({"id": block, "kind": "task", "payload": w2}),
    )
    .await
    .unwrap();
    let edited = current(&fx.boot, "w2").await;
    assert_eq!(
        (edited.id, edited.status, edited.goal.as_str()),
        (attempt, TaskStatus::Pending, "narrow the scope")
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
            "access: \"read_only\" requires an agent task",
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_read_preserves_mixed_sources_on_repeated_reads() {
    use calm_types::track_fs_dto::TaskMessageSource;
    let w = world().await;
    let fx = &w.fx;
    declare_reader(fx, "messages").await;
    let worker = wait_running(fx, "messages").await;
    let attempt = worker.task.id.clone();
    let ids = calm_server::db::write_in_tx_typed(fx.boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let planner = calm_server::db::sqlite::planner_message_insert_tx(
                tx,
                &attempt,
                "planner",
                "Planner instruction",
            )
            .await?;
            let user = calm_server::db::sqlite::planner_message_insert_with_source_tx(
                tx,
                &attempt,
                "user",
                "User clarification",
                TaskMessageSource::User,
            )
            .await?;
            Ok::<_, calm_server::error::CalmError>((planner, user))
        })
    })
    .await
    .unwrap();
    let read = || {
        call_tool(
            &fx.boot,
            "neige_message_read",
            worker.identity.clone(),
            json!({"attempt_id":worker.task.id}),
        )
    };
    let first = read().await.unwrap();
    let messages = first["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["message_id"], ids.0);
    assert_eq!(messages[0]["seq"], 1);
    assert_eq!(messages[0]["text"], "Planner instruction");
    assert!(
        messages[0].get("source").is_none(),
        "historical Planner shape remains optional"
    );
    assert_eq!(messages[1]["message_id"], ids.1);
    assert_eq!(messages[1]["seq"], 2);
    assert_eq!(messages[1]["text"], "User clarification");
    assert_eq!(messages[1]["source"], "user");
    assert!(messages.iter().all(|m| m["read_at"].is_string()));
    assert_eq!(
        read().await.unwrap(),
        first,
        "repeat reads preserve sources and read timestamps"
    );
    worker.complete(fx).await;
    let reported = read().await.unwrap();
    assert_eq!(reported["status"], "messages_are_not_work");
    assert_eq!(reported["messages"], first["messages"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_pre_upgrade_output_completes_without_sampling_checkout() {
    for kind in ["codex", "claude"] {
        for common_dir in [None, Some(json!("")), Some(json!(null))] {
            let w = world().await;
            let fx = &w.fx;
            declare_task(fx, "legacy", json!({"kind":kind,"access":"read_only"})).await;
            let mut worker = wait_running(fx, "legacy").await;
            worker.identity.provider = if kind == "codex" {
                calm_server::session_projection_repo::AgentProvider::Codex
            } else {
                calm_server::session_projection_repo::AgentProvider::Claude
            };
            let no_base: bool = sqlx::query_scalar(
                "SELECT base_sha IS NULL AND base_source IS NULL AND base_attempt_id IS NULL \
                 AND canonical_path IS NULL AND git_common_dir IS NULL FROM workspace_leases WHERE lease_id=?1",
            ).bind(&worker.lease_id).fetch_one(&fx.pool()).await.unwrap();
            assert!(no_base, "pre-upgrade read-only leases have no LeaseBase");
            // Both pre-upgrade adapters persisted canonical_path/base_sha but no git_common_dir.
            let output: String = sqlx::query_scalar(
                "SELECT tx_output_json FROM operations WHERE idempotency_key=?1",
            )
            .bind(&worker.task.id)
            .fetch_one(&fx.pool())
            .await
            .unwrap();
            let mut output: serde_json::Value = serde_json::from_str(&output).unwrap();
            output["data"]
                .as_object_mut()
                .unwrap()
                .remove("git_common_dir");
            if let Some(value) = common_dir {
                output["data"]["git_common_dir"] = value;
            }
            fx.db()
                .writer()
                .execute(
                    sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE idempotency_key=?2")
                        .bind(output.to_string())
                        .bind(&worker.task.id),
                )
                .await
                .unwrap();
            let moved = worker.cwd.with_extension("unavailable");
            std::fs::rename(&worker.cwd, &moved).unwrap();
            let result = call_tool(
                &fx.boot,
                "neige_task_done",
                worker.identity.clone(),
                json!({"attempt_id":worker.task.id}),
            )
            .await;
            std::fs::rename(&moved, &worker.cwd).unwrap();
            assert_eq!(result.unwrap()["status"], "report_received", "{kind}");
            assert_eq!(current(&fx.boot, "legacy").await.status, TaskStatus::Done);
            assert_eq!(lease_state(fx, &worker.lease_id).await, "released");
            assert_eq!(delivery_outcome(fx, &worker.task.id).await, None);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_post_upgrade_output_still_samples_without_lease_base() {
    for kind in ["codex", "claude"] {
        let w = world().await;
        let fx = &w.fx;
        declare_task(fx, "modern", json!({"kind":kind,"access":"read_only"})).await;
        let mut worker = wait_running(fx, "modern").await;
        worker.identity.provider = if kind == "codex" {
            calm_server::session_projection_repo::AgentProvider::Codex
        } else {
            calm_server::session_projection_repo::AgentProvider::Claude
        };
        assert!(worker.base_sha.is_empty());
        let dirty = worker.cwd.join("untracked.txt");
        std::fs::write(&dirty, "must refuse completion").unwrap();
        let result = call_tool(
            &fx.boot,
            "neige_task_done",
            worker.identity.clone(),
            json!({"attempt_id":worker.task.id}),
        )
        .await;
        std::fs::remove_file(dirty).unwrap();
        let error = result.unwrap_err();
        assert!(
            error.message.contains("worker-read-only-changed"),
            "{error:?}"
        );
        assert_eq!(
            current(&fx.boot, "modern").await.status,
            TaskStatus::Running
        );
        assert_eq!(lease_state(fx, &worker.lease_id).await, "held");
        worker.complete(fx).await;
        assert_eq!(current(&fx.boot, "modern").await.status, TaskStatus::Done);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_recorded_base_does_not_take_pre_upgrade_bypass() {
    let w = world().await;
    let fx = &w.fx;
    declare_reader(fx, "recorded-base").await;
    let worker = wait_running(fx, "recorded-base").await;
    let output: String =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE idempotency_key=?1")
            .bind(&worker.task.id)
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&output).unwrap();
    let data = &value["data"];
    fx.db().writer().execute(sqlx::query(
        "UPDATE workspace_leases SET base_sha=?1,base_source='commit',canonical_path=?2,git_common_dir=?3 WHERE lease_id=?4",
    ).bind(data["base_sha"].as_str().unwrap())
        .bind(data["canonical_path"].as_str().unwrap())
        .bind(data["git_common_dir"].as_str().unwrap())
        .bind(&worker.lease_id)).await.unwrap();
    value["data"]
        .as_object_mut()
        .unwrap()
        .remove("git_common_dir");
    fx.db()
        .writer()
        .execute(
            sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE idempotency_key=?2")
                .bind(value.to_string())
                .bind(&worker.task.id),
        )
        .await
        .unwrap();
    let error = call_tool(
        &fx.boot,
        "neige_task_done",
        worker.identity.clone(),
        json!({"attempt_id":worker.task.id}),
    )
    .await
    .unwrap_err();
    assert!(
        error.message.contains("worker-read-only-unsampled"),
        "{error:?}"
    );
    assert_eq!(
        current(&fx.boot, "recorded-base").await.status,
        TaskStatus::Running
    );
    assert_eq!(lease_state(fx, &worker.lease_id).await, "held");
}

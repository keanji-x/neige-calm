//! F4 production authoring, scheduler, Operation, retained files and TaskLaunch.
use crate::isolated_codex_smoke::{Fixture, fixture};
use crate::mcp_track_report::{call_tool, planner_identity};
use crate::task_recovery::{current, declare};
use calm_server::{
    model::{Task, TaskStatus},
    operation::{OperationKey, OperationOutcome},
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

fn declaration(key: &str, delivery: Value) -> Value {
    let workspace = if delivery["role"] == "consumer" {
        "file-input"
    } else {
        "empty"
    };
    json!({"key":key,"kind":"codex","goal":"Process the declared JSON document and report using native MCP.",
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,"no_gate_reason":"JSON syntax policy only; no business gate.",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":workspace,"file_delivery":delivery}}})
}
fn producer() -> Value {
    declaration(
        "produce",
        json!({"role":"producer","slot":"result","path":"result.json","policy":"json-document-v1"}),
    )
}
fn consumer() -> Value {
    declaration(
        "consume",
        json!({"role":"consumer","producer":"produce","slot":"result","purpose":"json-input"}),
    )
}
async fn schedule(fx: &Fixture) {
    let scheduler = fx.state.dispatcher.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    tokio::time::timeout(
        Duration::from_secs(20),
        scheduler.schedule_track(fx.boot.track_id.clone()),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while fx
            .boot
            .repo
            .tasks_by_track(fx.boot.track_id.as_str())
            .await
            .unwrap()
            .iter()
            .any(|task| task.status == TaskStatus::Dispatched)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for task in fx
        .boot
        .repo
        .tasks_by_track(fx.boot.track_id.as_str())
        .await
        .unwrap()
    {
        if task.status == TaskStatus::Failed {
            if let Some(op) = fx
                .state
                .operation_runtime
                .find_by_kind_and_idempotency("codex-isolated-worker", &task.id)
                .await
                .unwrap()
            {
                tokio::time::timeout(
                    Duration::from_secs(20),
                    fx.state.operation_runtime.wait(&op.id),
                )
                .await
                .unwrap()
                .unwrap();
            }
        }
    }
}
async fn workspace(fx: &Fixture, task: &Task) -> PathBuf {
    let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
        .bind(&task.id).fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
    let output: Value = serde_json::from_str(&raw).unwrap();
    PathBuf::from(
        output["data"]["isolated_execution"]["request"]["workspace"]
            .as_str()
            .unwrap(),
    )
}
async fn settle(fx: &Fixture, task: &Task, success: bool) {
    std::fs::write(
        workspace(fx, task).await.join(if success {
            "report-success"
        } else {
            "report-failure"
        }),
        b"",
    )
    .unwrap();
    let op = fx
        .state
        .operation_runtime
        .find_by_kind_and_idempotency("codex-isolated-worker", &task.id)
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        fx.state.operation_runtime.wait(&op.id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        success,
        "{result:?}"
    );
}
async fn source(scenario: &str, bytes: &[u8]) -> (Fixture, Task, PathBuf) {
    let fx = fixture(scenario).await;
    declare(&fx.boot, producer()).await;
    schedule(&fx).await;
    let task = current(&fx.boot, "produce").await;
    assert_eq!(task.status, TaskStatus::Running);
    let path = workspace(&fx, &task).await;
    std::fs::write(path.join("result.json"), bytes).unwrap();
    settle(&fx, &task, true).await;
    (fx, task, path)
}
async fn publish(fx: &Fixture, task: &Task) -> String {
    let source = fx
        .state
        .operation_runtime
        .find_by_kind_and_idempotency("codex-isolated-worker", &task.id)
        .await
        .unwrap()
        .unwrap();
    let payload =
        json!({"task_id":task.id,"track_id":task.track_id,"source_operation_id":source.id});
    let id = fx
        .state
        .operation_runtime
        .submit(
            "task-file-publication",
            OperationKey {
                operation_key: calm_server::model::new_id(),
                idempotency_key: Some(format!("file:{}", task.id)),
                payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        fx.state.operation_runtime.wait(&id),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{result:?}"
    );
    id
}
async fn input_binding(fx: &Fixture, task: &Task) -> Value {
    let raw: String =
        sqlx::query_scalar("SELECT binding_json FROM task_file_input_bindings WHERE attempt_id=?1")
            .bind(&task.id)
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    serde_json::from_str(&raw).unwrap()
}
async fn listed(fx: &Fixture) -> Value {
    call_tool(
        &fx.boot,
        "calm.plan.list",
        planner_identity(&fx.boot),
        json!({}),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_sealed_bytes_survive_source_mutation_deletion_and_consumer_recovery() {
    let bytes = b"{\"answer\":42}\n";
    let (fx, producer, path) = source("controlled", bytes).await;
    let publication = publish(&fx, &producer).await;
    let card = producer.worker_card_id.as_deref().unwrap();
    assert_eq!(
        crate::isolated_codex_retry::rest(
            &fx,
            "DELETE",
            &format!("/api/cards/{card}"),
            Value::Null
        )
        .await
        .0,
        axum::http::StatusCode::NO_CONTENT
    );
    std::fs::write(path.join("result.json"), b"not the verified version").unwrap();
    std::fs::remove_dir_all(&path).unwrap();
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let first = current(&fx.boot, "consume").await;
    assert_eq!(first.status, TaskStatus::Running, "{:?}", listed(&fx).await);
    let first_path = workspace(&fx, &first).await;
    assert_eq!(
        std::fs::read(first_path.join("inputs/source/result.json")).unwrap(),
        bytes
    );
    let original = input_binding(&fx, &first).await;
    assert_eq!(original["receipt"]["publication_operation_id"], publication);
    // Once actually started, the Worker may write its input; observation must not reverify it.
    std::fs::write(
        first_path.join("inputs/source/result.json"),
        b"worker changed its own input",
    )
    .unwrap();
    settle(&fx, &first, false).await;
    call_tool(&fx.boot, "calm.plan.recover", planner_identity(&fx.boot),
        json!({"key":"consume","expected_attempt_id":first.id,"idempotency_key":"same-input","reason":"Retry under the same frozen JSON input."})).await.unwrap();
    schedule(&fx).await;
    let second = current(&fx.boot, "consume").await;
    assert_ne!(first.id, second.id);
    assert_eq!(
        second.status,
        TaskStatus::Running,
        "{:?}",
        listed(&fx).await
    );
    assert_eq!(input_binding(&fx, &second).await, original);
    assert_eq!(
        std::fs::read(
            workspace(&fx, &second)
                .await
                .join("inputs/source/result.json")
        )
        .unwrap(),
        bytes
    );
    settle(&fx, &second, true).await;
    let public = listed(&fx).await.to_string();
    assert!(
        !public.contains(fx.root.path().to_str().unwrap()),
        "private paths leaked: {public}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_scheduler_publishes_after_quiescence_then_starts_consumer() {
    let fx = fixture("controlled").await;
    declare(&fx.boot, producer()).await;
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let producer = current(&fx.boot, "produce").await;
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    std::fs::write(
        workspace(&fx, &producer).await.join("result.json"),
        b"[1,2,3]",
    )
    .unwrap();
    settle(&fx, &producer, true).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            schedule(&fx).await;
            if current(&fx.boot, "consume").await.status == TaskStatus::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let task = current(&fx.boot, "consume").await;
    assert_eq!(
        std::fs::read(
            workspace(&fx, &task)
                .await
                .join("inputs/source/result.json")
        )
        .unwrap(),
        b"[1,2,3]"
    );
    settle(&fx, &task, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_invalid_json_remains_failed_without_consumer_or_automatic_retry() {
    let (fx, source, _) = source("controlled", b"{broken").await;
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(op) = fx
                .state
                .operation_runtime
                .find_by_kind_and_idempotency(
                    "task-file-publication",
                    &format!("file:{}", source.id),
                )
                .await
                .unwrap()
            {
                let result = fx.state.operation_runtime.wait(&op.id).await.unwrap();
                assert!(
                    matches!(result.outcome, OperationOutcome::Failed { .. }),
                    "{result:?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        schedule(&fx).await;
    }
    let consumer = current(&fx.boot, "consume").await;
    assert_eq!(consumer.status, TaskStatus::Pending);
    assert!(
        fx.state
            .operation_runtime
            .find_by_kind_and_idempotency("codex-isolated-worker", &consumer.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        listed(&fx)
            .await
            .to_string()
            .contains("json-document-v1 verification failed")
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind='task-file-publication'")
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_corrupt_sealed_storage_refuses_consumer_start() {
    let (fx, source, _) = source("controlled", b"42").await;
    let publication = publish(&fx, &source).await;
    let raw: String =
        sqlx::query_scalar("SELECT receipt_json FROM task_file_publications WHERE operation_id=?1")
            .bind(publication)
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    let receipt: Value = serde_json::from_str(&raw).unwrap();
    let root = PathBuf::from(receipt["store_root"].as_str().unwrap());
    let object = root
        .join("snapshots")
        .join(receipt["snapshot"].as_str().unwrap())
        .join("objects")
        .join(receipt["file_digest"].as_str().unwrap());
    std::fs::write(object, b"99").unwrap();
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let consumer = current(&fx.boot, "consume").await;
    assert_eq!(
        consumer.status,
        TaskStatus::Failed,
        "{:?}",
        listed(&fx).await
    );
    let path = workspace(&fx, &consumer).await;
    assert!(
        !path.join("result.txt").exists(),
        "fake Worker actually started"
    );
    assert!(
        listed(&fx).await.to_string().contains("integrity"),
        "{:?}",
        listed(&fx).await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_task_launch_rechecks_prepared_bytes_before_first_turn() {
    let (fx, source, _) = source("delivery-corrupt", b"42").await;
    publish(&fx, &source).await;
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let consumer = current(&fx.boot, "consume").await;
    assert_eq!(
        consumer.status,
        TaskStatus::Failed,
        "{:?}",
        listed(&fx).await
    );
    assert!(!workspace(&fx, &consumer).await.join("result.txt").exists());
    let op = fx
        .state
        .operation_runtime
        .find_by_kind_and_idempotency("codex-isolated-worker", &consumer.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        op.last_error.as_deref().unwrap().contains("integrity"),
        "{:?}",
        op.last_error
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_publication_refuses_withdrawn_ready_without_weakening_start_guard() {
    let fx = fixture("controlled").await;
    fx.state.dispatcher.abort_event_listener_for_test();
    let (block, rev) = declare(&fx.boot, producer()).await;
    schedule(&fx).await;
    let source = current(&fx.boot, "produce").await;
    std::fs::write(workspace(&fx, &source).await.join("result.json"), b"42").unwrap();
    settle(&fx, &source, true).await;
    let mut withdrawn = producer();
    withdrawn["ready"] = json!(false);
    call_tool(
        &fx.boot,
        "calm.report.blocks.upsert",
        planner_identity(&fx.boot),
        json!({"id":block,"kind":"task","payload":withdrawn,"if_rev":rev}),
    )
    .await
    .unwrap();
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(op) = fx
                .state
                .operation_runtime
                .find_by_kind_and_idempotency(
                    "task-file-publication",
                    &format!("file:{}", source.id),
                )
                .await
                .unwrap()
            {
                let result = fx.state.operation_runtime.wait(&op.id).await.unwrap();
                assert!(
                    matches!(result.outcome, OperationOutcome::Failed { .. }),
                    "{result:?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let task = current(&fx.boot, "consume").await;
    assert_eq!(task.status, TaskStatus::Pending);
    assert!(
        fx.state
            .operation_runtime
            .find_by_kind_and_idempotency("codex-isolated-worker", &task.id)
            .await
            .unwrap()
            .is_none()
    );
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    assert!(
        calm_server::operation::refuse_if_context_stale(&mut tx, Some(&source.id))
            .await
            .is_err(),
        "Done producer must remain forbidden for startup"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_claim_restart_retains_exact_input_and_rejects_missing_binding() {
    for missing in [false, true] {
        let (fx, source, path) = source("controlled", b"42").await;
        publish(&fx, &source).await;
        let scheduler = fx.state.dispatcher.scheduler();
        let claimed = std::sync::Arc::new(tokio::sync::Notify::new());
        let resume = std::sync::Arc::new(tokio::sync::Notify::new());
        scheduler.set_post_claim_drive_test_hook(calm_server::scheduler::PostClaimDriveTestHook {
            claimed: claimed.clone(),
            resume: resume.clone(),
        });
        declare(&fx.boot, consumer()).await;
        let running = tokio::spawn({
            let scheduler = scheduler.clone();
            let track = fx.boot.track_id.clone();
            async move { scheduler.schedule_track(track).await }
        });
        tokio::time::timeout(Duration::from_secs(10), claimed.notified())
            .await
            .unwrap();
        let task = current(&fx.boot, "consume").await;
        assert_eq!(task.status, TaskStatus::Dispatched);
        let bound = input_binding(&fx, &task).await;
        assert!(!bound["receipt"]["snapshot"].as_str().unwrap().is_empty());
        std::fs::remove_dir_all(path).unwrap();
        if missing {
            sqlx::query("DELETE FROM task_file_input_bindings WHERE attempt_id=?1")
                .bind(&task.id)
                .execute(&fx.boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
        }
        // Fresh scheduler resumes the already committed production claim, not a new selection.
        let restarted = calm_server::scheduler::Scheduler::new(
            fx.boot.repo.clone(),
            fx.boot.ctx.events.clone(),
            fx.boot.ctx.write.clone(),
            std::sync::Arc::downgrade(&fx.state.operation_runtime),
            std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
        );
        restarted.mark_boot_sweep_complete();
        restarted.mark_context_sweep_boot_complete();
        tokio::time::timeout(Duration::from_secs(20), restarted.sweep_all())
            .await
            .unwrap();
        let resumed = current(&fx.boot, "consume").await;
        if missing {
            assert_eq!(resumed.status, TaskStatus::Failed);
            assert!(resumed.worker_card_id.is_none());
        } else {
            assert_eq!(
                resumed.status,
                TaskStatus::Running,
                "{:?}",
                listed(&fx).await
            );
            assert_eq!(input_binding(&fx, &resumed).await, bound);
            assert_eq!(
                std::fs::read(
                    workspace(&fx, &resumed)
                        .await
                        .join("inputs/source/result.json")
                )
                .unwrap(),
                b"42"
            );
            settle(&fx, &resumed, true).await;
        }
        resume.notify_one();
        tokio::time::timeout(Duration::from_secs(10), running)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_source_only_failure_wakes_planner_and_replay_deduplicates() {
    for replay in [false, true] {
        let fx = fixture("controlled").await;
        if replay {
            fx.state.dispatcher.abort_event_listener_for_test();
        }
        let planner = crate::isolated_codex_retry::recovery_wake::planner(&fx).await;
        declare(&fx.boot, producer()).await;
        schedule(&fx).await;
        let source = current(&fx.boot, "produce").await;
        std::fs::write(
            workspace(&fx, &source).await.join("result.json"),
            b"not JSON",
        )
        .unwrap();
        settle(&fx, &source, true).await;
        let settled = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                schedule(&fx).await;
                let events = fx
                    .boot
                    .repo
                    .events_for_track(
                        fx.boot.track_id.as_str(),
                        &["task.file_publication_settled"],
                        None,
                    )
                    .await
                    .unwrap();
                if let Some(event) = events.into_iter().next() {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        if replay {
            fx.state
                .dispatcher
                .catch_up_push(fx.boot.track_id.clone(), settled.event.clone(), settled.id)
                .await;
        }
        tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if planner.snapshot().await.pending_observations().iter().any(|observation|
                matches!(observation, calm_server::harness::Observation::SystemContext { text } if text.contains("file publication") && text.contains("failed") && text.contains(&source.id))) { break }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
        for _ in 0..2 {
            fx.state
                .dispatcher
                .catch_up_push(fx.boot.track_id.clone(), settled.event.clone(), settled.id)
                .await;
            schedule(&fx).await;
        }
        let snapshot = planner.snapshot().await;
        assert_eq!(snapshot.pending_observations().iter().filter(|observation|
        matches!(observation, calm_server::harness::Observation::SystemContext { text } if text.contains("file publication") && text.contains(&source.id))).count(), 1);
        assert_eq!(
            fx.boot
                .repo
                .events_for_track(
                    fx.boot.track_id.as_str(),
                    &["task.file_publication_settled"],
                    None
                )
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_delivery_consumer_io_does_not_hold_track_scheduling_lock() {
    use nix::fcntl::{Flock, FlockArg};
    let (fx, source, _) = source("controlled", b"42").await;
    let publication = publish(&fx, &source).await;
    let raw: String =
        sqlx::query_scalar("SELECT receipt_json FROM task_file_publications WHERE operation_id=?1")
            .bind(publication)
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    let receipt: Value = serde_json::from_str(&raw).unwrap();
    let root = PathBuf::from(receipt["store_root"].as_str().unwrap());
    let lock = Flock::lock(
        std::fs::File::open(root.join(".lock")).unwrap(),
        FlockArg::LockExclusiveNonblock,
    )
    .unwrap();
    declare(&fx.boot, consumer()).await;
    let scheduler = fx.state.dispatcher.scheduler();
    let unlocked = tokio::time::timeout(
        Duration::from_secs(3),
        scheduler.schedule_track(fx.boot.track_id.clone()),
    )
    .await;
    let state = current(&fx.boot, "consume").await.status;
    drop(lock); // Always release the blocking IO boundary before asserting.
    assert!(
        unlocked.is_ok(),
        "consumer materialization held the Track scheduling lock"
    );
    assert_eq!(state, TaskStatus::Dispatched);
    schedule(&fx).await;
    let consumer = current(&fx.boot, "consume").await;
    assert_eq!(consumer.status, TaskStatus::Running);
    settle(&fx, &consumer, true).await;
}

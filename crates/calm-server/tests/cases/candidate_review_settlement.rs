//! Reviewer report completion precedes exact Operation settlement; both facts must wake.
use super::*;
use crate::isolated_codex_retry::recovery_wake::{planner, restore_planner};
use calm_server::{
    harness::{Observation, PlannerHarness},
    operation::{OperationRepo, SqlxOperationRepo},
};
fn notices(snapshot: &calm_server::harness::HarnessSnapshot, id: &str) -> usize {
    snapshot.pending_observations().iter().filter(|o| matches!(o, Observation::SystemContext { text } if text.contains(id) && text.contains("settled") && text.contains("calm.plan.list"))).count()
}
async fn observed(handle: &PlannerHarness, id: &str, settled: bool) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = handle.snapshot().await;
            if if settled { notices(&snapshot,id)>0 } else { snapshot.pending_observations().iter().any(|o| matches!(o,Observation::TaskCompleted { idempotency_key, .. } if idempotency_key==id)) } { return; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.is_ok()
}
async fn settlement_wake(live: bool) {
    let (fx, producer, _, _) = review_source_listener("controlled", CHECK, true).await;
    let handle = planner(&fx).await;
    let review = current(&fx.boot, "review").await;
    let op = fx
        .state
        .operation_runtime
        .find_by_kind_and_idempotency("codex-isolated-worker", &review.id)
        .await
        .unwrap()
        .unwrap();
    let ops = SqlxOperationRepo::new(fx.boot.repo.sqlite_pool().unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if ops.claim_parked(&op.id).await.unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let path = workspace(&fx, &review).await;
    std::fs::write(
        path.join("report-result.json"),
        json!({"passed":true,"blocking_findings":[]}).to_string(),
    )
    .unwrap();
    std::fs::write(path.join("report-success"), b"").unwrap();
    let early = observed(&handle, &review.id, false).await;
    let early_verdict = verdict(&fx, &producer, "accepted").await;
    let before = notices(&handle.snapshot().await, &review.id);
    if !live {
        fx.state.dispatcher.abort_event_listener_for_test();
        handle.persist_snapshot().await.unwrap();
        handle.shutdown().await.unwrap();
        fx.state
            .harness
            .remove(&planner_identity(&fx.boot).session_id);
    }
    ops.clear_parked_lease_for_boot(&op.id).await.unwrap();
    fx.state.operation_runtime.wait(&op.id).await.unwrap();
    let events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    // Always stop the real provider before the red assertion.
    if events.len() != 1 && live {
        handle.shutdown().await.unwrap();
    }
    assert!(early, "native report must wake before settlement");
    assert!(
        early_verdict.is_err(),
        "early acceptance must retain settlement fence"
    );
    assert_eq!(before, 0);
    assert_eq!(
        events.len(),
        1,
        "successful Reviewer settlement must leave a durable second wake; ordinary producer Done stays quiet"
    );
    assert!(
        matches!(&events[0].event,calm_server::event::Event::TaskExecutionSettled { task_id, operation_id } if task_id==&review.id && operation_id==&op.id)
    );
    let handle = if live {
        handle
    } else {
        restore_planner(&fx).await
    };
    let woke = observed(&handle, &review.id, true).await;
    for _ in 0..2 {
        fx.state
            .dispatcher
            .catch_up_push(
                fx.boot.track_id.clone(),
                events[0].event.clone(),
                events[0].id,
            )
            .await;
    }
    let count = notices(&handle.snapshot().await, &review.id);
    handle.persist_snapshot().await.unwrap();
    handle.shutdown().await.unwrap();
    fx.state.dispatcher.abort_event_listener_for_test();
    let boot = restore_planner(&fx).await;
    let boot_count = notices(&boot.snapshot().await, &review.id);
    boot.shutdown().await.unwrap();
    let recovery = fx.state.operation_runtime.recover_on_boot().await.unwrap();
    fx.state
        .operation_runtime
        .apply_recovery(recovery)
        .await
        .unwrap();
    let after = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    assert!(
        woke,
        "settled review must become observable without another business event"
    );
    assert_eq!(count, 1);
    assert_eq!(boot_count, 1);
    assert_eq!(after.len(), 1);
    verdict(&fx, &producer, "accepted").await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_settlement_wakes_live_planner_after_early_verdict() {
    settlement_wake(true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_settlement_replays_boot_once_after_early_verdict() {
    settlement_wake(false).await;
}

/// Real compensation after an authenticated Done report, under the held parked lease.
async fn review_terminal(
    failed: bool,
) -> (Fixture, Task, Task, calm_server::operation::OperationId) {
    use calm_server::operation::{PhaseTag, ProviderAdapter};
    let (fx, producer, _, _) = review_source().await;
    let review = current(&fx.boot, "review").await;
    let op = fx
        .state
        .operation_runtime
        .find_by_kind_and_idempotency("codex-isolated-worker", &review.id)
        .await
        .unwrap()
        .unwrap();
    let ops = SqlxOperationRepo::new(fx.boot.repo.sqlite_pool().unwrap());
    let owned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(op) = ops.claim_parked(&op.id).await.unwrap() {
                break op;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let path = workspace(&fx, &review).await;
    std::fs::write(
        path.join("report-result.json"),
        json!({"passed":true,"blocking_findings":[]}).to_string(),
    )
    .unwrap();
    std::fs::write(path.join("report-success"), b"").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while current(&fx.boot, "review").await.status != TaskStatus::Done {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if failed {
        let adapter = calm_server::isolated_codex::adapter::IsolatedCodexAdapter::new(
            Some(fx.backend.clone()),
            fx.boot.repo.clone(),
            None,
            fx.boot.ctx.write.clone(),
        );
        let fresh = ops.get_operation(&op.id).await.unwrap().unwrap();
        let output = fresh.tx_output.as_ref().unwrap();
        let state = adapter
            .plan_compensation(
                PhaseTag::Parked,
                "cancel review after report",
                output,
                &owned,
            )
            .await
            .unwrap();
        ops.set_compensating(&owned, &state, output)
            .await
            .unwrap()
            .expect("owned compensation CAS");
        fx.state.operation_runtime.drive().await.unwrap();
    } else {
        ops.clear_parked_lease_for_boot(&op.id).await.unwrap();
    }
    fx.state.operation_runtime.wait(&op.id).await.unwrap();
    fx.state.dispatcher.scheduler().sweep_all().await;
    (fx, producer, review, op.id)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_settlement_after_done_operation_failure_is_durable_and_not_recoverable() {
    let (fx, producer, review, id) = review_terminal(true).await;
    assert_eq!(current(&fx.boot, "review").await.status, TaskStatus::Done);
    let error = verdict(&fx, &producer, "accepted").await.unwrap_err();
    assert!(error.message.contains("Operation failed"), "{error:?}");
    let view = listed(&fx).await;
    let row = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "review")
        .unwrap();
    assert_eq!(
        row["recovery"]["allowed"], false,
        "Done is not retry authority"
    );
    assert_eq!(
        row["file_delivery"]["review"]["passed"], true,
        "historical report is distinct"
    );
    assert_eq!(
        row["file_delivery"]["review"]["operation"]["state"],
        "failed"
    );
    let events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    assert_eq!(
        events.len(),
        1,
        "compensated Done review must still wake after confirmed stop"
    );
    assert!(
        matches!(&events[0].event,calm_server::event::Event::TaskExecutionSettled { task_id, operation_id } if task_id==&review.id && operation_id==&id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_settlement_actual_briefing_never_binds_recover() {
    use crate::isolated_codex_retry::recovery_wake::planner_with_daemon;
    use calm_server::{codex_appserver::InputItem, shared_codex_appserver::SharedCodexAppServer};
    for state in ["succeeded", "failed", "withdrawn"] {
        let (fx, _, review, _) = review_terminal(state == "failed").await;
        let daemon =
            SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
        let handle = planner_with_daemon(&fx, daemon.clone()).await;
        calm_server::semantic_recovery::test_support::register_thread(
            fx.boot.repo.as_ref(),
            fx.boot.planner_card_id.as_str(),
            "planner-observer",
        )
        .await
        .unwrap();
        let events = fx
            .boot
            .repo
            .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
            .await
            .unwrap();
        let event = events.last().expect("review settlement persisted");
        fx.state
            .dispatcher
            .catch_up_push(fx.boot.track_id.clone(), event.event.clone(), event.id)
            .await;
        assert!(observed(&handle, &review.id, true).await);
        if state == "withdrawn" {
            sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
                .bind(&review.id)
                .execute(&fx.boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
        }
        handle
            .force_phase_for_dev(calm_server::harness::HarnessPhaseTag::Idle)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while daemon.turn_start_count_for_test() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let text = daemon.started_turns_for_test()[0]
            .1
            .iter()
            .filter_map(|item| match item {
                InputItem::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        handle.shutdown().await.unwrap();
        let body = text
            .rsplit_once("Candidate review execution settled (kernel snapshot):\n")
            .unwrap()
            .1
            .split_once("\nEnd candidate review settlement.")
            .unwrap()
            .0;
        let brief: Value = serde_json::from_str(body).unwrap();
        assert_eq!(brief["review_attempt_id"], review.id);
        assert_eq!(
            brief["operation_state"],
            if state == "failed" {
                "failed"
            } else {
                "succeeded"
            }
        );
        assert_eq!(brief["current_authority"], state != "withdrawn");
        assert!(
            brief.get("failure").is_none() && brief.get("planner_recovery").is_none(),
            "{brief}"
        );
        assert!(!text.contains("Recovery decision briefing"), "{text}");
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_recovery_issuances")
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        assert_eq!(
            count, 0,
            "Done Reviewer must not bind Recover, including a failed Operation"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_settlement_withdrawn_and_foreign_notices_stay_quiet() {
    let (fx, _, review, _) = review_terminal(false).await;
    let handle = planner(&fx).await;
    let events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    let event = &events[0];
    let forged = calm_server::event::Event::TaskExecutionSettled {
        task_id: review.id.clone(),
        operation_id: "foreign-operation".into(),
    };
    fx.state
        .dispatcher
        .catch_up_push(fx.boot.track_id.clone(), forged, event.id)
        .await;
    assert_eq!(notices(&handle.snapshot().await, &review.id), 0);
    sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
        .bind(&review.id)
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    fx.state
        .dispatcher
        .catch_up_push(fx.boot.track_id.clone(), event.event.clone(), event.id)
        .await;
    assert_eq!(notices(&handle.snapshot().await, &review.id), 0);
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_decision_track_delete_preserves_cascade_semantics() {
    let (fx, producer, _, _) = review_terminal(false).await;
    verdict(&fx, &producer, "accepted").await.unwrap();
    schedule(&fx).await;
    let consumer = current(&fx.boot, "consume").await;
    assert_eq!(consumer.status, TaskStatus::Running);
    settle(&fx, &consumer, true).await;
    let (status, body) =
        super::super::review::delete_http(&fx, &format!("/api/tracks/{}", fx.boot.track_id)).await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
    for table in [
        "task_candidate_decisions",
        "task_candidate_decision_bindings",
        "task_candidate_input_bindings",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

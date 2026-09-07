//! Native failure must remain visible, then settled execution must wake Planner again.
use super::*;
use crate::mcp_track_report::{call_tool, planner_identity};
use calm_server::harness::{
    HarnessConfig, HarnessSnapshot, Observation, PlannerHarness, PlannerHarnessParams,
};
use calm_server::operation::{OperationRepo, SqlxOperationRepo};

async fn planner(fx: &Fixture) -> PlannerHarness {
    let worker_session_id = planner_identity(&fx.boot).session_id;
    let areas = calm_server::track_area_cache::TrackAreaCache::new();
    fx.boot.repo.seed_track_area_cache(&areas).await.unwrap();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = calm_server::harness::HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("planner-observer".into());
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let mut tx = pool.begin().await.unwrap();
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    calm_server::db::sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: worker_session_id.clone(),
            card_id: fx.boot.planner_card_id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("planner-observer".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let handle = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: worker_session_id.clone(),
        track_id: fx.boot.track_id.clone(),
        card_id: fx.boot.planner_card_id.clone(),
        thread_id: Some("planner-observer".into()),
        repo: fx.boot.repo.clone(),
        events: fx.boot.ctx.events.clone(),
        card_role_cache: fx.boot.card_role_cache.clone(),
        track_area_cache: areas,
        // Keep the real harness queue live without issuing any model request.
        daemon: calm_server::shared_codex_appserver::SharedCodexAppServer::new_stub(
            fx.boot.repo.clone(),
        ),
        config: HarnessConfig::default(),
        snapshot,
    });
    handle
        .force_phase_for_dev(calm_server::harness::HarnessPhaseTag::TurnRunning)
        .await
        .unwrap();
    fx.state.harness.insert(worker_session_id, handle.clone());
    handle
}

async fn planner_recovery(fx: &Fixture) -> Value {
    let list = call_tool(
        &fx.boot,
        "calm.plan.list",
        planner_identity(&fx.boot),
        json!({}),
    )
    .await
    .unwrap();
    list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["key"] == "retry")
        .unwrap()["recovery"]
        .clone()
}

async fn wait_observation(handle: &PlannerHarness, task_id: &str, settled: bool) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observations = handle.snapshot().await.pending_observations();
            if observations.iter().any(|observation| match observation {
                Observation::TaskFailed {
                    idempotency_key, ..
                } => !settled && idempotency_key == task_id,
                Observation::SystemContext { text } => {
                    settled && text.contains(task_id) && text.contains("calm.plan.list")
                }
                _ => false,
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_observes_failure_then_settled_isolated_recovery() {
    let fx = fixture("controlled").await;
    let handle = planner(&fx).await;
    crate::task_recovery::declare(&fx.boot, json!({
        "key":"retry", "kind":"codex", "goal":"Write result.txt containing 42 and report completion.",
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "ready":true,
        "no_gate_reason":"Report-driven isolated fixture.",
        "context":{"neige_execution":{"version":"isolated-codex-v1", "workspace":"empty"}}
    })).await;
    let (first, workspace) = launch(&fx).await;
    let (op_id, _, _) = operation(&fx, &first).await;
    let ops = SqlxOperationRepo::new(fx.boot.repo.sqlite_pool().unwrap());
    // The real parked lease fence holds cleanup while the native MCP report lands.
    let owned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(op) = ops.claim_parked(&op_id).await.unwrap() {
                break op;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("hold cleanup lease");
    std::fs::write(workspace.join("report-failure"), b"").unwrap();
    let early = wait_observation(&handle, &first.id, false).await;
    assert_eq!(operation(&fx, &first).await.1, "parked");
    assert_eq!(planner_recovery(&fx).await["allowed"], false);
    assert!(!handle.snapshot().await.pending_observations().iter().any(|observation|
        matches!(observation, Observation::SystemContext { text } if text.contains(&first.id) && text.contains("calm.plan.list"))));
    assert_eq!(owned.id, op_id);
    ops.clear_parked_lease_for_boot(&op_id).await.unwrap();
    finish(&fx, &first, &workspace, false).await;
    assert_eq!(planner_recovery(&fx).await["allowed"], true);
    // Make the red assertion only after the owned fake process has been stopped.
    let settled = wait_observation(&handle, &first.id, true).await;
    assert!(early, "Planner must observe failure before cleanup");
    assert!(
        settled,
        "Planner must wake after the failed Operation settles"
    );
    let events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0].event, calm_server::event::Event::TaskExecutionSettled { task_id, operation_id } if task_id == &first.id && operation_id == &op_id)
    );
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
    assert_eq!(
        handle
            .snapshot()
            .await
            .pending_observations()
            .iter()
            .filter(
                |o| matches!(o, Observation::SystemContext { text } if text.contains(&first.id))
            )
            .count(),
        1
    );
    let recovered_ops = fx.state.operation_runtime.recover_on_boot().await.unwrap();
    fx.state
        .operation_runtime
        .apply_recovery(recovered_ops)
        .await
        .unwrap();
    assert_eq!(
        fx.boot
            .repo
            .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
            .await
            .unwrap()
            .len(),
        1,
        "repeated Operation recovery must not duplicate settlement"
    );
    let mut request = recovery(&first);
    request["key"] = json!("retry");
    let receipt = call_tool(
        &fx.boot,
        "calm.plan.recover",
        planner_identity(&fx.boot),
        request.clone(),
    )
    .await
    .unwrap();
    let replay = call_tool(
        &fx.boot,
        "calm.plan.recover",
        planner_identity(&fx.boot),
        request,
    )
    .await
    .unwrap();
    assert_eq!(
        receipt, replay,
        "request replay must not allocate another attempt"
    );
    let (second, next_workspace) = launch(&fx).await;
    assert_eq!(first.key, second.key);
    assert_eq!(first.goal, second.goal);
    assert_ne!(first.id, second.id);
    assert_ne!(workspace, next_workspace);
    finish(&fx, &second, &next_workspace, true).await;
    assert_eq!(
        current(&fx.boot, "retry").await.status,
        calm_server::model::TaskStatus::Done
    );
    let history = fx
        .boot
        .repo
        .task_history_by_key(fx.boot.track_id.as_str(), "retry")
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .any(|t| t.id == first.id && t.status == calm_server::model::TaskStatus::Failed)
    );
    let settled_events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    assert_eq!(
        settled_events.len(),
        1,
        "success does not emit a recovery wake"
    );
    handle.shutdown().await.unwrap();
}

async fn restore_planner(fx: &Fixture) -> PlannerHarness {
    use calm_server::harness::{
        ClaimMode, RecoveryOutcome, new_track_delete_locks, spawn_recovered_harness,
    };
    let id = planner_identity(&fx.boot).session_id;
    let runtime = fx
        .boot
        .repo
        .session_projection_by_id(&id)
        .await
        .unwrap()
        .unwrap();
    let areas = calm_server::track_area_cache::TrackAreaCache::new();
    fx.boot.repo.seed_track_area_cache(&areas).await.unwrap();
    let outcome = spawn_recovered_harness(
        fx.boot.repo.clone(),
        fx.boot.ctx.events.clone(),
        fx.boot.card_role_cache.clone(),
        areas,
        calm_server::shared_codex_appserver::SharedCodexAppServer::new_stub(fx.boot.repo.clone()),
        &fx.state.harness,
        &new_track_delete_locks(),
        runtime,
        ClaimMode::Replace,
    )
    .await
    .unwrap();
    let RecoveryOutcome::Installed(handle) = outcome else {
        panic!("Planner must recover")
    };
    handle
        .force_phase_for_dev(calm_server::harness::HarnessPhaseTag::TurnRunning)
        .await
        .unwrap();
    handle
}

fn hints(snapshot: &HarnessSnapshot, task_id: &str) -> usize {
    snapshot.pending_observations().iter().filter(|observation|
        matches!(observation, Observation::SystemContext { text } if text.contains(task_id) && text.contains("calm.plan.list"))).count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_replays_settled_isolated_recovery_once_with_user_required() {
    let fx = fixture("controlled").await;
    let handle = planner(&fx).await;
    start_task(&fx).await; // User-owned: keep its User-required recovery path visible.
    let (first, workspace) = launch(&fx).await;
    // Simulate losing the live publication while keeping the actual completion writer.
    fx.state.dispatcher.abort_event_listener_for_test();
    handle.shutdown().await.unwrap();
    fx.state
        .harness
        .remove(&planner_identity(&fx.boot).session_id);
    finish(&fx, &first, &workspace, false).await;
    assert_eq!(planner_recovery(&fx).await["allowed"], false);
    assert_eq!(
        rest(&fx, "GET", &route(&fx, "attempts"), Value::Null)
            .await
            .1["recovery"]["allowed"],
        true
    );
    let recovered = restore_planner(&fx).await;
    let snapshot = recovered.snapshot().await;
    assert!(snapshot.pending_observations().iter().any(|o| matches!(o, Observation::TaskFailed { idempotency_key, .. } if idempotency_key == &first.id)));
    assert_eq!(
        hints(&snapshot, &first.id),
        1,
        "boot must replay the settlement that was not published live"
    );
    recovered.persist_snapshot().await.unwrap();
    recovered.shutdown().await.unwrap();
    let repeated = restore_planner(&fx).await;
    assert_eq!(
        hints(&repeated.snapshot().await, &first.id),
        1,
        "persisted watermark prevents duplicate boot wake"
    );
    repeated.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_settled_hint_refuses_unproven_foreign_and_superseded_execution() {
    let (fx, first, original) = stopped_failure().await;
    fx.state.dispatcher.abort_event_listener_for_test();
    let handle = planner(&fx).await;
    let events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    let envelope = &events[0];
    let mut corrupt = original.clone();
    corrupt["data"]["isolated_execution"]["provider"]["record"]["stop"] = json!("Requested");
    set_output(&fx, &first, &corrupt).await;
    fx.state
        .dispatcher
        .catch_up_push(
            fx.boot.track_id.clone(),
            envelope.event.clone(),
            envelope.id,
        )
        .await;
    assert_eq!(
        hints(&handle.snapshot().await, &first.id),
        0,
        "unproven stop cannot wake Planner as settled"
    );
    set_output(&fx, &first, &original).await;
    let foreign = calm_server::event::Event::TaskExecutionSettled {
        task_id: first.id.clone(),
        operation_id: "foreign-operation".into(),
    };
    fx.state
        .dispatcher
        .catch_up_push(fx.boot.track_id.clone(), foreign, envelope.id)
        .await;
    assert_eq!(hints(&handle.snapshot().await, &first.id), 0);
    // The User can recover this User-owned task; the old notification then becomes stale.
    fx.state.dispatcher.semaphore().close();
    let (status, receipt) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    fx.state
        .dispatcher
        .catch_up_push(
            fx.boot.track_id.clone(),
            envelope.event.clone(),
            envelope.id,
        )
        .await;
    assert_eq!(
        hints(&handle.snapshot().await, &first.id),
        0,
        "superseded attempt cannot advertise recovery"
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settled_completion_ignores_canceled_missing_and_noncurrent_tasks() {
    use calm_server::operation::ProviderAdapter;
    let (fx, first, _) = stopped_failure().await;
    fx.state.dispatcher.abort_event_listener_for_test();
    fx.state.dispatcher.semaphore().close();
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let (op_id, _, _) = operation(&fx, &first).await;
    let ops = SqlxOperationRepo::new(pool.clone());
    let op = ops.get_operation(&op_id).await.unwrap().unwrap();
    let adapter = calm_server::isolated_codex::adapter::IsolatedCodexAdapter::new(
        Some(fx.backend.clone()),
        fx.boot.repo.clone(),
        Some(fx.root.path().join("mcp.sock")),
        fx.boot.ctx.write.clone(),
    );
    // Isolate the completion hook's irrelevant-outcome branches; each transaction
    // rolls back the fixture state and calls the actual adapter hook.
    for statement in [
        "UPDATE tasks SET status='canceled' WHERE id=?1",
        "DELETE FROM tasks WHERE id=?1",
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(statement)
            .bind(&first.id)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(
            adapter
                .complete_owned_parked_tx(&mut tx, &op)
                .await
                .unwrap()
                .is_empty()
        );
        tx.rollback().await.unwrap();
    }
    let (status, receipt) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let mut tx = pool.begin().await.unwrap();
    assert!(
        adapter
            .complete_owned_parked_tx(&mut tx, &op)
            .await
            .unwrap()
            .is_empty()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settled_event_failure_rolls_back_completion_and_reconciles_once() {
    let fx = fixture("controlled").await;
    start_task(&fx).await;
    let (first, workspace) = launch(&fx).await;
    let (op_id, _, _) = operation(&fx, &first).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    // Failure at the actual append boundary must roll back the Operation CAS too.
    sqlx::query("CREATE TRIGGER reject_settlement BEFORE INSERT ON events WHEN NEW.kind='task.execution_settled' BEGIN SELECT RAISE(ABORT, 'settlement fixture fault'); END")
        .execute(&pool).await.unwrap();
    std::fs::write(workspace.join("report-failure"), b"").unwrap();
    let phase = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (_, phase, output) = operation(&fx, &first).await;
            let quiesced =
                output["data"]["isolated_execution"]["provider"]["record"]["stop"]["Quiesced"]
                    .is_object();
            let released: bool =
                sqlx::query_scalar("SELECT lease_owner IS NULL FROM operations WHERE id=?1")
                    .bind(&op_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if quiesced && released {
                break phase;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='task.execution_settled'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("DROP TRIGGER reject_settlement")
        .execute(&pool)
        .await
        .unwrap();
    // Release the injected fault even if the assertions below detect a regression.
    finish(&fx, &first, &workspace, false).await;
    assert_eq!(
        phase, "parked",
        "append failure must not leave a failed Operation without a durable wake"
    );
    assert_eq!(count, 0);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='task.execution_settled'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "owned reconciliation must finish the original Operation once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_settled_hint_ignores_withdrawn_declaration() {
    let (fx, first, _) = stopped_failure().await;
    fx.state.dispatcher.abort_event_listener_for_test();
    let handle = planner(&fx).await;
    let report = fx
        .boot
        .repo
        .card_get(fx.boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let block = report.payload["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["payload"]["key"] == "retry")
        .unwrap();
    let mut payload = block["payload"].clone();
    payload["ready"] = json!(false);
    let (status, body) = rest(
        &fx,
        "PATCH",
        &format!(
            "/api/tracks/{}/report/blocks/{}",
            fx.boot.track_id,
            block["id"].as_str().unwrap()
        ),
        json!({"kind":"task", "payload":payload, "ifBlockRev":block["rev"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    fx.state
        .dispatcher
        .catch_up_push(
            fx.boot.track_id.clone(),
            events[0].event.clone(),
            events[0].id,
        )
        .await;
    assert_eq!(hints(&handle.snapshot().await, &first.id), 0);
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settlement_overtaking_failure_preserves_live_and_persisted_observations() {
    use calm_server::dispatcher::TaskFailurePushTestHook;
    use std::sync::Arc;
    use tokio::sync::Notify;
    let fx = fixture("controlled").await;
    let handle = planner(&fx).await;
    start_task(&fx).await;
    let (first, workspace) = launch(&fx).await;
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    fx.state
        .dispatcher
        .set_task_failure_push_hook_for_test(TaskFailurePushTestHook {
            task_id: first.id.clone(),
            entered: entered.clone(),
            resume: resume.clone(),
            finished: finished.clone(),
        });
    std::fs::write(workspace.join("report-failure"), b"").unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    // The actual isolated observer may finish while the native failure handler waits.
    finish(&fx, &first, &workspace, false).await;
    let saw_hint = wait_observation(&handle, &first.id, true).await;
    let overtaken = handle.snapshot().await;
    resume.notify_one();
    tokio::time::timeout(Duration::from_secs(5), finished.notified())
        .await
        .unwrap();
    handle.persist_snapshot().await.unwrap();
    let before_boot = handle.snapshot().await;
    fx.state.dispatcher.abort_event_listener_for_test();
    handle.shutdown().await.unwrap();
    let recovered = restore_planner(&fx).await;
    let after_boot = recovered.snapshot().await;
    recovered.shutdown().await.unwrap();
    assert!(
        saw_hint,
        "settlement must pass the suspended failure handler"
    );
    let pair = |snapshot: &HarnessSnapshot| {
        snapshot
            .pending_observations()
            .iter()
            .filter_map(|observation| match observation {
                Observation::TaskFailed {
                    idempotency_key, ..
                } if idempotency_key == &first.id => Some("failure"),
                Observation::SystemContext { text }
                    if text.contains(&first.id) && text.contains("calm.plan.list") =>
                {
                    Some("settled")
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let persisted = fx
        .boot
        .repo
        .events_for_track(
            fx.boot.track_id.as_str(),
            &["task.failed", "task.execution_settled"],
            None,
        )
        .await
        .unwrap();
    assert_eq!(persisted.len(), 2);
    assert!(persisted[0].id < persisted[1].id);
    assert!(before_boot.push_watermark >= persisted[1].id);
    assert_eq!(
        (pair(&overtaken), pair(&before_boot), pair(&after_boot)),
        (
            vec!["failure", "settled"],
            vec!["failure", "settled"],
            vec!["failure", "settled"]
        ),
        "settlement must not advance either watermark past its undispatched failure, including boot replay"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settlement_catch_up_respects_recovered_harness_watermark() {
    let (fx, first, _) = stopped_failure().await;
    fx.state.dispatcher.abort_event_listener_for_test();
    // No live Planner existed when the two actual events were published.
    assert_eq!(
        fx.state
            .dispatcher
            .push_cursor_for_test(&fx.boot.planner_card_id),
        0
    );
    let initial = planner(&fx).await;
    initial.shutdown().await.unwrap();
    let recovered = restore_planner(&fx).await;
    let events = fx
        .boot
        .repo
        .events_for_track(
            fx.boot.track_id.as_str(),
            &["task.failed", "task.execution_settled"],
            None,
        )
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].event,
        calm_server::event::Event::TaskFailed { .. }
    ));
    assert!(matches!(
        events[1].event,
        calm_server::event::Event::TaskExecutionSettled { .. }
    ));
    let before = recovered.snapshot().await;
    // Redeliver the persisted settlement, then let its delayed actual failure
    // through the same production observer used by the native failure handler.
    for index in [1, 1, 0] {
        fx.state
            .dispatcher
            .catch_up_push(
                fx.boot.track_id.clone(),
                events[index].event.clone(),
                events[index].id,
            )
            .await;
    }
    // A refused no-op queue command acknowledges all prior Delivery commands;
    // snapshot() alone does not drain the asynchronous observation ingress.
    use calm_server::harness::queue::{MutationRefused, QueueEntryId, QueueMutation};
    assert_eq!(
        recovered
            .mutate_pending_entry(
                QueueMutation::Delete {
                    entry_id: QueueEntryId::from_wire("absent-observation-barrier".into()),
                    if_entry_rev: 1,
                },
                calm_server::ids::ActorId::User
            )
            .await
            .unwrap(),
        Err(MutationRefused::NotFound)
    );
    recovered.persist_snapshot().await.unwrap();
    let after = recovered.snapshot().await;
    recovered.shutdown().await.unwrap();
    let rebooted = restore_planner(&fx).await;
    let after_boot = rebooted.snapshot().await;
    rebooted.shutdown().await.unwrap();
    assert_eq!(hints(&before, &first.id), 1);
    assert_eq!(
        (
            after.pending_observations(),
            after_boot.pending_observations()
        ),
        (before.pending_observations(), before.pending_observations()),
        "settlement must synchronize the recovered prefix so a delayed failure is not duplicated live or after persistence/replay"
    );
}

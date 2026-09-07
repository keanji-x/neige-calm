//! Native failure must remain visible, then settled execution must wake Planner again.
use super::*;
use crate::mcp_track_report::{call_tool, planner_identity};
use calm_server::harness::{
    HarnessConfig, HarnessSnapshot, Observation, PlannerHarness, PlannerHarnessParams,
};
use calm_server::operation::{OperationRepo, SqlxOperationRepo};

async fn planner(fx: &Fixture) -> PlannerHarness {
    let runtime_id = planner_identity(&fx.boot).session_id;
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
            id: runtime_id.clone(),
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
        worker_session_id: runtime_id.clone(),
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
    fx.state.harness.insert(runtime_id, handle.clone());
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

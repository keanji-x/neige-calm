//! Decision context is exercised at the actual harness turn/start boundary.
use super::*;
use calm_server::codex_appserver::InputItem;
use calm_server::shared_codex_appserver::SharedCodexAppServer;

async fn queued_settlement(fx: &Fixture, handle: &PlannerHarness) {
    let events = fx
        .boot
        .repo
        .events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None)
        .await
        .unwrap();
    let event = events.last().unwrap();
    fx.state
        .dispatcher
        .catch_up_push(fx.boot.track_id.clone(), event.event.clone(), event.id)
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.snapshot().await.push_watermark < event.id {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

async fn issued_briefing(handle: &PlannerHarness, daemon: &SharedCodexAppServer) -> Value {
    handle
        .force_phase_for_dev(calm_server::harness::HarnessPhaseTag::Idle)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while daemon.turn_start_count_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let text = daemon.started_turns_for_test()[0]
        .1
        .iter()
        .filter_map(|item| match item {
            InputItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Shut down before any assertion so even a RED run leaves no live harness.
    handle.shutdown().await.unwrap();
    let snapshot = handle.snapshot().await;
    let issued = snapshot
        .issued_input_segments
        .expect("issued segments must be persisted");
    for segment in &issued.segments {
        assert!(
            text.contains(&segment.text),
            "persisted and actual model input must agree"
        );
    }
    let body = text
        .split_once("Recovery decision briefing (kernel snapshot):\n")
        .expect("actual turn must contain a decision-ready recovery briefing")
        .1
        .split_once("\nEnd recovery decision briefing.")
        .unwrap()
        .0;
    serde_json::from_str(body).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_briefing_uses_receiving_planner_permission_in_actual_turn() {
    let (fx, first, _) = stopped_failure().await;
    fx.state.dispatcher.abort_event_listener_for_test();
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let handle = planner_with_daemon(&fx, daemon.clone()).await;
    queued_settlement(&fx, &handle).await;
    let user_text = "Explain the failure; do not claim old files are inherited.";
    handle
        .observe_user_message_durable(user_text.into(), vec![])
        .await
        .unwrap();
    let brief = issued_briefing(&handle, &daemon).await;
    let issued = handle.snapshot().await.issued_input_segments.unwrap();
    assert!(issued.segments.iter().any(|segment| segment.presentation
        == calm_server::model::HarnessInputPresentation::User
        && segment.text.contains(user_text)));
    assert_eq!(brief["key"], "retry");
    assert_eq!(brief["attempt_id"], first.id);
    assert_eq!(brief["planner_recovery"]["allowed"], false);
    assert!(
        brief["planner_recovery"]["reason"]
            .as_str()
            .unwrap()
            .contains("explicit User recovery")
    );
    assert_eq!(
        brief["planner_session_id"],
        planner_identity(&fx.boot).session_id
    );
    assert_eq!(brief["settlement"]["confirmed"], true);
    assert_eq!(brief["evidence"]["run"], format!("runs/{}.json", first.id));
    assert!(brief["as_of_ms"].as_i64().unwrap() > 0);
    assert!(
        brief["limitations"]
            .as_str()
            .unwrap()
            .contains("new empty workspace")
    );
    assert_eq!(brief["executor_environment"]["network"]["enabled"], false);
    assert_eq!(
        brief["executor_environment"]["recovery"]["environment"],
        "identical"
    );
    assert_eq!(
        brief["executor_environment"]["mcp_tools"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let changes = brief["recover_changes"].as_str().unwrap();
    assert!(changes.contains("identical execution environment"));
    assert!(changes.contains("only the workspace is new"));
}

async fn planner_failure() -> (Fixture, Task) {
    let fx = fixture("controlled").await;
    crate::task_recovery::declare(&fx.boot, json!({
        "key":"retry", "kind":"codex", "goal":"Write result.txt containing 42 and report completion.",
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "ready":true,
        "no_gate_reason":"Report-driven isolated fixture.",
        "context":{"neige_execution":{"version":"isolated-codex-v1", "workspace":"empty"}}
    })).await;
    let (first, workspace) = launch(&fx).await;
    finish(&fx, &first, &workspace, false).await;
    fx.state.dispatcher.abort_event_listener_for_test();
    (fx, first)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_briefing_allows_exact_recovery_without_preflight_reads() {
    let (fx, first) = planner_failure().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let handle = planner_with_daemon(&fx, daemon.clone()).await;
    queued_settlement(&fx, &handle).await;
    let brief = issued_briefing(&handle, &daemon).await;
    assert_eq!(brief["planner_recovery"]["allowed"], true);
    assert_eq!(brief["attempt_id"], first.id);
    assert_eq!(brief["is_current"], true);
    assert_eq!(brief["failure"]["status"], "failed");
    assert!(
        brief["failure"]["detail"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    let track = fx
        .boot
        .repo
        .track_get(fx.boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let view =
        calm_server::track_fs_view::TrackFsView::new(fx.boot.repo.as_ref(), &fx.boot.ctx.write);
    for path in brief["evidence"].as_object().unwrap().values() {
        assert!(
            !view
                .cat(&track, path.as_str().unwrap())
                .await
                .unwrap()
                .content
                .is_empty()
        );
    }
    // The only model decision call uses fields from the actual delivered brief;
    // no plan.list is used to construct this request. The event listener is
    // stopped, so only the explicit scheduler call below can start its successor.
    let request = json!({
        "key":brief["key"], "expected_attempt_id":brief["attempt_id"],
        "idempotency_key":"briefing-retry", "reason":"Retry the unchanged goal in a new empty workspace.",
    });
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
    assert_eq!(receipt, replay);
    assert_ne!(receipt["attempt_id"], first.id);
    assert_eq!(
        current(&fx.boot, "retry").await.id,
        receipt["attempt_id"].as_str().unwrap()
    );
    assert_eq!(
        current(&fx.boot, "retry").await.status,
        calm_server::model::TaskStatus::Pending
    );
    let (second, workspace) = launch(&fx).await;
    assert_eq!(second.goal, first.goal);
    finish(&fx, &second, &workspace, false).await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let restored = restore_planner_with_daemon(&fx, daemon.clone()).await;
    let second_brief = issued_briefing(&restored, &daemon).await;
    assert_eq!(second_brief["attempt_id"], second.id);
    assert_eq!(second_brief["planner_recovery"]["allowed"], false);
    assert!(
        second_brief["planner_recovery"]["reason"]
            .as_str()
            .unwrap()
            .contains("Planner recovery limit reached")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_briefing_rechecks_queued_policy_after_snapshot_restore() {
    let (fx, first) = planner_failure().await;
    let initial = planner(&fx).await;
    queued_settlement(&fx, &initial).await;
    initial.persist_snapshot().await.unwrap();
    initial.shutdown().await.unwrap();
    // Change authorization after the exact settlement notice was already queued.
    // This fixture setting does not rewrite the task's frozen contract.
    sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
        .bind(fx.boot.track_id.as_str())
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let restored = restore_planner_with_daemon(&fx, daemon.clone()).await;
    let brief = issued_briefing(&restored, &daemon).await;
    assert_eq!(brief["attempt_id"], first.id);
    assert_eq!(brief["planner_recovery"]["allowed"], false);
    assert!(
        brief["planner_recovery"]["reason"]
            .as_str()
            .unwrap()
            .contains("explicit User recovery")
    );
    let turns = daemon.started_turns_for_test();
    let text = serde_json::to_string(&turns[0].1).unwrap();
    assert_eq!(
        text.matches("Recovery decision briefing (kernel snapshot)")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_briefing_never_retargets_a_queued_superseded_attempt() {
    let (fx, first) = planner_failure().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let handle = planner_with_daemon(&fx, daemon.clone()).await;
    queued_settlement(&fx, &handle).await;
    fx.state.dispatcher.semaphore().close();
    let (status, receipt) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let brief = issued_briefing(&handle, &daemon).await;
    assert_eq!(brief["attempt_id"], first.id);
    assert_eq!(brief["is_current"], false);
    assert_eq!(brief["planner_recovery"]["allowed"], false);
    assert_eq!(brief["planner_recovery"]["code"], "superseded");
    assert_eq!(brief["evidence"]["run"], format!("runs/{}.json", first.id));
    let stale_request = json!({"key":brief["key"], "expected_attempt_id":brief["attempt_id"],
        "idempotency_key":"stale-brief", "reason":"Retry the observed attempt."});
    let error = call_tool(
        &fx.boot,
        "calm.plan.recover",
        planner_identity(&fx.boot),
        stale_request,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32409);
    assert!(error.message.contains("no longer current"));
    assert_eq!(
        current(&fx.boot, "retry").await.id,
        receipt["attempt_id"].as_str().unwrap()
    );
}

#[path = "isolated_codex_semantic_recovery.rs"]
mod semantic;

use super::*;

#[tokio::test]
async fn child_track_met_result_uses_kernel_report_and_latest_candidate() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let (_task, child) = seed_child_parent(&boot, "met-result", true, None).await;
    let report = boot
        .repo
        .cards_by_track(&child)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind == "track-report")
        .unwrap();
    let payload = calm_server::track_report::TrackReportPayload::initial();
    let mut doc = calm_server::track_report_doc::ReportDoc::from_payload(&payload);
    doc.increment_doc_rev().unwrap();
    doc.increment_doc_rev().unwrap();
    sqlx::query("UPDATE cards SET body_crdt=?1 WHERE id=?2")
        .bind(doc.to_bytes())
        .bind(report.id.as_str())
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    let expected = crate::support::child_track::seed_candidates(
        boot.repo.as_ref(),
        &child,
        report.id.as_str(),
    )
    .await;
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    let result = &event_rows(&boot, "task.completed").await[0].1["result"];
    assert_eq!(
        result,
        &json!({"source":"child-track","outcome":"met","child_track_id":child,
        "report":{"card_id":report.id,"rev":2},"candidate":expected})
    );
}

#[tokio::test]
async fn child_track_done_production_report_routes_gate_and_replays_once() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    let dir = unique_gate_dir("child-report");
    let (_runtime, scheduler) =
        build_scheduler(&boot, vec![Arc::new(TaskVerifyAdapter::new(dir.clone()))]);
    let gate =
        json!({"cwd":dir.to_str().unwrap(),"steps":[{"name":"ok","cmd":"true"}]}).to_string();
    let (attempt, child) = seed_child_parent(&boot, "gated-report", false, Some(gate)).await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: child.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: support::planner::payload(),
        })
        .await
        .unwrap();
    support::mcp::set_persisted_card_role(boot.repo.as_ref(), card.id.as_str(), CardRole::Planner)
        .await;
    boot.card_role_cache
        .insert(card.id.clone(), CardRole::Planner, child.clone().into());
    boot.ctx
        .write
        .remember_track(child.clone().into(), boot.area_id.clone());
    support::planner::start_session(boot.repo.as_ref(), card.id.as_str(), "child-gate-session")
        .await;
    let identity = ToolCallIdentity {
        card_id: card.id.to_string(),
        session_id: "child-gate-session".into(),
        track_id: Some(child.clone()),
        ..planner_identity(&boot)
    };
    let result = call_tool(
        &boot,
        "neige_task_done",
        identity,
        json!({"attempt_id":attempt}),
    )
    .await
    .unwrap();
    assert_eq!(result["outcome"], "met");
    assert!(
        boot.repo
            .track_get(&child)
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_some()
    );
    assert_eq!(
        boot.repo.task_get(&attempt).await.unwrap().unwrap().status,
        TaskStatus::Verifying
    );
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    assert_eq!(event_rows(&boot, "task.completed").await.len(), 1);
    scheduler.schedule_track(boot.track_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "gated-report", 30).await;
    assert_eq!(row.status, TaskStatus::Done, "{row:?}");
    assert_eq!(row.gate_attempt, 1);
    assert_eq!(operation_count(&boot, "task-verify").await, 1);
    std::fs::remove_dir_all(dir).ok();
}

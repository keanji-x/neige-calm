//! R1 review regressions through public read and real queued Planner input.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_plan_list_isolates_invalid_references() {
    for defect in ["copy", "remove-r2"] {
        let (fx, _, _, _, _) = rejected().await;
        let pair = request(&fx, "Fix findings").await.unwrap();
        let key = if defect == "copy" {
            let mut forged = receipt(&fx).await["repair"]["payload"].clone();
            forged["key"] = json!("forged-repair");
            declare(&fx.boot, forged).await;
            "forged-repair".to_owned()
        } else {
            let key = pair["receipt"]["review_key"].as_str().unwrap().to_owned();
            edit_task(&fx, &key, "/context/neige_execution/repair", Value::Null).await;
            key
        };
        let result = call_tool(
            &fx.boot,
            "calm.plan.list",
            planner_identity(&fx.boot),
            json!({}),
        )
        .await;
        let view = result.expect("one invalid reference must not hide the whole plan");
        let tasks = view["tasks"].as_array().unwrap();
        let invalid = tasks.iter().find(|task| task["key"] == key).unwrap();
        assert_eq!(invalid["file_delivery"]["state"], "invalid", "{view}");
        assert_eq!(invalid["file_delivery"]["qualified"], false);
        assert!(
            invalid["file_delivery"]["qualification"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("repair reference"))
        );
        assert!(tasks.iter().any(|task| task["key"] == "produce"));
        assert!(tasks.iter().any(|task| task["key"] == "review"));
        assert_eq!(current(&fx.boot, &key).await.status, TaskStatus::Pending);
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
            .bind(current(&fx.boot,&key).await.id).fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
        assert_eq!(count, 0);
        // Expected reference conflicts are local; actual database errors are still read errors.
        sqlx::query("ALTER TABLE task_candidate_repairs RENAME TO unavailable_repair_receipts")
            .execute(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        assert!(
            call_tool(
                &fx.boot,
                "calm.plan.list",
                planner_identity(&fx.boot),
                json!({})
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_settlement_replay_rechecks_original_lineage() {
    for original in ["produce", "review"] {
        let (count, evidence) = notice_case(original, false).await;
        assert_eq!(
            evidence["relevant"], false,
            "withdrawn {original}: direct production relevance"
        );
        assert_eq!(count, 0, "withdrawn {original}: replay must be irrelevant");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_queued_briefing_rechecks_original_lineage() {
    for original in ["produce", "review"] {
        let (_, brief) = notice_case(original, true).await;
        assert_eq!(
            brief["current_authority"], false,
            "withdrawn {original}: {brief}"
        );
        assert!(
            brief["authority_reason"]
                .as_str()
                .is_some_and(|r| !r.is_empty())
        );
    }
}
async fn notice_case(original: &str, queued: bool) -> (usize, Value) {
    use super::super::settlement::{notices, observed};
    use crate::isolated_codex_retry::recovery_wake::{planner, planner_with_daemon};
    use calm_server::{codex_appserver::InputItem, shared_codex_appserver::SharedCodexAppServer};
    let (fx, _, _, _, _) = rejected().await;
    let pair = request(&fx, "Fix findings").await.unwrap();
    let (_, _, _, r2) = produce_c2(&fx, &pair).await;
    std::fs::write(
        workspace(&fx, &r2).await.join("report-result.json"),
        passed().to_string(),
    )
    .unwrap();
    settle(&fx, &r2, true).await;
    let event=fx.boot.repo.events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None).await.unwrap()
        .into_iter().find(|e|matches!(&e.event,calm_server::event::Event::TaskExecutionSettled{task_id,..} if task_id==&r2.id)).unwrap();
    let source = current(&fx.boot, original).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    calm_server::db::sqlite::session_supersede_active_tx(
        &mut tx,
        &planner_identity(&fx.boot).session_id,
        calm_server::model::now_ms(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    if !queued {
        let handle = planner(&fx).await;
        // Accept the real preceding R2 completion before withdrawal. Otherwise an
        // unrelated old C1 prefix can prevent replay before the R2 relevance gate.
        let completed=fx.boot.repo.events_for_track(fx.boot.track_id.as_str(), &["task.completed"], None).await.unwrap()
            .into_iter().find(|e|matches!(&e.event,calm_server::event::Event::TaskCompleted{idempotency_key,..} if idempotency_key==&r2.id)).unwrap();
        fx.state
            .dispatcher
            .catch_up_push(fx.boot.track_id.clone(), completed.event, completed.id)
            .await;
        assert!(
            observed(&handle, &r2.id, false).await,
            "real completion prefix must be consumed"
        );
        sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
            .bind(&source.id)
            .execute(&pool)
            .await
            .unwrap();
        let calm_server::event::Event::TaskExecutionSettled { operation_id, .. } = &event.event
        else {
            unreachable!()
        };
        let relevant = calm_server::file_delivery::candidate_review_notice_relevant(
            fx.boot.repo.as_ref(),
            &fx.boot.track_id,
            &r2.id,
            operation_id,
        )
        .await
        .unwrap();
        fx.state
            .dispatcher
            .catch_up_push(fx.boot.track_id.clone(), event.event.clone(), event.id)
            .await;
        let count = notices(&handle.snapshot().await, &r2.id);
        handle.shutdown().await.unwrap();
        return (count, json!({"relevant":relevant}));
    }
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let handle = planner_with_daemon(&fx, daemon.clone()).await;
    calm_server::semantic_recovery::test_support::register_thread(
        fx.boot.repo.as_ref(),
        fx.boot.planner_card_id.as_str(),
        "planner-observer",
    )
    .await
    .unwrap();
    fx.state
        .dispatcher
        .catch_up_push(fx.boot.track_id.clone(), event.event.clone(), event.id)
        .await;
    let observed = observed(&handle, &r2.id, true).await;
    sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
        .bind(&source.id)
        .execute(&pool)
        .await
        .unwrap();
    handle
        .force_phase_for_dev(calm_server::harness::HarnessPhaseTag::Idle)
        .await
        .unwrap();
    let issued = tokio::time::timeout(Duration::from_secs(10), async {
        while daemon.turn_start_count_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let turns = daemon.started_turns_for_test();
    handle.shutdown().await.unwrap();
    assert!(observed, "valid R2 notice must queue before withdrawal");
    issued.expect("actual Planner turn must issue from queued notice");
    let text = turns[0]
        .1
        .iter()
        .filter_map(|i| match i {
            InputItem::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body = text
        .rsplit_once("Candidate review execution settled (kernel snapshot):\n")
        .unwrap()
        .1
        .split_once("\nEnd candidate review settlement.")
        .unwrap()
        .0;
    (0, serde_json::from_str(body).unwrap())
}

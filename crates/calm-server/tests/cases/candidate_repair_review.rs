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
        assert_eq!(
            brief["repair_acceptance"]["state"], "unavailable",
            "{brief}"
        );
        assert!(brief["repair_acceptance"]["next_action"].is_null());
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
    let (c2, publication, machine, r2) = produce_c2(&fx, &pair).await;
    std::fs::write(
        workspace(&fx, &r2).await.join("report-result.json"),
        passed().to_string(),
    )
    .unwrap();
    if original == "unsettled" {
        // Submit the real report while the controlled provider still owns its run.
        call_tool(
            &fx.boot,
            "calm.task.complete",
            review_identity(&fx, &r2).await,
            json!({"idempotency_key":r2.id,"result":passed(),"artifacts":[]}),
        )
        .await
        .unwrap();
        let operation = fx
            .state
            .operation_runtime
            .find_by_kind_and_idempotency("codex-isolated-worker", &r2.id)
            .await
            .unwrap()
            .unwrap();
        let observation = calm_server::file_delivery::candidate_review_notice_observation(
            fx.boot.repo.as_ref(),
            &fx.boot.track_id,
            &r2.id,
            &operation.id,
        )
        .await;
        // Always stop the real fixture worker before asserting reader results.
        settle(&fx, &r2, true).await;
        let calm_server::harness::Observation::SystemContext { text } =
            observation.unwrap().unwrap()
        else {
            panic!("expected production settlement context")
        };
        return (
            0,
            parse_briefing(&text, &r2.id, None).expect("exact R2 briefing"),
        );
    }
    settle(&fx, &r2, true).await;
    let event=fx.boot.repo.events_for_track(fx.boot.track_id.as_str(), &["task.execution_settled"], None).await.unwrap()
        .into_iter().find(|e|matches!(&e.event,calm_server::event::Event::TaskExecutionSettled{task_id,..} if task_id==&r2.id)).unwrap();
    let source = current(
        &fx.boot,
        if matches!(original, "produce" | "review") {
            original
        } else {
            "produce"
        },
    )
    .await;
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
    // Exercise one authenticated settlement through the real queue and delivery
    // reader. Dispatcher prefix replay is covered separately by notice_case(false).
    let calm_server::event::Event::TaskExecutionSettled { operation_id, .. } = &event.event else {
        unreachable!()
    };
    let observation = calm_server::file_delivery::candidate_review_notice_observation(
        fx.boot.repo.as_ref(),
        &fx.boot.track_id,
        &r2.id,
        operation_id,
    )
    .await
    .unwrap()
    .unwrap();
    handle.observe_envelope(observation, event.id).unwrap();
    let observed = observed(&handle, &r2.id, true).await;
    if matches!(original, "accepted" | "rejected") {
        verdict(&fx, &c2, original).await.unwrap();
    } else if original == "revoked-receipt" {
        sqlx::query("DELETE FROM task_candidate_repairs")
            .execute(&pool)
            .await
            .unwrap();
    } else if original == "failed" {
        sqlx::query("UPDATE operations SET phase=?1 WHERE kind='codex-isolated-worker' AND idempotency_key=?2")
            .bind("failed")
            .bind(&r2.id).execute(&pool).await.unwrap();
    } else if original != "ready" {
        sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
            .bind(&source.id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let brief = if matches!(original, "accepted" | "rejected" | "failed") {
        let calm_server::event::Event::TaskExecutionSettled { operation_id, .. } = &event.event
        else {
            unreachable!()
        };
        let observation = calm_server::file_delivery::candidate_review_notice_observation(
            fx.boot.repo.as_ref(),
            &fx.boot.track_id,
            &r2.id,
            operation_id,
        )
        .await
        .unwrap()
        .unwrap();
        handle.shutdown().await.unwrap();
        assert!(observed, "valid R2 notice must queue before state change");
        let calm_server::harness::Observation::SystemContext { text } = observation else {
            panic!("expected production settlement context")
        };
        parse_briefing(&text, &r2.id, None).expect("exact R2 briefing")
    } else {
        handle
            .force_phase_for_dev(calm_server::harness::HarnessPhaseTag::Idle)
            .await
            .unwrap();
        let issued = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                for (_, items) in daemon.started_turns_for_test() {
                    for item in items {
                        if let InputItem::Text { text } = item
                            && let Some(brief) = parse_briefing(&text, &r2.id, Some(event.id))
                        {
                            return brief;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if issued.is_err() {
            // Keep exact started/pending event identities available on a failure.
            eprintln!(
                "Expected R2 {}/{}; started turns: {:?}; queue snapshot: {:?}",
                r2.id,
                event.id,
                daemon.started_turns_for_test(),
                handle.snapshot().await
            );
        }
        handle.shutdown().await.unwrap();
        assert!(observed, "valid R2 notice must queue before withdrawal");
        issued.expect("actual Planner input must contain the exact R2 settlement")
    };
    if original == "ready" {
        assert_eq!(brief["subject"]["producer_attempt_id"], c2.id);
        assert_eq!(brief["subject"]["publication_operation_id"], publication);
        assert_eq!(
            brief["subject"]["verification_operation_id"],
            machine["verification_operation_id"]
        );
        assert_eq!(
            brief["delivery"]["candidate"]["snapshot"],
            machine["candidate"]["snapshot"]
        );
        assert_eq!(brief["delivery"]["review"]["review_attempt_id"], r2.id);
        let decisions: i64 = sqlx::query_scalar("SELECT count(*) FROM task_candidate_decisions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(decisions, 0, "briefing must not record acceptance");
        let action = &brief["repair_acceptance"]["next_action"];
        if let Some(tool) = action["tool"].as_str() {
            let mut args = action["arguments"].clone();
            args["message"] =
                json!("Accept C2 after checking both resolved findings and fresh checks.");
            call_tool(&fx.boot, tool, planner_identity(&fx.boot), args)
                .await
                .unwrap();
            let decisions: i64 =
                sqlx::query_scalar("SELECT count(*) FROM task_candidate_decisions")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(decisions, 1, "explicit producer verdict records acceptance");
        }
    }
    (0, brief)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_acceptance_briefing_ready() {
    let (_, brief) = notice_case("ready", true).await;
    let phase = &brief["repair_acceptance"];
    assert_eq!(phase["state"], "acceptance-ready", "{brief}");
    assert_eq!(phase["next_action"]["tool"], "calm.task.verdict");
    assert_eq!(
        phase["next_action"]["arguments"]["idempotency_key"],
        brief["subject"]["producer_attempt_id"]
    );
    assert_eq!(phase["next_action"]["arguments"]["status"], "accepted");
    assert_eq!(
        brief["delivery"]["review"]["finding_responses"],
        passed()["finding_responses"]
    );
    assert_eq!(
        brief["delivery"]["repair"]["blocking_findings"],
        json!(FINDINGS)
    );
    assert_eq!(brief["delivery"]["verification"]["passed"], true);
    assert_eq!(
        brief["delivery"]["review"]["operation"]["state"],
        "succeeded"
    );
    assert!(brief["delivery"]["candidate"]["snapshot"].is_string());
}

async fn assert_acceptance_refused(state: &str) {
    let (_, brief) = notice_case(state, true).await;
    let phase = &brief["repair_acceptance"];
    assert_eq!(
        phase["state"],
        if state == "failed" {
            "blocked"
        } else {
            "unavailable"
        },
        "{state}: {brief}"
    );
    assert!(phase["next_action"].is_null(), "{brief}");
    assert!(
        phase["reason"].as_str().is_some_and(|s| !s.is_empty()),
        "{brief}"
    );
}

// Each scenario owns a Tokio runtime so fixture background work cannot outlive
// its scenario and accumulate into the next one. Nextest also isolates processes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_acceptance_briefing_refuses_revoked_receipt() {
    assert_acceptance_refused("revoked-receipt").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_acceptance_briefing_blocks_failed_review() {
    assert_acceptance_refused("failed").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_acceptance_briefing_refuses_unsettled_execution() {
    assert_acceptance_refused("unsettled").await;
}

async fn assert_acceptance_decided(state: &str) {
    let (_, brief) = notice_case(state, true).await;
    let phase = &brief["repair_acceptance"];
    assert_eq!(phase["state"], "already-decided", "{brief}");
    assert!(phase["next_action"].is_null(), "{brief}");
    assert_eq!(brief["delivery"]["decision"]["state"], state);
    assert!(brief["delivery"]["decision"]["event_id"].is_i64());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_acceptance_briefing_already_accepted() {
    assert_acceptance_decided("accepted").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_acceptance_briefing_already_rejected() {
    assert_acceptance_decided("rejected").await;
}

// Historical R1 and current R2 notices can share a turn or arrive in different
// turns. Select the notified identity, never the first turn's last settlement.
fn parse_briefing(text: &str, attempt: &str, event: Option<i64>) -> Option<Value> {
    text.split("Candidate review execution settled (kernel snapshot):\n")
        .skip(1)
        .find_map(|section| {
            let (body, _) = section.split_once("\nEnd candidate review settlement.")?;
            let brief: Value = serde_json::from_str(body).expect("production briefing JSON");
            (brief["review_attempt_id"] == attempt
                && event.is_none_or(|id| brief["event_id"] == id))
            .then_some(brief)
        })
}

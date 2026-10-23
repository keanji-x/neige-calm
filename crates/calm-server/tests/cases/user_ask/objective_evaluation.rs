//! Measurement acceptance through Planner tools, REST approval and the real projector.
use super::objective_approval::{define, definition, request, setup};
use super::*;
use calm_truth::objective_host;
const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEXT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
async fn evaluate(
    boot: &Boot,
    revision: &str,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(
        boot,
        "neige_objective_evaluate",
        json!({"key":"kr","revision":revision}),
    )
    .await
}
async fn settle(
    boot: &Boot,
    decision: &str,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(
        boot,
        "neige_objective_settle",
        json!({"key":"kr","evaluation":0,"decision":decision,"note":"measured"}),
    )
    .await
}
async fn approve(boot: &Boot) {
    let id = request(boot).await["ask_id"].as_i64().unwrap();
    assert_eq!(
        answer(boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
}
async fn state(boot: &Boot) -> objective::State {
    objective_host::load_state(&boot.repo.sqlite_db().unwrap(), &boot.track_id)
        .await
        .unwrap()
        .1
}
async fn project(boot: &Boot) {
    calm_server::track_planner_context::TrackPlannerContextProjector::new(
        boot.repo.clone(),
        boot.events.clone(),
        boot.ctx.write.clone(),
    )
    .unwrap()
    .recompute_track(boot.track_id.as_str())
    .await
    .unwrap();
}
async fn counts(boot: &Boot) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM objective_events")
        .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap()
}
async fn accepted(boot: &Boot, holds: bool) {
    let actor = calm_server::ids::ActorId::AiPlanner(boot.planner_card_id.clone());
    let scope = calm_server::event::EventScope::Track {
        track: boot.track_id.clone(),
        area: boot.area_id.clone(),
    };
    // Use the existing authorized append seam; never insert event rows directly.
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            calm_server::db::sqlite::append_decision_event_in_tx(
                tx,
                &actor,
                &scope,
                None,
                &if holds {
                    Event::TaskCompleted {
                        notification: calm_types::event::PlannerNotification::Suppress,
                        idempotency_key: "finish".into(),
                        result: json!({"status":"accepted"}),
                        artifacts: vec![],
                        agent_message: None,
                    }
                } else {
                    Event::TaskFailed {
                        notification: calm_types::event::PlannerNotification::Suppress,
                        idempotency_key: "finish".into(),
                        reason: "rejected".into(),
                        details: None,
                        agent_message: None,
                    }
                },
            )
            .await?;
            Ok::<_, calm_server::error::CalmError>(())
        })
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn objective_evaluation_fact_cycle_projection_and_conflicting_retry() {
    let (boot, _) = setup().await;
    assert!(
        evaluate(&boot, REV)
            .await
            .unwrap_err()
            .message
            .contains("approval_required")
    );
    approve(&boot).await;
    accepted(&boot, true).await;
    let requested = evaluate(&boot, REV).await.unwrap();
    assert_eq!(requested["evaluation"], 0);
    let s = state(&boot).await;
    let kr = &s.objectives[&objective::ids::ObjectiveKey::new("kr")];
    assert!(matches!(
        kr.evaluation,
        Some(objective::engine::Evaluation::Ready { .. })
    ));
    project(&boot).await;
    project(&boot).await;
    let wakes = rows(&boot, "track.wake_requested").await;
    assert_eq!(wakes.len(), 1);
    settle(&boot, "keep").await.unwrap();
    settle(&boot, "keep").await.unwrap();
    assert_eq!(settle(&boot, "discard").await.unwrap_err().code, -32409);
    project(&boot).await;
    let overlays = boot
        .repo
        .overlays_for("track", boot.track_id.as_str())
        .await
        .unwrap();
    let row = &overlays
        .iter()
        .find(|o| o.kind == "objective.kr")
        .unwrap()
        .payload["cell"]["table"]["rows"][0];
    assert_eq!(row["baseline"], "aaaaaaa");
    assert_eq!(row["ledger"], "1");
    assert_eq!(row["verdict"], "done");
}
#[tokio::test]
async fn objective_evaluation_atomic_restart_after_dispatch_before_report() {
    let (boot, _) = setup().await;
    approve(&boot).await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query(
        "CREATE TRIGGER crash_fact BEFORE INSERT ON objective_events \
         WHEN NEW.kind='check_finished' \
         BEGIN SELECT RAISE(ABORT,'crash after dispatched'); END",
    )
    .execute(db.read_pool())
    .await
    .unwrap();
    let _ = evaluate(&boot, REV).await;
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM objective_intents WHERE kind='run_check'")
            .fetch_all(db.read_pool())
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec!["pending"],
        "mark dispatched must roll back with the failed report"
    );
    sqlx::query("DROP TRIGGER crash_fact")
        .execute(db.read_pool())
        .await
        .unwrap();
    project(&boot).await;
    let n = counts(&boot).await;
    project(&boot).await;
    assert_eq!(counts(&boot).await, n, "Reported jobs never run again");
    assert!(
        objective_host::unsettled_intents(&db)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn objective_evaluation_superseded_backlog_and_late_result() {
    let (boot, _) = setup().await;
    approve(&boot).await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query(
        "CREATE TRIGGER crash_fact BEFORE INSERT ON objective_events \
         WHEN NEW.kind='check_finished' \
         BEGIN SELECT RAISE(ABORT,'hold results'); END",
    )
    .execute(db.read_pool())
    .await
    .unwrap();
    let _ = evaluate(&boot, REV).await;
    let old = objective_host::unsettled_intents(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|r| matches!(r.intent, objective::Intent::RunCheck { .. }))
        .unwrap();
    let _ = evaluate(&boot, NEXT).await;
    sqlx::query("DROP TRIGGER crash_fact")
        .execute(db.read_pool())
        .await
        .unwrap();
    project(&boot).await;
    assert!(
        objective_host::unsettled_intents(&db)
            .await
            .unwrap()
            .is_empty(),
        "obsolete RunCheck must not accumulate"
    );
    let settlement: String = sqlx::query_scalar("SELECT settlement_json FROM objective_intents WHERE track_id=? AND event_seq=? AND ordinal=?")
        .bind(boot.track_id.as_str()).bind(old.id.seq.0 as i64).bind(i64::from(old.id.ordinal))
        .fetch_one(db.read_pool()).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&settlement).unwrap()["settlement"],
        "superseded"
    );
    let objective::Intent::RunCheck {
        objective,
        evaluation,
        check,
        revision,
        sample,
    } = old.intent
    else {
        panic!()
    };
    assert!(
        objective_host::react(
            &db,
            objective_host::ObjectiveInput {
                track_id: boot.track_id.clone(),
                source: objective_host::InputSource::IntentJob { intent: old.id },
                event: objective::Event::CheckFinished {
                    objective,
                    evaluation,
                    check,
                    revision,
                    sample,
                    output: objective::check::SampleOutput::Fact(objective::check::FactOutput {
                        holds: true
                    })
                }
            }
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn objective_evaluation_refusals_and_unsupported_checks_write_nothing() {
    for config in [
        json!({"kind":"metric","params":{"command":"true","direction":"lower"}}),
        json!({"kind":"fact","params":{"fact":"pr_merged","subject":"#1"}}),
    ] {
        let (boot, block) = setup().await;
        let mut d = definition();
        d["checks"][0]["config"] = config;
        define(&boot, Some(block), d).await;
        approve(&boot).await;
        let n = counts(&boot).await;
        assert!(evaluate(&boot, REV).await.is_err());
        assert_eq!(counts(&boot).await, n);
    }
    let (boot, _) = setup().await;
    for rev in ["bad", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
        assert!(evaluate(&boot, rev).await.is_err());
    }
    crate::support::mcp::set_persisted_card_role(
        boot.repo.as_ref(),
        boot.planner_card_id.as_str(),
        CardRole::Worker,
    )
    .await;
    assert_eq!(evaluate(&boot, REV).await.unwrap_err().code, -32403);
    assert_eq!(settle(&boot, "keep").await.unwrap_err().code, -32403);
}
#[tokio::test]
async fn objective_evaluation_regression_refused_and_notified() {
    let (boot, block) = setup().await;
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":2}});
    d["checks"][0]["role"] = json!("ratchet");
    define(&boot, Some(block), d).await;
    approve(&boot).await;
    accepted(&boot, true).await;
    evaluate(&boot, REV).await.unwrap();
    settle(&boot, "keep").await.unwrap();
    accepted(&boot, false).await;
    evaluate(&boot, NEXT).await.unwrap();
    call_tool(
        &boot,
        "neige_objective_settle",
        json!({"key":"kr","evaluation":1,"decision":"keep","note":"regression"}),
    )
    .await
    .unwrap();
    project(&boot).await;
    assert_eq!(
        state(&boot).await.objectives[&objective::ids::ObjectiveKey::new("kr")]
            .ledger
            .entries
            .len(),
        1
    );
    assert!(rows(&boot, "track.wake_requested").await.iter().any(|r| {
        r.1.payload_value()["text"]
            .as_str()
            .unwrap()
            .contains("SettleRefused")
    }));
}

#[tokio::test]
async fn objective_evaluation_ask_answered_and_progress_projection() {
    let (boot, block) = setup().await;
    let id = ask_one(&boot, "A fact to measure").await;
    let mut d = definition();
    d["checks"][0]["config"] =
        json!({"kind":"fact","params":{"fact":"ask_answered","subject":id.to_string()}});
    define(&boot, Some(block), d).await;
    approve(&boot).await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query(
        "CREATE TRIGGER pause_fact BEFORE INSERT ON objective_events \
         WHEN NEW.kind='check_finished' \
         BEGIN SELECT RAISE(ABORT,'pause'); END",
    )
    .execute(db.read_pool())
    .await
    .unwrap();
    evaluate(&boot, REV).await.unwrap();
    project(&boot).await;
    let overlays = boot
        .repo
        .overlays_for("track", boot.track_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        overlays
            .iter()
            .find(|o| o.kind == "objective.kr")
            .unwrap()
            .payload["cell"]["table"]["rows"][0]["evaluation"],
        "0/1"
    );
    sqlx::query("DROP TRIGGER pause_fact")
        .execute(db.read_pool())
        .await
        .unwrap();
    project(&boot).await;
    let s = state(&boot).await;
    let objective::engine::Evaluation::Ready { results, .. } = s.objectives
        [&objective::ids::ObjectiveKey::new("kr")]
        .evaluation
        .as_ref()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        results[&objective::ids::CheckId::new("accepted")],
        objective::check::CheckOutput::Fact(objective::check::FactOutput { holds: false })
    );
    let overlays = boot
        .repo
        .overlays_for("track", boot.track_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        overlays
            .iter()
            .find(|o| o.kind == "objective.kr")
            .unwrap()
            .payload["cell"]["table"]["rows"][0]["evaluation"],
        "待结算"
    );
    settle(&boot, "discard").await.unwrap();
    assert_eq!(
        answer(&boot, id, json!([{"text":"yes"}])).await.0,
        StatusCode::NO_CONTENT
    );
    evaluate(&boot, NEXT).await.unwrap();
    let s = state(&boot).await;
    let objective::engine::Evaluation::Ready { results, .. } = s.objectives
        [&objective::ids::ObjectiveKey::new("kr")]
        .evaluation
        .as_ref()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        results[&objective::ids::CheckId::new("accepted")],
        objective::check::CheckOutput::Fact(objective::check::FactOutput { holds: true })
    );
}

#[tokio::test]
async fn objective_evaluation_plateau_and_approval_invalidated_notices_delivered_once() {
    let (boot, block) = setup().await;
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":1}});
    d["checks"][0]["role"] = json!("ratchet");
    define(&boot, Some(block.clone()), d.clone()).await;
    approve(&boot).await;
    evaluate(&boot, REV).await.unwrap();
    settle(&boot, "discard").await.unwrap();
    assert!(rows(&boot, "track.wake_requested").await.iter().any(|r| {
        r.1.payload_value()["text"]
            .as_str()
            .unwrap()
            .contains("PlateauReached")
    }));
    d["checks"][0]["config"]["params"]["subject"] = json!("different");
    define(&boot, Some(block), d).await;
    project(&boot).await;
    project(&boot).await;
    let wakes = rows(&boot, "track.wake_requested").await;
    assert_eq!(wakes.len(), 3);
    assert!(wakes.iter().any(|r| {
        r.1.payload_value()["text"]
            .as_str()
            .unwrap()
            .contains("ApprovalInvalidated")
    }));
}

#[tokio::test]
async fn objective_evaluation_settle_admission_preserves_request_ids() {
    let (boot, block) = setup().await;
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":10}});
    d["checks"][0]["role"] = json!("ratchet");
    define(&boot, Some(block), d).await;
    approve(&boot).await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query("CREATE TRIGGER pause_settle BEFORE INSERT ON objective_events WHEN NEW.kind='check_finished' \
            BEGIN SELECT RAISE(ABORT,'pause'); END")
        .execute(db.read_pool()).await.unwrap();
    evaluate(&boot, REV).await.unwrap();
    let n = counts(&boot).await;
    let error = settle(&boot, "discard").await.unwrap_err();
    assert_eq!(error.code, -32409);
    assert!(error.message.starts_with("neige_objective_settle:"));
    assert!(error.message.contains("Running"));
    assert_eq!(counts(&boot).await, n);
    sqlx::query("DROP TRIGGER pause_settle")
        .execute(db.read_pool())
        .await
        .unwrap();
    project(&boot).await;
    let n = counts(&boot).await;
    let future = json!({"key":"kr","evaluation":1,"decision":"discard","note":"future"});
    assert_eq!(
        call_tool(&boot, "neige_objective_settle", future.clone())
            .await
            .unwrap_err()
            .code,
        -32409
    );
    assert_eq!(counts(&boot).await, n);
    settle(&boot, "discard").await.unwrap();
    let n = counts(&boot).await;
    assert_eq!(
        settle(&boot, "discard").await.unwrap()["status"],
        "Duplicate"
    );
    assert_eq!(counts(&boot).await, n);
    assert_eq!(settle(&boot, "keep").await.unwrap_err().code, -32409);
    assert_eq!(evaluate(&boot, NEXT).await.unwrap()["evaluation"], 1);
    project(&boot).await;
    call_tool(&boot, "neige_objective_settle", future)
        .await
        .unwrap();
    assert_eq!(
        state(&boot).await.objectives[&objective::ids::ObjectiveKey::new("kr")]
            .ledger
            .entries
            .len(),
        2
    );
}

#[tokio::test]
async fn objective_evaluation_retry_leaves_next_round_available() {
    let (boot, block) = setup().await;
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":10}});
    d["checks"][0]["role"] = json!("ratchet");
    define(&boot, Some(block), d).await;
    approve(&boot).await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query("CREATE TRIGGER pause_retry BEFORE INSERT ON objective_events WHEN NEW.kind='check_finished' \
            BEGIN SELECT RAISE(ABORT,'pause'); END")
        .execute(db.read_pool()).await.unwrap();
    let first = evaluate(&boot, REV).await.unwrap();
    let n = counts(&boot).await;
    assert_eq!(
        evaluate(&boot, REV).await.unwrap()["evaluation"],
        first["evaluation"]
    );
    assert_eq!(
        counts(&boot).await,
        n,
        "Running retry must not append Requested"
    );
    sqlx::query("DROP TRIGGER pause_retry")
        .execute(db.read_pool())
        .await
        .unwrap();
    project(&boot).await;
    let n = counts(&boot).await;
    assert_eq!(
        evaluate(&boot, REV).await.unwrap()["evaluation"],
        first["evaluation"]
    );
    assert_eq!(
        counts(&boot).await,
        n,
        "Ready retry must not append Requested"
    );
    settle(&boot, "discard").await.unwrap();
    assert_eq!(evaluate(&boot, REV).await.unwrap()["evaluation"], 1);
    let runs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM objective_intents WHERE kind='run_check'")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    assert_eq!(runs, 2);
    project(&boot).await;
    assert!(matches!(
        state(&boot).await.objectives[&objective::ids::ObjectiveKey::new("kr")].evaluation,
        Some(objective::engine::Evaluation::Ready { .. })
    ));
}

#[tokio::test]
async fn objective_evaluation_reads_legacy_task_verdicts_without_rewriting_sources() {
    for holds in [true, false] {
        let (boot, _) = setup().await;
        approve(&boot).await;
        accepted(&boot, holds).await;
        let db = boot.repo.sqlite_db().unwrap();
        // Historical persisted input, before owner-declared notification existed.
        sqlx::query(
            "UPDATE events SET event_version=30,payload=json_remove(payload,'$.notification') \
            WHERE kind IN ('task.completed','task.failed') AND scope_track=?1",
        )
        .bind(boot.track_id.as_str())
        .execute(db.writer_pool())
        .await
        .unwrap();
        let source = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT id,payload,event_version FROM events WHERE scope_track=?1 \
             AND kind IN ('task.completed','task.failed') ORDER BY id",
        )
        .bind(boot.track_id.as_str())
        .fetch_all(db.read_pool())
        .await
        .unwrap();
        assert_eq!(source.len(), 1);
        evaluate(&boot, REV).await.unwrap();
        project(&boot).await;
        let state = state(&boot).await;
        let objective::engine::Evaluation::Ready { results, .. } = state.objectives
            [&objective::ids::ObjectiveKey::new("kr")]
            .evaluation
            .as_ref()
            .unwrap()
        else {
            panic!("legacy task verdict must finish the real evaluation");
        };
        assert_eq!(
            results[&objective::ids::CheckId::new("accepted")],
            objective::check::CheckOutput::Fact(objective::check::FactOutput { holds })
        );
        let after = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT id,payload,event_version FROM events WHERE scope_track=?1 \
             AND kind IN ('task.completed','task.failed') ORDER BY id",
        )
        .bind(boot.track_id.as_str())
        .fetch_all(db.read_pool())
        .await
        .unwrap();
        assert_eq!(
            after, source,
            "reading legacy input must preserve source identity"
        );
    }
}

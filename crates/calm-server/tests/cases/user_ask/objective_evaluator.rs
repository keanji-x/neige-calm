//! Evaluators cross the production Planner, Report and worker-done boundaries.
use super::objective_approval::{define, request, setup_on};
use super::*;
use calm_server::ids::ActorId;
use calm_truth::objective_host;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEXT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
async fn fixture() -> Boot {
    let (boot, block) = setup_on(boot_with_task_dispatch(false).await).await;
    let mut d = super::objective_approval::definition();
    d["mode"] = json!({"loop":{"plateau":3}});
    d["checks"] = json!([{"id":"review","role":"ratchet","config":{"kind":"evaluator","params":{
        "rubric":[{"id":"quality","criterion":"Keep observable behavior correct"}],"samples":3,"aggregate":"majority"}}}]);
    define(&boot, Some(block), d).await;
    let ask = request(&boot).await["ask_id"].as_i64().unwrap();
    assert_eq!(
        answer(&boot, ask, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    boot
}
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
async fn blocks(boot: &Boot) -> Vec<calm_types::track_report::ReportBlock> {
    calm_server::track_report::resolve_report_for_track(boot.repo.as_ref(), boot.track_id.as_str())
        .await
        .unwrap()
        .2
        .blocks
        .unwrap()
}
async fn tasks(boot: &Boot) -> Vec<calm_types::track_report::ReportBlock> {
    blocks(boot)
        .await
        .into_iter()
        .filter(|b| b.kind == "task")
        .collect()
}
// Inspect persisted envelopes, including the session id and report-owned attribution.
async fn assert_evaluator_attribution(boot: &Boot, session: &str) {
    let edits = boot
        .repo
        .events_for_track(boot.track_id.as_str(), &["track.report_edited"], None)
        .await
        .unwrap();
    let declarations: Vec<_> = edits.iter().rev().take(3).collect();
    assert_eq!(declarations.len(), 3);
    for row in declarations {
        assert_eq!(row.actor, ActorId::AiPlannerSession(session.into()));
        assert!(matches!(
            &row.event,
            Event::TrackReportEdited {
                author: calm_server::event::EditAuthor::Planner,
                ..
            }
        ));
    }
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
async fn count(boot: &Boot, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap()
}
fn sample() -> Value {
    json!({"summary":"checked","judgements":[{"item":"quality","trend":"same","evidence":[{"path":"src/lib.rs","line":1}]}],"findings":{"blocking":0,"major":0,"minor":0}})
}
async fn worker(boot: &Boot, key: &str) -> (ToolCallIdentity, String) {
    let id = format!("worker-{key}");
    let db = boot.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        id.clone(),
        NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Worker,
        true,
        &boot.card_role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let session = format!("{id}-session");
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    session_start_runtime_tx(
        &mut tx,
        calm_server::session_projection_repo::WorkerSessionInit {
            id: session.clone(),
            card_id: id.clone(),
            kind: calm_server::session_projection_repo::WorkerSessionKind::CodexCard,
            agent_provider: None,
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 1,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let task: String = sqlx::query_scalar("SELECT id FROM tasks WHERE track_id=? AND key=?")
        .bind(boot.track_id.as_str())
        .bind(key)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET status='running',worker_card_id=? WHERE id=?")
        .bind(&id)
        .bind(&task)
        .execute(db.writer_pool())
        .await
        .unwrap();
    (
        ToolCallIdentity {
            card_id: id,
            session_id: session,
            role: CardRole::Worker,
            ..planner_identity(boot)
        },
        task,
    )
}
async fn done(
    boot: &Boot,
    identity: ToolCallIdentity,
    task: &str,
    result: Value,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    boot.registry.lookup("neige_task_done").unwrap()(
        boot.ctx.clone(),
        identity,
        json!({"attempt_id":task,"result":result}),
    )
    .await
    .map(calm_server::mcp_server::result::ToolResult::into_structured)
}
#[tokio::test]
async fn objective_evaluator_done_books_samples_before_verdict_and_moves_baseline() {
    let boot = fixture().await;
    let knowledge = count(&boot, "SELECT count(*) FROM report_read_ledger_blocks").await;
    let reads = count(&boot, "SELECT count(*) FROM report_read_ledger").await;
    evaluate(&boot, REV).await.unwrap();
    assert_eq!(
        count(&boot, "SELECT count(*) FROM report_read_ledger").await,
        reads
    );
    assert_eq!(
        count(&boot, "SELECT count(*) FROM report_read_ledger_blocks").await,
        knowledge
    );
    let ts = tasks(&boot).await;
    assert_eq!(ts.len(), 3);
    assert_evaluator_attribution(&boot, PLANNER_SESSION_ID).await;
    for (i, b) in ts.iter().enumerate() {
        let p = &b.payload;
        assert_eq!(p["kind"], "codex");
        assert_eq!(p["access"], "read_only");
        assert_eq!(p["result_contract"], "evaluation-v1");
        assert_eq!(p["declared_by"], PLANNER_DECLARATION_AUTHOR);
        for key in ["head", "refs", "context", "depends_on", "gate"] {
            assert!(p.get(key).is_none(), "{key}");
        }
        let goal = p["goal"].as_str().unwrap();
        let expected = include_str!("../../../prompts/objective_evaluator.md")
            .replace(
                "{rubric}",
                r#"[{"id":"quality","criterion":"Keep observable behavior correct"}]"#,
            )
            .replace("{candidate}", REV)
            .replace("{baseline}", "absent")
            .replace("{sample}", &i.to_string());
        assert_eq!(goal, expected);
        assert!(goal.contains(REV));
        assert!(goal.contains("Keep observable behavior correct"));
        let (w, t) = worker(&boot, p["key"].as_str().unwrap()).await;
        done(&boot, w.clone(), &t, sample()).await.unwrap();
        done(&boot, w, &t, sample()).await.unwrap();
        assert_eq!(count(&boot,"SELECT count(*) FROM objective_intents WHERE json_extract(settlement_json,'$.settlement')='reported'").await,(i+1) as i64);
        call_tool(
            &boot,
            "neige_task_reject",
            json!({"attempt_id":t,"reason":"Planner disagrees","message":"Review disagrees"}),
        )
        .await
        .unwrap();
    }
    assert!(matches!(
        state(&boot).await.objectives[&objective::ids::ObjectiveKey::new("kr")].evaluation,
        Some(objective::engine::Evaluation::Ready { .. })
    ));
    project(&boot).await;
    project(&boot).await;
    assert_eq!(rows(&boot, "track.wake_requested").await.len(), 1);
    call_tool(
        &boot,
        "neige_objective_settle",
        json!({"key":"kr","evaluation":0,"decision":"keep","note":"baseline"}),
    )
    .await
    .unwrap();
    let settled = state(&boot).await;
    let baseline = settled.objectives[&objective::ids::ObjectiveKey::new("kr")]
        .ledger
        .baseline
        .as_ref()
        .unwrap();
    assert_eq!(baseline.revision.as_str(), REV);
    evaluate(&boot, NEXT).await.unwrap();
    let next: Vec<_> = tasks(&boot)
        .await
        .into_iter()
        .filter(|t| t.payload["goal"].as_str().unwrap().contains(NEXT))
        .collect();
    assert_eq!(next.len(), 3);
    assert!(
        next.iter()
            .all(|b| b.payload["goal"].as_str().unwrap().contains(REV))
    );
}
#[tokio::test]
async fn objective_evaluator_guard_rejects_changes_allows_tombstone_and_wakes_once() {
    let boot = fixture().await;
    evaluate(&boot, REV).await.unwrap();
    let b = tasks(&boot).await.remove(0);
    for field in ["goal", "refs", "key"] {
        let before = blocks(&boot).await;
        let mut payload = b.payload.clone();
        payload[field] = if field == "refs" {
            let block_id = blocks(&boot)
                .await
                .into_iter()
                .find(|b| b.kind == "prose")
                .unwrap()
                .id;
            json!([calm_types::report_links::format_track_destination(
                boot.track_id.as_str(),
                Some(&block_id)
            )])
        } else {
            json!("changed")
        };
        let err = crate::support::report_writes::upsert_block(
            &boot.ctx,
            &boot.registry,
            planner_identity(&boot),
            json!({"id":b.id,"kind":"task","payload":payload}),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code,
            if field == "key" { -32602 } else { -32403 },
            "{err:?}"
        );
        assert_eq!(blocks(&boot).await, before);
    }
    crate::support::report_writes::read_then_commit(&boot.ctx,&boot.registry,planner_identity(&boot),json!([{"op":"upsert","id":b.id,"kind":"task","payload":{"key":b.payload["key"],"declared_by":PLANNER_DECLARATION_AUTHOR,"tombstoned_by":PLANNER_DECLARATION_AUTHOR,"tombstone":{"reason":"withdrawn"}}}])).await.unwrap();
    project(&boot).await;
    project(&boot).await;
    assert_eq!(rows(&boot, "track.wake_requested").await.len(), 1);
    assert!(
        evaluate(&boot, REV).await.is_err(),
        "tombstone forbids redeclaration"
    );
}
#[tokio::test]
async fn objective_evaluator_retry_repairs_crash_without_duplicate_blocks_or_bindings() {
    let boot = fixture().await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query("CREATE TRIGGER crash_blocks BEFORE UPDATE ON cards WHEN NEW.kind='track-report' BEGIN SELECT RAISE(ABORT,'crash block write'); END").execute(db.writer_pool()).await.unwrap();
    assert!(evaluate(&boot, REV).await.is_err());
    assert_eq!(
        count(&boot, "SELECT count(*) FROM objective_check_jobs").await,
        3
    );
    assert!(tasks(&boot).await.is_empty());
    sqlx::query("DROP TRIGGER crash_blocks")
        .execute(db.writer_pool())
        .await
        .unwrap();
    project(&boot).await;
    project(&boot).await;
    let wakes = rows(&boot, "track.wake_requested").await;
    assert_eq!(wakes.len(), 3);
    assert!(wakes.iter().all(|(_, event)| matches!(event, Event::TrackWakeRequested {text,..} if text.contains(REV) && text.contains("kr"))));
    evaluate(&boot, REV).await.unwrap();
    let before = blocks(&boot).await;
    let edits = rows(&boot, "track.report_edited").await.len();
    let commits = count(&boot, "SELECT count(*) FROM track_vcs_commits").await;
    let knowledge = count(&boot, "SELECT count(*) FROM report_read_ledger_blocks").await;
    evaluate(&boot, REV).await.unwrap();
    assert_eq!(blocks(&boot).await, before);
    assert_eq!(rows(&boot, "track.report_edited").await.len(), edits);
    assert_eq!(
        count(&boot, "SELECT count(*) FROM track_vcs_commits").await,
        commits
    );
    assert_eq!(
        count(&boot, "SELECT count(*) FROM report_read_ledger_blocks").await,
        knowledge
    );
    assert_eq!(tasks(&boot).await.len(), 3);
    assert_eq!(
        count(&boot, "SELECT count(*) FROM objective_check_jobs").await,
        3
    );
}
#[tokio::test]
async fn objective_evaluator_done_atomic_failure_and_late_completion() {
    let boot = fixture().await;
    evaluate(&boot, REV).await.unwrap();
    let db = boot.repo.sqlite_db().unwrap();
    let b = tasks(&boot).await.remove(0);
    let (w, t) = worker(&boot, b.payload["key"].as_str().unwrap()).await;
    assert!(done(&boot, w.clone(), &t, json!({})).await.is_err());
    sqlx::query("CREATE TRIGGER crash_sample BEFORE INSERT ON objective_events WHEN NEW.kind='check_finished' BEGIN SELECT RAISE(ABORT,'crash sample'); END").execute(db.writer_pool()).await.unwrap();
    assert!(done(&boot, w.clone(), &t, sample()).await.is_err());
    assert_eq!(
        count(
            &boot,
            "SELECT count(*) FROM objective_events WHERE kind='check_finished'"
        )
        .await,
        0
    );
    let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id=?")
        .bind(&t)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    assert_eq!(status, "running");
    sqlx::query("DROP TRIGGER crash_sample")
        .execute(db.writer_pool())
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER crash_done BEFORE INSERT ON events WHEN NEW.kind='task.completed' BEGIN SELECT RAISE(ABORT,'crash worker event'); END")
        .execute(db.writer_pool()).await.unwrap();
    assert!(done(&boot, w.clone(), &t, sample()).await.is_err());
    assert_eq!(
        count(
            &boot,
            "SELECT count(*) FROM objective_events WHERE kind='check_finished'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER crash_done")
        .execute(db.writer_pool())
        .await
        .unwrap();
    evaluate(&boot, NEXT).await.unwrap();
    done(&boot, w, &t, sample()).await.unwrap();
    assert_eq!(
        count(
            &boot,
            "SELECT count(*) FROM objective_events WHERE kind='check_finished'"
        )
        .await,
        1
    );
    assert_eq!(count(&boot, "SELECT count(*) FROM objective_intents WHERE json_extract(settlement_json,'$.settlement')='reported'").await, 1);
    assert!(matches!(
        &state(&boot).await.objectives[&objective::ids::ObjectiveKey::new("kr")].evaluation,
        Some(objective::engine::Evaluation::Running { revision, samples, .. })
            if revision.as_str() == NEXT && samples.values().all(|samples| samples.is_empty())
    ));
}
#[tokio::test]
async fn objective_evaluator_unbound_collision_does_not_consume_request_identity() {
    let boot = fixture().await;
    let seq = count(&boot, "SELECT max(seq) FROM objective_events").await + 1;
    let key = format!("objcheck-{seq}-0");
    let payload = json!({"key":key,"kind":"codex","goal":"ordinary review","access":"read_only","result_contract":"evaluation-v1","ready":true,"declared_by":PLANNER_DECLARATION_AUTHOR});
    let out = crate::support::report_writes::upsert_block(
        &boot.ctx,
        &boot.registry,
        planner_identity(&boot),
        json!({"kind":"task","payload":payload}),
    )
    .await
    .unwrap();
    let before = count(&boot, "SELECT count(*) FROM objective_events").await;
    assert!(evaluate(&boot, REV).await.is_err());
    assert_eq!(
        count(&boot, "SELECT count(*) FROM objective_events").await,
        before
    );
    assert_eq!(
        count(&boot, "SELECT count(*) FROM objective_check_jobs").await,
        0
    );
    let (w, t) = worker(&boot, &key).await;
    done(&boot, w, &t, sample()).await.unwrap();
    assert_eq!(
        count(&boot, "SELECT count(*) FROM objective_events").await,
        before,
        "ordinary evaluation-v1 is not an objective sample"
    );
    // Keep a live unrelated block at the colliding key; every retry is refused atomically.
    assert!(evaluate(&boot, REV).await.is_err());
    assert!(!out["id"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn objective_evaluator_concurrent_retry_uses_task_key_and_report_transaction() {
    let boot = fixture().await;
    let (first, second) = tokio::join!(evaluate(&boot, REV), evaluate(&boot, REV));
    assert!(first.is_ok() || second.is_ok(), "{first:?} {second:?}");
    evaluate(&boot, REV).await.unwrap();
    assert_eq!(tasks(&boot).await.len(), 3);
    assert_eq!(
        count(&boot, "SELECT count(*) FROM objective_check_jobs").await,
        3
    );
    assert_eq!(
        count(
            &boot,
            "SELECT count(*) FROM objective_events WHERE kind='requested'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn objective_evaluator_restarted_planner_repairs_original_binding_identity() {
    let boot = fixture().await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query("CREATE TRIGGER crash_blocks BEFORE UPDATE ON cards WHEN NEW.kind='track-report' BEGIN SELECT RAISE(ABORT,'crash block write'); END").execute(db.writer_pool()).await.unwrap();
    assert!(evaluate(&boot, REV).await.is_err());
    sqlx::query("DROP TRIGGER crash_blocks")
        .execute(db.writer_pool())
        .await
        .unwrap();
    let session = "restarted-planner";
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    calm_server::db::sqlite::session_supersede_and_start_tx(
        &mut tx,
        &PLANNER_SESSION_ID.to_owned(),
        calm_server::session_projection_repo::WorkerSessionInit::shared_planner(
            session.into(),
            boot.planner_card_id.to_string(),
            AgentProvider::Codex,
            WorkerSessionState::Starting,
            None,
            json!({}),
            2,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let edits = rows(&boot, "track.report_edited").await.len();
    let error = evaluate(&boot, REV).await.unwrap_err();
    assert_eq!(error.code, -32403);
    assert!(tasks(&boot).await.is_empty());
    assert_eq!(rows(&boot, "track.report_edited").await.len(), edits);

    let identity = ToolCallIdentity {
        session_id: session.into(),
        ..planner_identity(&boot)
    };
    boot.registry.lookup("neige_objective_evaluate").unwrap()(
        boot.ctx.clone(),
        identity,
        json!({"key":"kr","revision":REV}),
    )
    .await
    .unwrap();
    assert_evaluator_attribution(&boot, session).await;
    assert_eq!(tasks(&boot).await.len(), 3);
    assert_eq!(
        count(&boot, "SELECT count(*) FROM objective_check_jobs").await,
        3
    );
    assert_eq!(
        count(
            &boot,
            "SELECT count(*) FROM objective_events WHERE kind='requested'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn objective_evaluator_tombstone_cannot_be_revived_by_batch() {
    let boot = fixture().await;
    evaluate(&boot, REV).await.unwrap();
    let b = tasks(&boot).await.remove(0);
    crate::support::report_writes::read_then_commit(
        &boot.ctx,
        &boot.registry,
        planner_identity(&boot),
        json!([{"op":"upsert","id":b.id,"kind":"task","payload":{
            "key":b.payload["key"],"declared_by":PLANNER_DECLARATION_AUTHOR,
            "tombstoned_by":PLANNER_DECLARATION_AUTHOR,"tombstone":{"reason":"withdrawn"}
        }}]),
    )
    .await
    .unwrap();
    let before = blocks(&boot).await;
    let edits = rows(&boot, "track.report_edited").await.len();
    let error = crate::support::report_writes::read_then_commit(
        &boot.ctx,
        &boot.registry,
        planner_identity(&boot),
        json!([{"op":"delete","id":b.id},{"op":"upsert","kind":"task","payload":b.payload}]),
    )
    .await
    .expect_err("a batch must not revive a withdrawn evaluator key");
    assert_eq!(error.code, -32403, "{error:?}");
    assert_eq!(blocks(&boot).await, before, "the entire batch rolls back");
    assert_eq!(rows(&boot, "track.report_edited").await.len(), edits);
}

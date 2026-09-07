//! Bounded Planner handoff through authored tasks, native reports and track views.
use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity, worker_identity};
use crate::task_recovery::{current, declare};
use calm_server::db::sqlite::{begin_immediate_tx, card_create_with_id_tx, task_mark_running_tx};
use calm_server::model::{CardRole, NewCard, Task, TaskStatus};
use calm_server::operation::planner_harness_start_adapter::render_planner_developer_instructions_for_test;
use calm_server::operation::task_verify_adapter::TaskVerifyAdapter;
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, SpawnCtx, SqlxOperationRepo,
};
use calm_server::scheduler::{PostClaimDriveTestHook, Scheduler, build_worker_payload};
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_fs_view::{TrackFsError, TrackFsView};
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

async fn start(boot: &Boot, scheduler: &Arc<Scheduler>, task: &Task, worker_card: &str) -> Value {
    // Let the actual scheduler freeze context and emit the dispatch record.
    // Stop at its existing provider seam; this test supplies the worker report.
    let claimed = Arc::new(tokio::sync::Notify::new());
    scheduler.set_post_claim_drive_test_hook(PostClaimDriveTestHook {
        claimed: claimed.clone(),
        resume: Arc::new(tokio::sync::Notify::new()),
    });
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("claim timed out"),
        _ = scheduler.schedule_track(boot.track_id.clone()) => panic!("task was not claimed"),
        _ = claimed.notified() => {}
    }
    let frozen = current(boot, &task.key).await;
    assert_eq!(frozen.status, TaskStatus::Dispatched);
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert_eq!(
        task_mark_running_tx(&mut tx, &task.id, Some(worker_card), 11, 100000)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    build_worker_payload(&frozen).unwrap().1
}

#[tokio::test]
async fn planner_advertised_result_route_reads_recorded_audit() {
    let boot = boot().await;
    let dir = tempfile::tempdir().unwrap();
    let events = boot.ctx.events.clone();
    let operations = Arc::new(SqlxOperationRepo::new(boot.repo.sqlite_pool().unwrap()));
    let completion = OperationCompletionBus::new();
    let spawn = SpawnCtx::new(
        boot.repo.clone(),
        operations.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        events.clone(),
        completion.clone(),
    );
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operations,
        vec![Arc::new(TaskVerifyAdapter::new(dir.path().to_path_buf()))],
        events.clone(),
        completion,
        spawn,
    ));
    let scheduler = Scheduler::new_with_task_budget_default(
        boot.repo.clone(),
        events,
        boot.ctx.write.clone(),
        Arc::downgrade(&runtime),
        Arc::new(tokio::sync::Semaphore::new(1)),
        1,
    );
    let source = r#"{"timeout_secs":-1}"#;
    std::fs::write(dir.path().join("config.json"), source).unwrap();
    declare(&boot, json!({
        "key":"audit", "kind":"codex", "goal":"Audit config.json without changing it; timeout_secs must be positive. Write matching JSON to audit.json and your result: subject (source version), valid (boolean), findings (array with id, field, actual, recommendation for each finding).",
        "acceptance":"Report one finding with actual equal to the observed source value and a valid timeout recommendation. Set valid according to source validity; preserve the source.",
        "ready":true, "declared_by":PLANNER_DECLARATION_AUTHOR,
        "gate":{"cwd":dir.path(), "timeout_secs":5, "steps":[{"name":"audit",
            "cmd":"python3 -c 'import json; a=json.load(open(\"audit.json\")); c=json.load(open(\"config.json\")); assert a[\"findings\"][0][\"actual\"] == c[\"timeout_secs\"] == -1; assert a[\"valid\"] is False; print(\"audit-json-checked\")'"}]}
    })).await;
    let mut downstream = json!({"key":"recommendation", "kind":"codex",
        "goal":"Recommend a timeout from the selected finding; leave the source unchanged.",
        "ready":false, "declared_by":PLANNER_DECLARATION_AUTHOR, "depends_on":["audit"],
        "no_gate_reason":"Recommendation only; no implementation is changed."});
    let (b_block, b_rev) = declare(&boot, downstream.clone()).await;
    let a = current(&boot, "audit").await;
    assert_ne!(a.id, a.key, "execution IDs are opaque, not author keys");
    start(&boot, &scheduler, &a, boot.worker_card_id.as_str()).await;
    let result = json!({
        "subject":"config-v1", "valid":false, "findings":[{
            "id":"F1", "field":"timeout_secs", "actual":-1, "recommendation":30
        }]
    });
    std::fs::write(dir.path().join("audit.json"), result.to_string()).unwrap();
    call_tool(
        &boot,
        "calm.task.complete",
        worker_identity(&boot),
        json!({"idempotency_key":a.id, "result":result}),
    )
    .await
    .unwrap();
    assert_eq!(current(&boot, "audit").await.status, TaskStatus::Verifying);
    scheduler.schedule_track(boot.track_id.clone()).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = current(&boot, "audit").await;
            if task.status != TaskStatus::Verifying {
                assert_eq!(task.status, TaskStatus::Done, "{task:?}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        boot.repo
            .task_current_get(boot.track_id.as_str(), "recommendation")
            .await
            .unwrap()
            .is_none(),
        "A.done must not activate an unready B"
    );

    // Follow the actual production prompt's result-reading instruction. Do not
    // duplicate TrackFsView's path dispatch or synthesize a completion event.
    let prompt = render_planner_developer_instructions_for_test(boot.track_id.as_str(), None, None);
    let advertised = prompt
        .split_once("never assume relative files are shared. Read")
        .expect("gate guidance includes the Planner read route")
        .1
        .split('`')
        .nth(1)
        .unwrap();
    let path = advertised
        .replace("<key>", &a.key)
        .replace("<attempt_id>", &a.id);
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let view = TrackFsView::new(boot.repo.as_ref(), &boot.ctx.write)
        .with_gate_log_access(CardRole::Planner, dir.path().to_path_buf());
    let content = view
        .cat(&track, &path)
        .await
        .unwrap_or_else(|error| panic!("advertised result path {path}: {error:?}"));
    let run: Value = serde_json::from_str(&content.content).unwrap();
    assert_eq!(run["events"]["completed"]["payload"]["result"], result);
    assert!(
        run["verdict"].is_null(),
        "a worker report is not a Planner verdict"
    );

    let gate_events = boot
        .repo
        .events_for_track(boot.track_id.as_str(), &["task.gate_result"], None)
        .await
        .unwrap();
    assert_eq!(gate_events.len(), 1);
    let gate = gate_events[0].event.payload_value();
    assert_eq!(gate["task_id"], a.id);
    assert_eq!(gate["passed"], true);
    let gate_path = prompt
        .split_once("never assume relative files are shared. Read")
        .unwrap()
        .1
        .split('`')
        .nth(3)
        .unwrap()
        .replace("<attempt_id>", &a.id)
        .replace("<N>", &gate["attempt"].to_string());
    assert!(
        view.cat(&track, &gate_path)
            .await
            .unwrap()
            .content
            .contains("audit-json-checked")
    );
    let worker_view = TrackFsView::new(boot.repo.as_ref(), &boot.ctx.write)
        .with_gate_log_access(CardRole::Worker, dir.path().to_path_buf());
    assert!(matches!(
        worker_view.cat(&track, &gate_path).await,
        Err(TrackFsError::Forbidden(_))
    ));

    // A fixture Planner judges an accurate audit, not the invalid configuration.
    let selected = run["events"]["completed"]["payload"]["result"]["findings"][0].clone();
    assert_eq!(selected["actual"], -1);
    let decision = "Accept the accurate audit; recommend 30 seconds without changing config-v1.";
    call_tool(
        &boot,
        "calm.task.verdict",
        planner_identity(&boot),
        json!({"idempotency_key":a.id,"status":"accepted","reason":decision,"message":decision}),
    )
    .await
    .unwrap();
    let judged: Value =
        serde_json::from_str(&view.cat(&track, &path).await.unwrap().content).unwrap();
    assert_eq!(judged["verdict"]["status"], "accepted");
    assert_eq!(judged["events"]["completed"], run["events"]["completed"]);
    assert!(
        boot.repo
            .task_current_get(boot.track_id.as_str(), "recommendation")
            .await
            .unwrap()
            .is_none(),
        "the verdict alone must not activate B"
    );
    let context = json!({"subject":"config-v1", "selected":selected, "decision":decision,
        "source_attempt_id":a.id, "source_event_id":run["events"]["completed"]["event_id"],
        "gate_event_id":gate_events[0].id});
    assert!(context["source_event_id"].as_i64().unwrap() > 0);
    downstream["context"] = context.clone();
    downstream["ready"] = json!(true);
    call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({"id":b_block,"kind":"task","payload":downstream,"if_rev":b_rev}),
    )
    .await
    .unwrap();
    let b = current(&boot, "recommendation").await;
    // Each task has its own bound worker/session, even with one-at-a-time execution.
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        "recommendation-worker".into(),
        NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        },
        CardRole::Worker,
        true,
        &boot.card_role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    crate::mcp_track_report::seed_non_root_session_with_provider(
        boot.repo.as_ref(),
        &boot.track_id,
        &card.id,
        "recommendation-session",
        calm_types::worker::WorkerProviderKind::Codex,
    )
    .await;
    let mut b_identity = worker_identity(&boot);
    b_identity.card_id = card.id.to_string();
    b_identity.session_id = "recommendation-session".into();
    let worker_payload = start(&boot, &scheduler, &b, card.id.as_str()).await;
    assert_eq!(worker_payload["context"], context);
    // B consumes the real operation payload at the fixture provider seam.
    let recommendation = json!({"timeout_secs":worker_payload["context"]["selected"]["recommendation"],
        "source_attempt_id":worker_payload["context"]["source_attempt_id"]});
    call_tool(
        &boot,
        "calm.task.complete",
        b_identity,
        json!({"idempotency_key":b.id,"result":recommendation}),
    )
    .await
    .unwrap();
    assert_eq!(
        current(&boot, "recommendation").await.status,
        TaskStatus::Done
    );
    let b_run: Value = serde_json::from_str(
        &view
            .cat(&track, &format!("runs/{}.json", b.id))
            .await
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        b_run["events"]["completed"]["payload"]["result"]["timeout_secs"],
        30
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.json")).unwrap(),
        source
    );
}

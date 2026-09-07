//! Saved provider evidence read through the production MCP; no provider processes.
use crate::mcp_track_report::{Boot, WORKER_SESSION_ID, boot, call_tool, planner_identity};
use crate::task_recovery::{current, declare};
use calm_server::{
    model::Task,
    operation::{OperationKey, OperationRepo, SqlxOperationRepo, TxOutput},
};
use serde_json::{Value, json};

async fn declared() -> (Boot, Task) {
    let boot = boot().await;
    declare(
        &boot,
        json!({"key":"activity","kind":"codex","goal":"Record activity",
        "ready":true,"declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
        "no_gate_reason":"Report-driven isolated task",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}}),
    )
    .await;
    let task = current(&boot, "activity").await;
    (boot, task)
}
async fn bound() -> (Boot, Task, String) {
    let (boot, task) = declared().await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let payload = serde_json::to_value(calm_server::isolated_codex::worker_payload(&task)).unwrap();
    let op = SqlxOperationRepo::new(pool.clone())
        .insert_operation(
            "codex-isolated-worker",
            OperationKey {
                operation_key: "activity-operation".into(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    // Full historical checkpoint fixture. Private transport fields deliberately contain
    // a sentinel: none of these fields may escape in the activity read projection.
    let request = json!({"identity":{"run_id":op,"attempt_id":task.id,
        "card_id":boot.worker_card_id,"session_id":WORKER_SESSION_ID},
        "workspace":"/private-fixture","developer_instructions":"PRIVATE_SENTINEL"});
    let session = json!({"endpoint":{"version":1,"request":request,
        "home":{"version":1,"run_id":op,"request_digest":"fixture","root":"/private-fixture",
            "home":"/private-fixture/home","control":"/private-fixture/control","socket":"/private-fixture/socket",
            "mcp_source_socket":"/private-fixture/mcp","mcp_device":1,"mcp_inode":1,
            "policy_digest":"fixture","authentication_digest":"PRIVATE_SENTINEL"},
        "boundary":{"run_id":op,"attempt_id":task.id,"config_digest":"fixture",
            "init":{"pid":1,"start_time":1,"boot_id":"fixture","namespace_inode":1}},
        "launch":{"attempt_id":task.id,"network":"isolated","workspace":"/private-fixture",
            "program":"/usr/bin/false","args":[],"environment":{},"mounts":[]}},
        "phase":{"TurnActive":{"thread_id":"activity-thread","turn_id":"activity-turn",
            "request_key":"fixture","prompt_digest":"fixture"}},"stop":"Open"});
    let session: calm_server::dedicated_codex::SessionRecord =
        serde_json::from_value(session).unwrap();
    let mut output = TxOutput::new("card", Some(boot.worker_card_id.to_string()), json!({}));
    output.data = json!({"isolated_execution":{"version":"isolated-run-v1","request":request,
        "track_id":boot.track_id,"native_token":"PRIVATE_SENTINEL","admission":"open",
        "provider":{"state":"prepared","record":session}}});
    sqlx::query(
        "UPDATE operations SET target_type='card',target_id=?1,tx_output_json=?2 WHERE id=?3",
    )
    .bind(boot.worker_card_id.as_str())
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&op)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE id=?2")
        .bind(boot.worker_card_id.as_str())
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET spawn_op_id=?1,thread_id='activity-thread',active_turn_id='activity-turn' WHERE id=?2")
        .bind(&op).bind(WORKER_SESSION_ID).execute(&pool).await.unwrap();
    // Run discovery includes the scheduler's persisted claim, before any terminal result.
    // The generic MCP boot card has a null payload, so it cannot stand in for that event.
    let mut tx = pool.begin().await.unwrap();
    calm_server::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &calm_server::ids::ActorId::KernelDispatcher,
        &calm_server::event::EventScope::Track {
            track: boot.track_id.clone(),
            area: boot.area_id.clone(),
        },
        None,
        &calm_server::event::Event::TaskDispatched {
            idempotency_key: task.id.clone(),
            kind: "codex".into(),
            agent_message: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (boot, task, op)
}
async fn listed(boot: &Boot) -> Value {
    call_tool(boot, "calm.plan.list", planner_identity(boot), json!({}))
        .await
        .unwrap()["tasks"][0]
        .clone()
}
async fn row(boot: &Boot, payload: &Value, captured_at: i64) -> i64 {
    let raw = payload.to_string();
    insert(boot, payload["type"].as_str().unwrap(), &raw, captured_at).await
}
async fn insert(boot: &Boot, kind: &str, payload: &str, captured_at: i64) -> i64 {
    boot.repo
        .worker_flow_item_insert(
            Some(boot.worker_card_id.as_str()),
            Some(WORKER_SESSION_ID),
            Some(boot.track_id.as_str()),
            Some(WORKER_SESSION_ID),
            kind,
            payload,
            captured_at,
        )
        .await
        .unwrap()
}
fn item(kind: &str, seq: u64) -> Value {
    json!({"type":kind,"seq":seq,"turn":1,"session_id":WORKER_SESSION_ID,"provider":"codex",
        "timestamp":1234,"source_uuid":null,"provider_extra":null,"raw_ref":null})
}
fn invocation(seq: u64) -> Value {
    let mut v = item("toolCall", seq);
    v["call_id"] = json!("exec-1");
    v["name"] = json!("exec");
    v["input"] = json!("const r = await tools.write_stdin({session_id: 12}); text(r);");
    v["input_summary"] = json!("provider summary");
    v
}
fn command(seq: u64, status: &str) -> Value {
    let mut v = item("commandExecution", seq);
    v["call_id"] = json!("command-1");
    v["command"] = json!("long-command");
    v["status"] = json!(status);
    v["source"] = json!("agent");
    v["parsed_actions"] = json!([]);
    v["exit_code"] = if status == "inProgress" {
        Value::Null
    } else {
        json!(7)
    };
    v
}
#[tokio::test]
async fn isolated_activity_replay_keeps_source_and_capture_times_and_generic_output() {
    let (boot, task, _) = bound().await;
    let call = row(&boot, &invocation(1), 9000).await;
    let mut result = item("toolResult", 2);
    result["call_id"] = json!("exec-1");
    result["ok"] = json!(true);
    result["output_summary"] = json!("Script completed");
    result["output"] = json!([{"type":"text","text":"Script completed\nWall time: 1s\nACTIVITY_STARTED\nProcess continues with session 12"}]);
    let end = row(&boot, &result, 9001).await;
    let first = listed(&boot).await;
    let a = &first["activity"];
    assert_eq!(a["coverage"], "partial");
    assert_eq!(a["collector_health"], "unknown");
    assert_eq!(a["inspected_row_ids"], json!([call, end]));
    assert!(
        a["recent"][0]["detail"]["summary"]
            .as_str()
            .unwrap()
            .contains("write_stdin")
    );
    assert!(
        a["latest_recorded"]["detail"]["excerpts"][0]
            .as_str()
            .unwrap()
            .contains("ACTIVITY_STARTED")
    );
    assert_eq!(
        a["latest_recorded"]["detail"]["kind"],
        "tool_result_recorded"
    );
    assert!(a["latest_command_end"].is_null());
    assert_eq!(a["latest_recorded"]["source_at_ms"], 1234);
    assert_eq!(a["latest_recorded"]["captured_at_ms"], 9001);
    assert!(a["as_of_ms"].as_i64().unwrap() > 9001);
    assert_eq!(a["binding"]["session_id"], WORKER_SESSION_ID);
    assert_eq!(a["binding"]["conversation"]["scope"], "card");
    assert_eq!(a["binding"]["run"]["scope"], "attempt");
    assert_eq!(
        a["binding"]["run"]["path"],
        format!("runs/{}.json", task.id)
    );
    assert!(!a.to_string().contains("PRIVATE_SENTINEL"));
    for link in ["conversation", "run"] {
        call_tool(
            &boot,
            "calm.track.cat",
            planner_identity(&boot),
            json!({"path":a["binding"][link]["path"]}),
        )
        .await
        .expect("supplied detail link resolves");
    }

    let replay = row(&boot, &invocation(1), 9999).await;
    let second = listed(&boot).await;
    assert_eq!(second["activity"]["latest_recorded"]["row_id"], replay);
    assert_eq!(second["activity"]["latest_recorded"]["source_at_ms"], 1234);
    assert_eq!(
        second["activity"]["latest_recorded"]["captured_at_ms"],
        9999
    );
    assert_eq!(second["recovery"], first["recovery"]);
    assert_eq!(second["status"], first["status"]);
}
#[tokio::test]
async fn isolated_activity_explicit_command_end_is_not_task_outcome() {
    let (boot, _, _) = bound().await;
    row(&boot, &command(1, "inProgress"), 10000).await;
    let before = listed(&boot).await;
    assert_eq!(
        before["activity"]["latest_recorded"]["detail"]["kind"],
        "historical_command_invocation"
    );
    assert!(before["activity"]["latest_command_end"].is_null());
    let end = row(&boot, &command(2, "failed"), 10001).await;
    row(&boot, &invocation(3), 10002).await;
    let after = listed(&boot).await;
    let e = &after["activity"]["latest_command_end"];
    assert_eq!(e["row_id"], end);
    assert_eq!(e["call_id"], "command-1");
    assert_eq!(
        after["activity"]["recent"][0]["detail"]["completion"]["row_id"],
        end
    );
    assert_eq!(e["detail"]["exit_code"], 7);
    assert_eq!(e["detail"]["status"], "failed");
    assert_eq!(after["status"], before["status"]);
    assert_eq!(after["recovery"], before["recovery"]);
}
#[tokio::test]
async fn isolated_activity_empty_unknown_and_truncated_tails_are_explicit() {
    let (boot, _, _) = bound().await;
    let before = listed(&boot).await;
    assert_eq!(before["activity"]["coverage"], "no_recorded_activity");
    let bad = insert(&boot, "toolCall", "{broken", 1).await;
    let unknown = listed(&boot).await;
    assert_eq!(unknown["activity"]["coverage"], "unknown_rows");
    assert_eq!(unknown["activity"]["unknown_row_ids"], json!([bad]));
    for i in 0..33 {
        row(&boot, &invocation(i), 20000 + i as i64).await;
    }
    let tail = listed(&boot).await;
    assert_eq!(tail["activity"]["tail_truncated"], true);
    assert_eq!(
        tail["activity"]["inspected_row_ids"]
            .as_array()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(tail["activity"]["coverage"], "partial");
    assert_eq!(tail["recovery"], before["recovery"]);
    let mut huge = invocation(40);
    huge["input"] = json!("x".repeat(70000));
    let oversized = row(&boot, &huge, 30000).await;
    let mut unknown_time = invocation(41);
    unknown_time["timestamp"] = Value::Null;
    row(&boot, &unknown_time, 30001).await;
    let final_view = listed(&boot).await;
    assert_eq!(
        final_view["activity"]["unknown_row_ids"],
        json!([oversized])
    );
    assert!(final_view["activity"]["latest_recorded"]["source_at_ms"].is_null());
    assert_eq!(final_view["recovery"], before["recovery"]);
}
#[tokio::test]
async fn isolated_activity_foreign_sessions_and_broken_receipts_never_supply_evidence() {
    let (boot, _, op) = bound().await;
    let good = row(&boot, &invocation(1), 1).await;
    let foreign = row(&boot, &invocation(2), 2).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE worker_flow_items SET captured_session_id='foreign-session' WHERE id=?1")
        .bind(foreign)
        .execute(&pool)
        .await
        .unwrap();
    let cross_track = row(&boot, &invocation(3), 3).await;
    sqlx::query("UPDATE worker_flow_items SET track_id='foreign-track' WHERE id=?1")
        .bind(cross_track)
        .execute(&pool)
        .await
        .unwrap();
    let read = listed(&boot).await;
    assert_eq!(read["activity"]["inspected_row_ids"], json!([good]));
    let mut mismatched = invocation(3);
    mismatched["session_id"] = json!("foreign-session");
    let bad = row(&boot, &mismatched, 3).await;
    assert_eq!(
        listed(&boot).await["activity"]["unknown_row_ids"],
        json!([bad])
    );
    sqlx::query("UPDATE worker_sessions SET spawn_op_id=NULL WHERE id=?1")
        .bind(WORKER_SESSION_ID)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        listed(&boot).await["activity"]["coverage"],
        "binding_unavailable"
    );
    sqlx::query("UPDATE worker_sessions SET spawn_op_id=?1 WHERE id=?2")
        .bind(&op)
        .bind(WORKER_SESSION_ID)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.isolated_execution.request.identity.attempt_id','old-attempt') WHERE id=?1")
        .bind(op).execute(&pool).await.unwrap();
    let broken = listed(&boot).await;
    assert_eq!(broken["activity"]["coverage"], "binding_unavailable");
    assert!(broken["activity"]["binding"].is_null());
    assert!(broken["activity"]["recent"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn isolated_activity_replacement_does_not_inherit_predecessor_rows() {
    let (boot, old, _) = bound().await;
    row(&boot, &invocation(1), 1).await;
    assert_eq!(listed(&boot).await["activity"]["coverage"], "partial");
    // Replace the allocation with an unprojected successor, as can occur before scheduling.
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1")
        .bind(&old.id)
        .execute(&pool)
        .await
        .unwrap();
    let closure = calm_server::task_context::TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
    .resolve_task_closure(boot.track_id.as_str(), "activity")
    .await
    .unwrap();
    let origin = calm_types::task_recovery::TaskAttemptOrigin::Recovery {
        previous_attempt_id: old.id.clone(),
        idempotency_key: "replacement-request".into(),
        request_fingerprint: "fixture".into(),
        reason: "Saved successor before projection".into(),
        actor: calm_server::ids::ActorId::User,
        constraint: calm_types::task_recovery::TaskRecoveryConstraint::V1 {
            refs: closure.refs,
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
        },
    };
    sqlx::query("INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES('replacement',?1,'activity',2,?2,2)")
        .bind(boot.track_id.as_str()).bind(serde_json::to_string(&origin).unwrap())
        .execute(&pool).await.unwrap();
    let after = listed(&boot).await;
    assert_eq!(after["attempt_id"], "replacement");
    assert_eq!(after["activity"]["attempt_id"], "replacement");
    assert_eq!(after["activity"]["coverage"], "binding_unavailable");
    assert!(after["activity"]["recent"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn isolated_activity_result_excerpts_retain_tail_and_flag_omissions() {
    let (boot, _, _) = bound().await;
    let mut result = item("toolResult", 1);
    result["call_id"] = json!("wrapper");
    result["ok"] = json!(true);
    result["output"] = json!([
        {"type":"text","text":"earlier discovery"},
        {"type":"text","text":format!("{}\nACTIVITY_STARTED", "metadata ".repeat(300))},
        {"type":"text","text":"write_stdin: process session 12; no new output"}]);
    row(&boot, &result, 9000).await;
    let read = listed(&boot).await;
    let e = &read["activity"]["latest_recorded"];
    assert_eq!(e["summary_truncated"], true);
    assert_eq!(e["detail"]["omitted_blocks"], 1);
    assert_eq!(e["detail"]["excerpts"].as_array().unwrap().len(), 2);
    let first = e["detail"]["excerpts"][0].as_str().unwrap();
    assert!(first.ends_with("ACTIVITY_STARTED"));
    assert!(first.chars().count() <= 640);
    assert!(
        e["detail"]["excerpts"][1]
            .as_str()
            .unwrap()
            .contains("write_stdin")
    );
    assert_eq!(
        read["activity"]["interpretation"],
        "historical_untrusted_worker_evidence"
    );
    assert!(read["activity"]["latest_command_end"].is_null());
}

#[tokio::test]
async fn isolated_activity_terminal_task_unmatched_invocation_stays_historical() {
    let (boot, task, _) = bound().await;
    row(&boot, &command(1, "inProgress"), 1).await;
    row(&boot, &invocation(2), 2).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    // Saved terminal outcome; deliberately no corresponding command-end capture.
    sqlx::query("UPDATE tasks SET status='done',finished_at_ms=2 WHERE id=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET state='exited',completed_at_ms=2 WHERE id=?1")
        .bind(WORKER_SESSION_ID)
        .execute(&pool)
        .await
        .unwrap();
    let read = listed(&boot).await;
    assert_eq!(read["status"], "done");
    assert_eq!(
        read["activity"]["latest_recorded"]["detail"]["kind"],
        "tool_invocation_recorded"
    );
    assert_eq!(
        read["activity"]["interpretation"],
        "historical_untrusted_worker_evidence"
    );
    assert_eq!(
        read["activity"]["recent"][0]["detail"]["kind"],
        "historical_command_invocation"
    );
    assert_eq!(
        read["activity"]["recent"][0]["detail"]["completion"]["kind"],
        "not_observed_in_tail"
    );
    assert!(read["activity"]["latest_command_end"].is_null());
    assert_eq!(read["activity"]["collector_health"], "unknown");
    assert_eq!(read["recovery"]["allowed"], false);
    assert_eq!(
        current(&boot, "activity").await.status,
        calm_server::model::TaskStatus::Done
    );
}

#[tokio::test]
async fn isolated_activity_legacy_backend_is_explicitly_unsupported() {
    let (boot, _, op) = bound().await;
    row(&boot, &invocation(1), 1).await;
    sqlx::query("UPDATE operations SET kind='codex-worker' WHERE id=?1")
        .bind(op)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let read = listed(&boot).await;
    assert_eq!(read["activity"]["coverage"], "unsupported");
    assert!(read["activity"]["recent"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn isolated_activity_declined_invocation_does_not_prove_process_completion() {
    let (boot, _, _) = bound().await;
    row(&boot, &command(1, "declined"), 1).await;
    let read = listed(&boot).await;
    assert_eq!(
        read["activity"]["latest_recorded"]["detail"]["kind"],
        "invocation_declined"
    );
    assert!(read["activity"]["latest_command_end"].is_null());
    assert!(
        read["activity"]["latest_recorded"]["detail"]
            .get("exit_code")
            .is_none()
    );
    assert_eq!(read["recovery"]["allowed"], false);
}

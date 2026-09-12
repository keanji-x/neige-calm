//! Recovery uses actual namespace stop evidence from the runtime-owned fake provider.
use crate::isolated_codex_smoke::{Fixture, fixture};
use crate::task_recovery::current;
use axum::{
    Extension,
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::model::Task;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tower::ServiceExt;

pub(super) async fn rest(
    fx: &Fixture,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = calm_server::routes::protected_router()
        .with_state(fx.state.clone())
        .layer(Extension(crate::task_projection_acceptance::principal()))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "user")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn route(fx: &Fixture, tail: &str) -> String {
    format!("/api/tracks/{}/tasks/retry/{tail}", fx.boot.track_id)
}
async fn operation(fx: &Fixture, task: &Task) -> (String, String, Value) {
    let (id, phase, raw): (String, String, String) = sqlx::query_as(
        "SELECT id,phase,tx_output_json FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
        .bind(&task.id).fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
    (id, phase, serde_json::from_str(&raw).unwrap())
}
async fn launch(fx: &Fixture) -> (Task, PathBuf) {
    let scheduler = fx.state.dispatcher.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    tokio::time::timeout(
        Duration::from_secs(20),
        scheduler.schedule_track(fx.boot.track_id.clone()),
    )
    .await
    .unwrap();
    let task = current(&fx.boot, "retry").await;
    assert_eq!(task.status, calm_server::model::TaskStatus::Running);
    let (_, _, output) = operation(fx, &task).await;
    let workspace = PathBuf::from(
        output["data"]["isolated_execution"]["request"]["workspace"]
            .as_str()
            .unwrap(),
    );
    (task, workspace)
}
async fn finish(fx: &Fixture, task: &Task, workspace: &std::path::Path, success: bool) -> Value {
    std::fs::write(
        workspace.join(if success {
            "report-success"
        } else {
            "report-failure"
        }),
        b"",
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (_, phase, output) = operation(fx, task).await;
            if phase == if success { "succeeded" } else { "failed" } {
                let record = &output["data"]["isolated_execution"];
                assert_eq!(record["admission"], "closed");
                assert!(record["provider"]["record"]["stop"]["Quiesced"].is_object());
                assert_eq!(
                    record["provider"]["record"]["stop"]["Quiesced"]["handle"],
                    record["provider"]["record"]["endpoint"]["boundary"]
                );
                return output;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("real native report and runtime stop must settle")
}
fn recovery(task: &Task) -> Value {
    json!({"expected_attempt_id":task.id,"idempotency_key":"retry-user-request","reason":"Retry the same goal in a new empty workspace."})
}

async fn start_task(fx: &Fixture) {
    let (status, body) = rest(fx, "POST", &format!("/api/tracks/{}/isolated-tasks", fx.boot.track_id),
        json!({"key":"retry","goal":"Write result.txt containing 42 and report completion.","ifDocRev":0})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
async fn stopped_failure() -> (Fixture, Task, Value) {
    let fx = fixture("controlled").await;
    start_task(&fx).await;
    let (task, workspace) = launch(&fx).await;
    let output = finish(&fx, &task, &workspace, false).await;
    (fx, task, output)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_isolated_task_retries_after_actual_stop() {
    let fx = fixture("controlled").await;
    start_task(&fx).await;
    let (first, workspace) = launch(&fx).await;
    let view = rest(&fx, "GET", &route(&fx, "attempts"), Value::Null)
        .await
        .1;
    assert_eq!(view["recovery"]["allowed"], false);
    let output = finish(&fx, &first, &workspace, false).await;
    let card = output["data"]["isolated_execution"]["request"]["identity"]["card_id"]
        .as_str()
        .unwrap();
    let (status, body) = rest(&fx, "DELETE", &format!("/api/cards/{card}"), Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let view = rest(&fx, "GET", &route(&fx, "attempts"), Value::Null)
        .await
        .1;
    assert_eq!(view["recovery"]["allowed"], true, "{view}");
    assert!(
        view["recovery"]["reason"]
            .as_str()
            .unwrap()
            .contains("new empty workspace")
    );
    let (status, receipt) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stopped isolated failure should recover: {receipt}"
    );
    assert_ne!(receipt["attempt_id"], first.id);
    let (status, replay) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, receipt);
    let (second, second_workspace) = launch(&fx).await;
    assert_eq!(second.key, first.key);
    assert_eq!(second.goal, first.goal);
    assert_eq!(second.declared_by, first.declared_by);
    assert_ne!(workspace, second_workspace);
    assert!(!second_workspace.join("report-failure").exists());
    finish(&fx, &second, &second_workspace, true).await;
    let history = rest(&fx, "GET", &route(&fx, "attempts"), Value::Null)
        .await
        .1;
    assert_eq!(history["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(history["attempts"][0]["status"], "failed");
    assert_eq!(history["attempts"][1]["status"], "done");
    let (status, late_replay) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{late_replay}");
    assert_eq!(late_replay, receipt);
    let mut stale = recovery(&first);
    stale["idempotency_key"] = json!("different-stale-request");
    assert_eq!(
        rest(&fx, "POST", &route(&fx, "recover"), stale).await.0,
        StatusCode::CONFLICT
    );
    for (task, kind) in [(&first, "failed"), (&second, "completed")] {
        let (status, report) = rest(
            &fx,
            "GET",
            &route(&fx, &format!("attempts/{}/report", task.id)),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["report"]["kind"], kind);
        if kind == "completed" {
            assert_eq!(report["report"]["result"], json!({"answer":42}));
        }
    }
}

async fn set_output(fx: &Fixture, task: &Task, value: &Value) {
    sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE kind='codex-isolated-worker' AND idempotency_key=?2")
        .bind(value.to_string()).bind(&task.id).execute(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
}
async fn expect_denied(fx: &Fixture, task: &Task, label: &str) {
    let (status, view) = rest(fx, "GET", &route(fx, "attempts"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{label}: {view}");
    assert_eq!(view["recovery"]["allowed"], false, "{label}: {view}");
    let (status, body) = rest(fx, "POST", &route(fx, "recover"), recovery(task)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{label}: {body}");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_attempt_allocations WHERE track_id=?1 AND key='retry'",
    )
    .bind(fx.boot.track_id.as_str())
    .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(count, 1, "{label}: rejection must not allocate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_evidence_corruption_and_ambiguous_operations_refuse_retry() {
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let (fx, task, original) = stopped_failure().await;
    // No unexpected retry may start a provider while adversarial evidence is installed.
    fx.state.dispatcher.semaphore().close();
    let record = "/data/isolated_execution";
    let changes = [
        ("/admission", json!("open")),
        ("/provider", json!({"state":"unprepared"})),
        ("/provider/record/stop", json!("Open")),
        ("/provider/record/stop", json!("Requested")),
        ("/provider/record/stop", Value::Null),
        ("/track_id", json!("foreign")),
        ("/request/identity/run_id", json!("foreign")),
        ("/request/identity/attempt_id", json!("foreign")),
        ("/request/identity/card_id", json!("foreign")),
        ("/request/identity/session_id", json!("foreign")),
        ("/request/workspace", json!("/foreign")),
        (
            "/provider/record/endpoint/request/developer_instructions",
            json!("foreign"),
        ),
        ("/provider/record/endpoint/version", json!(99)),
        (
            "/provider/record/endpoint/launch/attempt_id",
            json!("foreign"),
        ),
        (
            "/provider/record/endpoint/launch/workspace",
            json!("/foreign"),
        ),
        ("/provider/record/endpoint/home/run_id", json!("foreign")),
        (
            "/provider/record/endpoint/home/request_digest",
            json!("foreign"),
        ),
        (
            "/provider/record/endpoint/boundary/run_id",
            json!("foreign"),
        ),
        (
            "/provider/record/endpoint/boundary/attempt_id",
            json!("foreign"),
        ),
        (
            "/provider/record/endpoint/boundary/config_digest",
            json!("foreign"),
        ),
        (
            "/provider/record/stop/Quiesced/handle/init/namespace_inode",
            json!(0),
        ),
        (
            "/provider/record/stop/Quiesced/method",
            json!("leader_exit"),
        ),
        ("/provider/record/stop/Quiesced/observed_at_ms", json!(0)),
    ];
    for (path, value) in changes {
        let mut changed = original.clone();
        *changed
            .pointer_mut(&format!("{record}{path}"))
            .expect("production receipt path") = value;
        set_output(&fx, &task, &changed).await;
        expect_denied(&fx, &task, path).await;
    }
    set_output(&fx, &task, &original).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    for phase in [
        "pending",
        "spawn_started",
        "succeeded",
        "compensating",
        "stuck",
    ] {
        sqlx::query("UPDATE operations SET phase=?1 WHERE idempotency_key=?2")
            .bind(phase)
            .bind(&task.id)
            .execute(&pool)
            .await
            .unwrap();
        expect_denied(&fx, &task, phase).await;
    }
    sqlx::query("UPDATE operations SET phase='failed' WHERE idempotency_key=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    let originals: (String, String) =
        sqlx::query_as("SELECT payload_json,target_id FROM operations WHERE idempotency_key=?1")
            .bind(&task.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    for (path, value) in [
        ("$.track_id", json!("foreign")),
        ("$.task_id", json!("foreign")),
        ("$.actor.kind", json!("User")),
    ] {
        sqlx::query(
            "UPDATE operations SET payload_json=json_set(?1,?2,json(?3)) WHERE idempotency_key=?4",
        )
        .bind(&originals.0)
        .bind(path)
        .bind(value.to_string())
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
        expect_denied(&fx, &task, path).await;
    }
    sqlx::query("UPDATE operations SET payload_json=?1 WHERE idempotency_key=?2")
        .bind(&originals.0)
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET target_id='foreign' WHERE idempotency_key=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    expect_denied(&fx, &task, "operation target").await;
    sqlx::query("UPDATE operations SET target_id=?1 WHERE idempotency_key=?2")
        .bind(&originals.1)
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET compensation_state='{}' WHERE idempotency_key=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    expect_denied(&fx, &task, "compensation").await;
    sqlx::query("UPDATE operations SET compensation_state=NULL WHERE idempotency_key=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    // All operations sharing the execution key, including verification by payload,
    // must be resolved. Use the real Operation writer; never forge event timestamps.
    let ops = SqlxOperationRepo::new(pool.clone());
    for (index, (kind, keyed)) in [("foreign-worker", true), ("task-verify", false)]
        .into_iter()
        .enumerate()
    {
        let payload = json!({"task_id":task.id,"track_id":task.track_id});
        let id = ops
            .insert_operation(
                kind,
                OperationKey {
                    operation_key: format!("conflicting-{index}"),
                    idempotency_key: if keyed { Some(task.id.clone()) } else { None },
                    payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(
                        &payload,
                    )
                    .unwrap(),
                },
                payload,
            )
            .await
            .unwrap();
        expect_denied(&fx, &task, kind).await;
        // Retain the keyed row. Move only this adversarial fixture's correlation
        // away before checking the independent verification-payload case.
        sqlx::query("UPDATE operations SET idempotency_key=NULL,payload_json='{}' WHERE id=?1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let view = rest(&fx, "GET", &route(&fx, "attempts"), Value::Null)
        .await
        .1;
    assert_eq!(view["recovery"]["allowed"], true, "{view}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn successor_claim_and_preparation_recheck_stop_and_refuse_old_callbacks() {
    use calm_server::db::sqlite::{begin_immediate_tx, task_claim_pending_tx};
    use calm_server::operation::{OperationKey, OperationRepo, ProviderAdapter, SqlxOperationRepo};
    let (fx, first, original) = stopped_failure().await;
    fx.state.dispatcher.abort_event_listener_for_test();
    let permits = fx.state.dispatcher.semaphore();
    let held = permits
        .clone()
        .acquire_many_owned(fx.state.dispatcher.permits() as u32)
        .await
        .unwrap();
    let (status, receipt) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let second = current(&fx.boot, "retry").await;
    assert_ne!(first.id, second.id);
    let mut changed = original.clone();
    changed["data"]["isolated_execution"]["provider"]["record"]["stop"] = json!("Requested");
    set_output(&fx, &first, &changed).await;
    drop(held);
    tokio::time::timeout(
        Duration::from_secs(10),
        fx.state
            .dispatcher
            .scheduler()
            .schedule_track(fx.boot.track_id.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        current(&fx.boot, "retry").await.status,
        calm_server::model::TaskStatus::Pending,
        "claim must recheck predecessor stop evidence after allocation"
    );
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM operations WHERE idempotency_key=?1")
        .bind(&second.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "denied claim cannot create a successor Operation");
    // Isolate the post-claim window without starting another provider.
    fx.state.dispatcher.semaphore().close();
    set_output(&fx, &first, &original).await;
    let closure = calm_server::task_context::TaskContextMonitor::new(
        fx.boot.repo.clone(),
        fx.boot.ctx.events.clone(),
        fx.boot.ctx.write.clone(),
    )
    .resolve_task_closure(&second.track_id, &second.key)
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert_eq!(
        task_claim_pending_tx(
            &mut tx,
            &second.id,
            calm_server::model::now_ms(),
            &closure.refs,
            false
        )
        .await
        .unwrap(),
        1
    );
    calm_server::operation::refuse_if_context_stale(&mut tx, Some(&second.id))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let adapter = calm_server::isolated_codex::adapter::IsolatedCodexAdapter::new(
        Some(fx.backend.clone()),
        fx.boot.repo.clone(),
        Some(fx.root.path().join("mcp.sock")),
        fx.boot.ctx.write.clone(),
    );
    let (_, payload) = calm_server::scheduler::build_worker_payload(&second).unwrap();
    let ops = SqlxOperationRepo::new(pool.clone());
    let id = ops
        .insert_operation(
            "codex-isolated-worker",
            OperationKey {
                operation_key: "successor-prepare-check".into(),
                idempotency_key: Some(second.id.clone()),
                payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let op = ops.get_operation(&id).await.unwrap().unwrap();
    set_output(&fx, &first, &changed).await;
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let result = adapter.prepare_tx(&mut tx, &payload, &op).await;
    assert!(result.is_err(), "preparation must recheck stop after claim");
    assert!(result.unwrap_err().to_string().contains("predecessor"));
    tx.rollback().await.unwrap();
    set_output(&fx, &first, &original).await;
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.rollback().await.unwrap();
    let (old_id, _, _) = operation(&fx, &first).await;
    let old_op = ops.get_operation(&old_id).await.unwrap().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert!(
        adapter
            .prepare_tx(&mut tx, &old_op.payload, &old_op)
            .await
            .is_err(),
        "old Operation cannot prepare again"
    );
    tx.rollback().await.unwrap();
    let identity = &original["data"]["isolated_execution"]["request"]["identity"];
    let old_worker = calm_server::mcp_server::ToolCallIdentity {
        card_id: identity["card_id"].as_str().unwrap().into(),
        role: calm_server::model::CardRole::Worker,
        provider: calm_server::session_projection_repo::AgentProvider::Codex,
        session_id: identity["session_id"].as_str().unwrap().into(),
        track_id: Some(first.track_id.clone()),
        area_id: fx.boot.area_id.to_string(),
        thread_id: "obsolete-thread".into(),
    };
    let reported = crate::mcp_track_report::call_tool(
        &fx.boot,
        "calm.task.complete",
        old_worker,
        json!({"idempotency_key":second.id,"result":"late old report","artifacts":[]}),
    )
    .await;
    assert!(
        reported.is_err(),
        "old reporter cannot finish the successor"
    );
    assert_eq!(
        current(&fx.boot, "retry").await.status,
        calm_server::model::TaskStatus::Dispatched
    );
}

#[path = "isolated_task_files.rs"]
mod files;

#[path = "isolated_codex_recovery_wake.rs"]
pub(super) mod recovery_wake;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_plugin_grants_recovery_preserves_frozen_tools() {
    let fx = fixture("controlled").await;
    let grants = json!(["plugin.research_search", "plugin.research_detail"]);
    crate::task_recovery::declare(&fx.boot,json!({"key":"retry","kind":"codex","goal":"Analyze delegated material","declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,"no_gate_reason":"Research fixture",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty","plugin_tools":grants}}})).await;
    let (first, workspace) = launch(&fx).await;
    let original = finish(&fx, &first, &workspace, false).await;
    let (status, receipt) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let (second, workspace) = launch(&fx).await;
    assert_ne!(first.id, second.id);
    assert_eq!(first.context_json, second.context_json);
    assert_eq!(
        serde_json::from_str::<Value>(&second.context_json).unwrap()["neige_execution"]["plugin_tools"],
        grants
    );
    let replacement = finish(&fx, &second, &workspace, true).await;
    for output in [original, replacement] {
        let home =
            output["data"]["isolated_execution"]["provider"]["record"]["endpoint"]["home"]["home"]
                .as_str()
                .unwrap();
        let config: toml_edit::DocumentMut =
            std::fs::read_to_string(std::path::Path::new(home).join("config.toml"))
                .unwrap()
                .parse()
                .unwrap();
        let tools: Vec<_> = config["mcp_servers"]["calm"]["enabled_tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(tools.len(), 6);
        assert!(
            tools.contains(&"plugin.research_search") && tools.contains(&"plugin.research_detail")
        );
    }
}

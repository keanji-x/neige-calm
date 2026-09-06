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

async fn rest(fx: &Fixture, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_isolated_task_retries_after_actual_stop() {
    let fx = fixture("controlled").await;
    let (status, body) = rest(&fx, "POST", &format!("/api/tracks/{}/isolated-tasks", fx.boot.track_id),
        json!({"key":"retry","goal":"Write result.txt containing 42 and report completion.","ifDocRev":0})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (first, workspace) = launch(&fx).await;
    let view = rest(&fx, "GET", &route(&fx, "attempts"), Value::Null)
        .await
        .1;
    assert_eq!(view["recovery"]["allowed"], false);
    finish(&fx, &first, &workspace, false).await;
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

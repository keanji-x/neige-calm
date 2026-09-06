//! Explicit single-task selection through the production report and dispatch codec.
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use crate::task_recovery::{current, declare};
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::scheduler::build_worker_payload;
use serde_json::{Value, json};

fn declaration(key: &str) -> Value {
    json!({"key":key,"kind":"codex","goal":"Create result.txt and report the result.",
        "no_gate_reason":"Single-task report-driven execution; no machine verification.",
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
        "ready":true,"context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}})
}

#[tokio::test]
async fn isolated_codex_authored_context_selects_distinct_operation() {
    let boot = boot().await;
    declare(&boot, declaration("isolated")).await;
    let task = current(&boot, "isolated").await;
    let (kind, payload) = build_worker_payload(&task).unwrap();
    assert_eq!(kind, "codex-isolated-worker");
    assert_eq!(
        payload,
        json!({"version":"isolated-worker-v1","actor":calm_server::ids::ActorId::KernelDispatcher,
        "track_id":boot.track_id,"task_id":task.id,"idempotency_key":task.id})
    );
    let mut legacy = declaration("ordinary");
    legacy["context"] = json!({"ordinary":"context"});
    declare(&boot, legacy).await;
    let task = current(&boot, "ordinary").await;
    assert_eq!(build_worker_payload(&task).unwrap().0, "codex-worker");
}

#[tokio::test]
async fn isolated_codex_authoring_rejects_invalid_and_unsupported_selection() {
    let boot = boot().await;
    let mut cases = Vec::new();
    for tag in [
        Value::Null,
        json!({"version":"future","workspace":"empty"}),
        json!({"version":"isolated-codex-v1","workspace":"repository"}),
        json!({"version":"isolated-codex-v1","workspace":"empty","fallback":true}),
    ] {
        let mut value = declaration("invalid");
        value["context"]["neige_execution"] = tag;
        cases.push(value);
    }
    for (field, value) in [
        ("depends_on", json!(["other"])),
        ("gate", json!({"steps":[{"name":"check","cmd":"true"}]})),
        ("kind", json!("claude")),
        (
            "spawn",
            json!(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE),
        ),
    ] {
        let mut payload = declaration("invalid");
        payload[field] = value;
        cases.push(payload);
    }
    for payload in cases {
        let report = call_tool(
            &boot,
            "calm.report.read",
            planner_identity(&boot),
            json!({}),
        )
        .await
        .unwrap();
        let result = call_tool(
            &boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            planner_identity(&boot),
            json!({"kind":"task","payload":payload,"if_doc_rev":report["docRev"]}),
        )
        .await;
        assert!(
            result.is_err(),
            "unsupported selection must not persist or fall back: {payload}"
        );
    }
    assert!(
        boot.repo
            .tasks_by_track(boot.track_id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn isolated_codex_disabled_backend_keeps_safe_preparation_recovery() {
    let boot = boot().await;
    declare(&boot, declaration("disabled")).await;
    let state = crate::task_projection_acceptance::route_state(&boot).await;
    let scheduler = state.dispatcher.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        scheduler.schedule_track(boot.track_id.clone()),
    )
    .await
    .unwrap();
    let task = current(&boot, "disabled").await;
    assert_eq!(task.status, calm_server::model::TaskStatus::Failed);
    assert!(
        task.status_detail
            .as_deref()
            .unwrap()
            .contains("--isolated-codex-config")
    );
    let (kind, output, artifacts): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT kind,tx_output_json,spawn_artifacts_json FROM operations WHERE idempotency_key=?1",
    )
    .bind(&task.id)
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(kind, "codex-isolated-worker");
    assert!(output.is_none() && artifacts.is_none());
    let view = calm_server::task_recovery::task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        &task.key,
        calm_server::ids::ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap();
    assert!(view.recovery.allowed, "{}", view.recovery.reason);
    let receipt=call_tool(&boot,"calm.plan.recover",planner_identity(&boot),
        json!({"key":task.key,"expected_attempt_id":task.id,"idempotency_key":"enable-and-recover","reason":"Retry preparation after configuring the isolated backend."})).await.unwrap();
    assert_eq!(receipt["key"], task.key);
    assert_ne!(receipt["attempt_id"], task.id);
    assert_eq!(current(&boot, "disabled").await.key, task.key);
}

#[tokio::test]
async fn isolated_selection_does_not_reinterpret_a_recorded_legacy_worker() {
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let boot = boot().await;
    declare(&boot, declaration("recorded")).await;
    let task = current(&boot, "recorded").await;
    // Historical pre-feature Operation: the context was opaque to its legacy producer.
    let payload = serde_json::to_value(
        calm_server::operation::codex_adapter::CodexWorkerOperationPayload {
            actor: calm_server::ids::ActorId::KernelDispatcher,
            track_id: task.track_id.clone(),
            idempotency_key: task.id.clone(),
            goal: task.goal.clone(),
            cwd: None,
            context: serde_json::from_str(&task.context_json).unwrap(),
            acceptance_criteria: task.acceptance_criteria.clone(),
        },
    )
    .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let operations = SqlxOperationRepo::new(pool.clone());
    let op = operations
        .insert_operation(
            "codex-worker",
            OperationKey {
                operation_key: "historical-worker".into(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let output = calm_server::operation::TxOutput::new(
        "card",
        Some(boot.worker_card_id.to_string()),
        json!({"id":boot.worker_card_id}),
    );
    sqlx::query("UPDATE operations SET phase='succeeded',target_type='card',target_id=?1,tx_output_json=?2 WHERE id=?3")
        .bind(boot.worker_card_id.as_str()).bind(serde_json::to_string(&output).unwrap()).bind(&op).execute(&pool).await.unwrap();
    let state = crate::task_projection_acceptance::route_state(&boot).await;
    let scheduler = state.dispatcher.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        scheduler.schedule_track(boot.track_id.clone()),
    )
    .await
    .unwrap();
    let kind: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM operations WHERE idempotency_key=?1")
            .bind(&task.id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(kind, vec!["codex-worker"]);
    let current = current(&boot, "recorded").await;
    assert_eq!(current.status, calm_server::model::TaskStatus::Running);
    assert_eq!(
        current.worker_card_id.as_deref(),
        Some(boot.worker_card_id.as_str())
    );
}

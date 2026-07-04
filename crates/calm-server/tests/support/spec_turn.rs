use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::ids::ActorId;
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::model::{CardRole, new_id};
use calm_server::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use calm_server::operation::{OperationKey, OperationOutcome};
use calm_server::routes::terminal_cards::stable_payload_hash;
use calm_server::session_projection_repo::{AgentProvider, WorkerSessionProjectionRepo};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use super::agent_diag::panic_with_agent_diag;
use super::codex_fixture::{Fixture, PLUGIN_ID, SPEC_SESSION_ID};
use super::event_queries::event_payloads;

pub async fn boot_spec_harness_via_start_op(fx: &Fixture, goal: String) {
    let request = SpecHarnessStartOperationPayload {
        actor: ActorId::Kernel,
        wave_id: fx.wave_id.as_str().to_string(),
        spec_card_id: fx.spec_card_id.clone(),
        report_card_id: None,
        sort: None,
        cwd: fx.wave_cwd.display().to_string(),
        goal: Some(goal),
        reset_harness_items: false,
        force_new_thread: true,
    };
    let payload = serde_json::to_value(&request).expect("spec-harness-start payload");
    let payload_hash = stable_payload_hash(&json!({ "request": &request }))
        .expect("spec-harness-start payload hash");
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(format!(
            "codex-forge-e2e-spec-start:{}:{}",
            fx.wave_id.as_str(),
            fx.spec_card_id.as_str()
        )),
        payload_hash,
    };

    let op_id = fx
        .runtime
        .submit("spec-harness-start", key, payload)
        .await
        .expect("submit spec-harness-start");
    let outcome = fx
        .runtime
        .wait(&op_id)
        .await
        .expect("wait spec-harness-start")
        .outcome;
    match outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {}
        other => panic!("spec-harness-start outcome: {other:?}"),
    }
}

pub fn spec_identity(fx: &Fixture) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: fx.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: SPEC_SESSION_ID.to_string(),
        wave_id: Some(fx.wave_id.as_str().to_string()),
        cove_id: fx.cove_id.as_str().to_string(),
        thread_id: "spec-thread".into(),
    }
}

pub async fn plan_updated_rows(repo: &SqlxRepo) -> Vec<(ActorId, Value)> {
    actor_payload_rows(repo, "plan.updated").await
}

pub async fn lifecycle_changed_rows(repo: &SqlxRepo) -> Vec<(ActorId, Value)> {
    actor_payload_rows(repo, "wave.lifecycle_changed").await
}

pub async fn actor_payload_rows(repo: &SqlxRepo, kind: &str) -> Vec<(ActorId, Value)> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT actor, payload FROM events WHERE kind = ?1 ORDER BY id ASC")
            .bind(kind)
            .fetch_all(repo.pool())
            .await
            .unwrap_or_else(|e| panic!("{kind} event rows: {e}"));
    rows.into_iter()
        .map(|(actor, payload)| {
            (
                serde_json::from_str(&actor).expect("event actor json"),
                serde_json::from_str(&payload).expect("event payload json"),
            )
        })
        .collect()
}

pub async fn wait_for_plan_updated(fx: &Fixture, budget: Duration) -> (ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows = plan_updated_rows(&fx.repo).await;
        if let Some(row) = rows.into_iter().next() {
            return row;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!("timed out after {budget:?} waiting for plan.updated"),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn assert_bound_issue_development_workflow_preconditions(fx: &Fixture) {
    let bound_workflow: Option<String> =
        sqlx::query_scalar("SELECT workflow_id FROM waves WHERE id = ?1")
            .bind(fx.wave_id.as_str())
            .fetch_one(fx.repo.pool())
            .await
            .expect("select bound wave workflow_id");
    assert_eq!(
        bound_workflow.as_deref(),
        Some("issue-development"),
        "wave must be bound to issue-development workflow",
    );

    let registered = event_payloads(&fx.repo, "workflow.registered").await;
    assert!(
        registered.iter().any(|payload| {
            payload["pluginId"] == json!(PLUGIN_ID)
                && payload["workflowId"] == json!("issue-development")
        }),
        "expected workflow.registered for {PLUGIN_ID}/issue-development, got {registered:?}"
    );
}

pub async fn shutdown_spec_harness_if_registered(fx: &Fixture) {
    let Ok(Some(runtime)) = fx
        .repo
        .session_projection_active_for_card(&fx.spec_card_id.to_string())
        .await
    else {
        return;
    };
    if let Some(harness) = fx.harness.remove(&runtime.id) {
        let _ = harness.shutdown().await;
    }
}

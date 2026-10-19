//! Version-one receipt compatibility. New requests use the single handoff workflow.
use super::{OperationReceipt, OperationRuntime};
use crate::error::{CalmError, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ActivationResult {
    AcceptedPending,
    Preparing,
    Blocked {
        reason: String,
        resources_checkpointed: bool,
    },
    ResourceRequestRecorded,
    Unknown {
        reason: String,
    },
    ResourcesCheckpointed {
        thread_id: String,
    },
    NeedsRecovery {
        reason: String,
    },
    Activated {
        session_id: String,
    },
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "explicit legacy recovery; no external ingress")
)]
pub(crate) async fn activate(
    runtime: &OperationRuntime,
    cutover: String,
) -> Result<OperationReceipt> {
    if let Some(state @ ActivationResult::Activated { .. }) =
        super::handoff_legacy::query_activation(runtime, &cutover).await?
    {
        if let Some(id) = super::handoff_legacy::mapped(runtime, &cutover).await? {
            return receipt(runtime, &id).await;
        }
        let old = runtime
            .find_by_kind_and_idempotency("handoff-activation", &cutover)
            .await?
            .ok_or_else(super::handoff_cutover::conflict)?;
        return Ok(OperationReceipt::Settled(super::OperationResult {
            op_id: old.id,
            outcome: super::OperationOutcome::Succeeded {
                result: serde_json::to_value(state)?,
            },
        }));
    }
    if let Some(id) = super::handoff_legacy::mapped(runtime, &cutover).await? {
        return receipt(runtime, &id).await;
    }
    let _drive = runtime
        .drive_mutex
        .try_lock()
        .map_err(|_| CalmError::Conflict("Handoff driver busy".into()))?;
    let id = super::handoff_legacy::adopt_cutover(runtime, &cutover).await?;
    let op = runtime
        .repo
        .get_operation(&id)
        .await?
        .ok_or_else(super::handoff_cutover::conflict)?;
    let key = super::OperationKey {
        operation_key: op.operation_key,
        idempotency_key: op.idempotency_key,
        payload_hash: op.payload_hash,
    };
    drop(_drive);
    runtime
        .retry_interaction(
            super::handoff_coordinator::HANDOFF_COORDINATOR,
            &key,
            op.attempt,
        )
        .await?;
    receipt(runtime, &id).await
}
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "explicit legacy generation recovery")
)]
#[cfg(test)]
pub(crate) async fn retry(
    runtime: &OperationRuntime,
    cutover: String,
    attempt: i32,
) -> Result<OperationReceipt> {
    let id = super::handoff_legacy::mapped(runtime, &cutover)
        .await?
        .ok_or_else(super::handoff_cutover::conflict)?;
    let op = runtime
        .repo
        .get_operation(&id)
        .await?
        .ok_or_else(super::handoff_cutover::conflict)?;
    runtime
        .retry_interaction(
            super::handoff_coordinator::HANDOFF_COORDINATOR,
            &super::OperationKey {
                operation_key: op.operation_key,
                idempotency_key: op.idempotency_key,
                payload_hash: op.payload_hash,
            },
            attempt,
        )
        .await?;
    receipt(runtime, &id).await
}
#[cfg_attr(not(test), expect(dead_code, reason = "read-only legacy receipt"))]
pub(crate) async fn query(runtime: &OperationRuntime, cutover: &str) -> Result<ActivationResult> {
    super::handoff_legacy::query_activation(runtime, cutover)
        .await?
        .ok_or_else(|| CalmError::NotFound("activation intent is not recorded".into()))
}

async fn receipt(runtime: &OperationRuntime, id: &str) -> Result<OperationReceipt> {
    let op = runtime
        .repo
        .get_operation(id)
        .await?
        .ok_or_else(super::handoff_cutover::conflict)?;
    let state = super::handoff_legacy::activation_result(&op)?;
    let terminal = matches!(state, ActivationResult::Activated { .. });
    let result = serde_json::to_value(state)?;
    if terminal {
        return Ok(OperationReceipt::Settled(super::OperationResult {
            op_id: op.id,
            outcome: super::OperationOutcome::Succeeded { result },
        }));
    }
    let mut receipt = super::interaction_retry::receipt_from_operation(op)?;
    match &mut receipt {
        OperationReceipt::AwaitingRetry { output, .. } => output.result = result,
        OperationReceipt::Settled(super::OperationResult {
            outcome: super::OperationOutcome::Succeeded { result: value },
            ..
        }) => *value = result,
        _ => {}
    }
    Ok(receipt)
}

/// Build an upgraded version-one intent fixture using the retained SQL/briefing owners.
/// No resource or success receipt is fabricated, and no legacy adapter is registered.
#[cfg(test)]
pub(crate) async fn record_legacy_intent_for_test(
    repo: &dyn crate::db::Repo,
    cutover: &str,
) -> Result<String> {
    let pool = repo
        .sqlite_db()
        .ok_or_else(super::handoff_cutover::conflict)?;
    let operations = super::SqlxOperationRepo::new(pool.clone());
    let payload = serde_json::json!({"cutover_operation_id":cutover});
    let key = super::OperationKey {
        operation_key: cutover.into(),
        idempotency_key: Some(cutover.into()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload)?,
    };
    use super::OperationRepo;
    let id = operations
        .insert_operation("handoff-activation", key, payload)
        .await?;
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    let plan:super::handoff_cutover::ValidatedCutoverPlan=serde_json::from_str(&sqlx::query_scalar::<_,String>("SELECT payload_json FROM operations WHERE id=?1 AND kind='handoff-cutover' AND phase='succeeded'").bind(cutover).fetch_one(&mut **tx).await?)?;
    let facts = super::handoff_activation_sql::current_facts(
        &mut tx,
        plan.clone(),
        cutover.into(),
        None,
        false,
    )
    .await?;
    sqlx::query(r#"
        INSERT INTO
        planner_handoff_activations(cutover_operation_id,activation_operation_id,session_id,retired_source_session_id,retired_target_session_id)
        VALUES(?1,?2,?3,?4,?5)
        "#)
        .bind(cutover).bind(&id).bind(&plan.scope.new_session_id).bind(&plan.scope.source_session_id).bind(&plan.scope.target_session_id).execute(&mut **tx).await?;
    let briefing = super::handoff_activation_briefing::render(repo, &plan).await?;
    let mut output =
        super::handoff_activation_sql::prepare_snapshot(&mut tx, &facts, briefing).await?;
    output.result = serde_json::to_value(ActivationResult::ResourceRequestRecorded)?;
    sqlx::query(r#"UPDATE operations SET phase='stuck',phase_detail_json='{"reason":"Legacy handoff requires explicit adoption","since":0,"from_phase":"app_server_interact"}',tx_output_json=?1 WHERE id=?2"#)
        .bind(serde_json::to_string(&output)?).bind(&id).execute(&mut **tx).await?;
    tx.commit().await?;
    Ok(id)
}

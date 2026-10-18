//! Bounded version-one receipt reader and explicit, write-once recovery adoption.
//! No version-one journal is driven. Immutable input, audit and resource rows stay intact.
use super::handoff_activation::ActivationResult;
use super::handoff_coordinator::{
    Claim, Claims, CommittedIdentity, CoordinatorInput, CoordinatorResult, HANDOFF_COORDINATOR,
    IdentityOrigin, Progress, Resources, UserHandoffRequest,
};
use super::handoff_cutover::{CutoverResult, ValidatedCutoverPlan, conflict};
use super::{Operation, OperationKey, OperationRuntime, Tx, TxOutput};
use crate::error::Result;
use crate::harness::handoff_pause::{PauseReceipt, PauseRequest, ReleaseOutcome};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InputV1 {
    request: UserHandoffRequest,
    source: PauseRequest,
    target: PauseRequest,
    plan_id: String,
    new_session_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressV1 {
    stage: LegacyStage,
    source_attempted: bool,
    target_attempted: bool,
    source_receipt: Option<PauseReceipt>,
    target_receipt: Option<PauseReceipt>,
    source_release: Option<ReleaseOutcome>,
    target_release: Option<ReleaseOutcome>,
    plan: Option<ValidatedCutoverPlan>,
    committed: Option<CutoverResult>,
    reason: Option<String>,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum LegacyStage {
    Reserved,
    SourceIntent,
    SourcePaused,
    TargetIntent,
    BothPaused,
    PlanRecorded,
    CommitIntent,
    Committed,
    Compensating,
    Finished,
    ReleasedNeedsRecovery,
}
fn claim(
    attempted: bool,
    receipt: Option<PauseReceipt>,
    outcome: Option<ReleaseOutcome>,
) -> Result<Claim> {
    match (attempted, receipt, outcome) {
        (false, None, None) => Ok(Claim::Untouched),
        (true, None, None) => Ok(Claim::Intent),
        (true, Some(receipt), None) => Ok(Claim::Paused { receipt }),
        (true, Some(receipt), Some(outcome)) => Ok(Claim::Released { receipt, outcome }),
        _ => Err(conflict()),
    }
}
async fn row(tx: &mut Tx<'_>, id: &str) -> Result<Operation> {
    let raw = sqlx::query("SELECT * FROM operations WHERE id=?1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    super::repo_sqlite::operation_from_row(&raw)
}
fn inactive(op: &Operation) -> Result<()> {
    if op.lease_owner.is_some()
        && op
            .lease_until_ms
            .is_some_and(|until| until > crate::model::now_ms())
    {
        return Err(conflict());
    }
    Ok(())
}

pub(super) async fn query_coordinator(
    runtime: &OperationRuntime,
    request: &UserHandoffRequest,
) -> Result<CoordinatorResult> {
    let key = request.operation_key()?;
    let id = runtime
        .keyed_replay("handoff-coordinator", &key)
        .await?
        .ok_or_else(conflict)?;
    let op = runtime
        .repo
        .get_operation(&id)
        .await?
        .ok_or_else(conflict)?;
    let _: InputV1 = serde_json::from_value(op.payload)?;
    let Some(output) = op.tx_output else {
        return Ok(CoordinatorResult::NeedsRecovery {
            reason: "Legacy v1 requires explicit adoption".into(),
        });
    };
    let p: ProgressV1 = serde_json::from_value(output.result)?;
    let committed = if p.committed.is_some() {
        p.committed
    } else if p.stage == LegacyStage::CommitIntent {
        super::handoff_cutover::query_recorded(
            &runtime.repo.sqlite_pool(),
            p.plan.as_ref().ok_or_else(conflict)?,
        )
        .await?
    } else {
        None
    };
    if let Some(cutover) = committed {
        let CutoverResult::CutoverCommittedPendingActivation { operation_id, .. } = &cutover;
        let activation = query_activation(runtime, operation_id)
            .await?
            .unwrap_or(ActivationResult::AcceptedPending);
        return Ok(CoordinatorResult::Forward {
            cutover: Box::new(cutover),
            activation,
        });
    }
    if p.stage == LegacyStage::Finished {
        let _settled = super::handoff_coordinator::SettledClaims::try_from(Claims {
            source: claim(p.source_attempted, p.source_receipt, p.source_release)?,
            target: claim(p.target_attempted, p.target_receipt, p.target_release)?,
        })?;
        return Ok(CoordinatorResult::CancelledOrRefused);
    }
    Ok(CoordinatorResult::NeedsRecovery {
        reason: p
            .reason
            .unwrap_or_else(|| "Legacy v1 requires explicit adoption".into()),
    })
}

pub(super) async fn mapped(runtime: &OperationRuntime, legacy: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT owner_operation_id FROM planner_handoff_adoptions WHERE legacy_operation_id=?1",
    )
    .bind(legacy)
    .fetch_optional(&runtime.repo.sqlite_pool())
    .await?)
}
pub(super) async fn query_activation(
    runtime: &OperationRuntime,
    cutover: &str,
) -> Result<Option<ActivationResult>> {
    if let Some(id) = mapped(runtime, cutover).await? {
        let op = runtime
            .repo
            .get_operation(&id)
            .await?
            .ok_or_else(conflict)?;
        return Ok(Some(activation_result(&op)?));
    }
    let Some(op) = runtime
        .find_by_kind_and_idempotency("handoff-activation", cutover)
        .await?
    else {
        return Ok(None);
    };
    // Business terminal precedes runner diagnostics and today's mutable ownership.
    if let Some(output) = &op.tx_output {
        return Ok(Some(serde_json::from_value(output.result.clone())?));
    }
    Ok(Some(ActivationResult::NeedsRecovery {
        reason: op
            .last_error
            .unwrap_or_else(|| "Legacy v1 requires explicit adoption".into()),
    }))
}
pub(super) fn activation_result(op: &Operation) -> Result<ActivationResult> {
    let input: CoordinatorInput = serde_json::from_value(op.payload.clone())?;
    if input.version != 2 {
        return Err(conflict());
    }
    let output = op.tx_output.as_ref().ok_or_else(conflict)?;
    match serde_json::from_value::<Progress>(output.result.clone())? {
        Progress::Activated { .. } => Ok(ActivationResult::Activated {
            session_id: input.new_session_id,
        }),
        Progress::Committed { resources, .. } => Ok(resources.result(op.last_error.as_deref())),
        _ => Err(conflict()),
    }
}

/// Exact original key and generation select a legacy coordinator. No persisted state is live.
pub(super) async fn adopt_coordinator(
    runtime: &OperationRuntime,
    request: &UserHandoffRequest,
    expected: i32,
) -> Result<String> {
    let id = runtime
        .keyed_replay("handoff-coordinator", &request.operation_key()?)
        .await?
        .ok_or_else(conflict)?;
    let pool = runtime.repo.sqlite_pool();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    if let Some(id) = sqlx::query_scalar(
        "SELECT owner_operation_id FROM planner_handoff_adoptions WHERE legacy_operation_id=?1",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?
    {
        return Ok(id);
    }
    let original = row(&mut tx, &id).await?;
    inactive(&original)?;
    if original.attempt != expected {
        return Err(conflict());
    }
    let input: InputV1 = serde_json::from_value(original.payload.clone())?;
    if input.request != *request {
        return Err(conflict());
    }
    let input = CoordinatorInput {
        version: 2,
        request: input.request,
        source: input.source,
        target: input.target,
        plan_id: input.plan_id,
        new_session_id: input.new_session_id,
    };
    let mut output = match original.tx_output {
        Some(output) => output,
        None => TxOutput::new(
            "track",
            Some(request.track_id.to_string()),
            serde_json::Value::Null,
        ),
    };
    let mut cutover = None;
    let state = if output.result.is_null() {
        Progress::Reserved
    } else {
        let p: ProgressV1 = serde_json::from_value(output.result.clone())?;
        if let Some(result) = p.committed {
            let CutoverResult::CutoverCommittedPendingActivation { operation_id, .. } = result;
            cutover = Some(operation_id);
            Progress::Reserved
        } else if p.stage == LegacyStage::CommitIntent {
            let plan = p.plan.ok_or_else(conflict)?;
            cutover = super::handoff_cutover::query_recorded_tx(&mut tx, &plan)
                .await?
                .map(|result| {
                    let CutoverResult::CutoverCommittedPendingActivation { operation_id, .. } =
                        result;
                    operation_id
                });
            Progress::Compensating {
                claims: Claims {
                    source: claim(p.source_attempted, p.source_receipt, p.source_release)?,
                    target: claim(p.target_attempted, p.target_receipt, p.target_release)?,
                },
                reason: "Legacy precommit recovery; original claims only".into(),
            }
        } else {
            let claims = Claims {
                source: claim(p.source_attempted, p.source_receipt, p.source_release)?,
                target: claim(p.target_attempted, p.target_receipt, p.target_release)?,
            };
            if p.stage == LegacyStage::Finished {
                Progress::Finished {
                    claims: claims.try_into()?,
                }
            } else {
                Progress::Compensating {
                    claims,
                    reason: p.reason.unwrap_or_else(|| {
                        "Legacy precommit recovery; original claims only".into()
                    }),
                }
            }
        }
    };
    let (state, children) = if let Some(cutover) = cutover {
        committed_state(&mut tx, &cutover, &input, &mut output).await?
    } else {
        (state, vec![])
    };
    let id = insert_owner(&mut tx, input, output, state, Some(&id), children, expected).await?;
    tx.commit().await?;
    Ok(id)
}

/// Compatibility for a standalone, already committed legacy cutover: recovery only.
pub(super) async fn adopt_cutover(runtime: &OperationRuntime, cutover: &str) -> Result<String> {
    let pool = runtime.repo.sqlite_pool();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    if let Some(id) = sqlx::query_scalar(
        "SELECT owner_operation_id FROM planner_handoff_adoptions WHERE legacy_operation_id=?1",
    )
    .bind(cutover)
    .fetch_optional(&mut *tx)
    .await?
    {
        return Ok(id);
    }
    let old = row(&mut tx, cutover).await?;
    inactive(&old)?;
    if old.kind != "handoff-cutover" || old.tx_output.is_none() {
        return Err(conflict());
    }
    let plan: ValidatedCutoverPlan = serde_json::from_value(old.payload)?;
    let s = &plan.scope;
    let generations: (i64,i64,i64) = sqlx::query_as(r#"
        SELECT s.generation,t.generation,r.generation FROM planner_handoff_card_generations
        s,planner_handoff_card_generations t,planner_handoff_track_generations r WHERE s.card_id=?1 AND
        t.card_id=?2 AND r.track_id=?3
        "#)
        .bind(s.source_card_id.as_str()).bind(s.target_card_id.as_str()).bind(s.track_id.as_str()).fetch_one(&mut *tx).await?;
    let input = CoordinatorInput {
        version: 2,
        request: UserHandoffRequest {
            track_id: s.track_id.clone(),
            source_card_id: s.source_card_id.clone(),
            source_session_id: s.source_session_id.clone(),
            target_card_id: s.target_card_id.clone(),
            target_session_id: s.target_session_id.clone(),
            source_generation: generations.0,
            target_generation: generations.1,
            track_generation: generations.2,
            user_audit_id: s.user_audit_id.clone(),
            actor: crate::ids::ActorId::User,
            key: format!("legacy-cutover:{cutover}"),
        },
        source: s.source_pause.request().clone(),
        target: s.target_pause.request().clone(),
        plan_id: s.plan_id.clone(),
        new_session_id: s.new_session_id.clone(),
    };
    let mut output = TxOutput::new(
        "track",
        Some(s.track_id.to_string()),
        serde_json::Value::Null,
    );
    let (state, children) = committed_state(&mut tx, cutover, &input, &mut output).await?;
    let id = insert_owner(&mut tx, input, output, state, None, children, old.attempt).await?;
    tx.commit().await?;
    Ok(id)
}

async fn committed_state(
    tx: &mut Tx<'_>,
    cutover: &str,
    input: &CoordinatorInput,
    output: &mut TxOutput,
) -> Result<(Progress, Vec<String>)> {
    let old = row(tx, cutover).await?;
    inactive(&old)?;
    if old.kind != "handoff-cutover" || old.tx_output.is_none() {
        return Err(conflict());
    }
    let plan: ValidatedCutoverPlan = serde_json::from_value(old.payload)?;
    let original = old.tx_output.ok_or_else(conflict)?;
    let result: CutoverResult = serde_json::from_value(original.result)?;
    let CutoverResult::CutoverCommittedPendingActivation {
        operation_id,
        push_watermark,
        ..
    } = result.clone();
    if operation_id != cutover
        || plan.scope.source_pause.request() != &input.source
        || plan.scope.target_pause.request() != &input.target
        || plan.scope.new_session_id != input.new_session_id
        || plan.scope.plan_id != input.plan_id
    {
        return Err(conflict());
    }
    let identity = CommittedIdentity {
        origin: IdentityOrigin::Legacy {
            cutover_operation_id: cutover.into(),
        },
        configuration: plan.configuration,
        source_pause: plan.scope.source_pause,
        target_pause: plan.scope.target_pause,
        push_watermark,
    };
    if identity.result(input, cutover) != result {
        return Err(conflict());
    }
    let raw = sqlx::query(
        "SELECT * FROM operations WHERE kind='handoff-activation' AND idempotency_key=?1",
    )
    .bind(cutover)
    .fetch_optional(&mut **tx)
    .await?;
    let mut children = vec![cutover.into()];
    let mut resources = Resources::Unprepared;
    if let Some(raw) = raw {
        let activation = super::repo_sqlite::operation_from_row(&raw)?;
        inactive(&activation)?;
        children.push(activation.id);
        if let Some(previous) = activation.tx_output {
            let state: ActivationResult = serde_json::from_value(previous.result.clone())?;
            *output = previous;
            if matches!(state, ActivationResult::Activated { .. }) {
                return Ok((Progress::Activated { identity }, children));
            }
            resources = match state {
                ActivationResult::ResourceRequestRecorded | ActivationResult::Unknown { .. } => {
                    Resources::Unknown {
                        reason: "Legacy native result unconfirmed; never resend".into(),
                    }
                }
                _ if output.data["codex_thread_id"].is_string() => Resources::Proven {
                    thread_id: output.data["codex_thread_id"]
                        .as_str()
                        .ok_or_else(conflict)?
                        .into(),
                },
                _ => Resources::Prepared,
            };
        }
    }
    let legacy_activation: Option<String>=sqlx::query_scalar("SELECT activation_operation_id FROM planner_handoff_activations WHERE cutover_operation_id=?1").bind(cutover).fetch_optional(&mut **tx).await?;
    super::handoff_activation_sql::current_facts(
        tx,
        identity.plan(input),
        cutover.into(),
        legacy_activation.as_deref(),
        false,
    )
    .await?;
    Ok((
        Progress::Committed {
            identity,
            resources,
        },
        children,
    ))
}

async fn insert_owner(
    tx: &mut Tx<'_>,
    input: CoordinatorInput,
    mut output: TxOutput,
    state: Progress,
    reservation: Option<&str>,
    mut children: Vec<String>,
    attempt: i32,
) -> Result<String> {
    let id = crate::model::new_id();
    let mut key: OperationKey = input.request.operation_key()?;
    // operation_key is globally unique; the user's original keyed receipt remains the
    // idempotency key. This Neige-owned identity names only the one adoption owner.
    key.operation_key = format!("handoff-adoption:{id}");
    super::repo_sqlite::insert_pending_row(
        &id,
        HANDOFF_COORDINATOR,
        &key,
        &serde_json::to_value(&input)?,
    )?
    .execute(&mut **tx)
    .await?;
    output.result = serde_json::to_value(&state)?;
    let (_, detail) = super::Phase::AwaitingRetry {
        kind: super::handoff_coordinator_adapter::continuation(),
    }
    .serialize_split();
    sqlx::query("UPDATE operations SET phase='awaiting_retry',phase_detail_json=?1,tx_output_json=?2,attempt=?3 WHERE id=?4")
        .bind(detail.map(|v|v.to_string())).bind(serde_json::to_string(&output)?).bind(attempt).bind(&id).execute(&mut **tx).await?;
    if let Some(old) = reservation {
        children.push(old.into());
    }
    for legacy in children {
        let original = row(tx, &legacy).await?;
        sqlx::query("INSERT INTO planner_handoff_adoptions(legacy_operation_id,owner_operation_id,legacy_attempt) VALUES(?1,?2,?3)")
            .bind(&legacy).bind(&id).bind(original.attempt).execute(&mut **tx).await?;
    }
    let prior_reservation = if let Some(old) = reservation {
        Some((
            old,
            sqlx::query_scalar::<_, bool>(
                "SELECT active FROM planner_handoff_coordinators WHERE operation_id=?1",
            )
            .bind(old)
            .fetch_one(&mut **tx)
            .await?,
        ))
    } else {
        None
    };
    match prior_reservation {
        Some((old, active)) if active || state.terminal() => {
            sqlx::query(
                "UPDATE planner_handoff_coordinators SET operation_id=?1 WHERE operation_id=?2",
            )
            .bind(&id)
            .bind(old)
            .execute(&mut **tx)
            .await?;
        }
        Some((old, _)) => {
            sqlx::query(
                r#"
        INSERT INTO planner_handoff_coordinators(operation_id,track_id,cancel_requested) SELECT
        ?1,track_id,cancel_requested FROM planner_handoff_coordinators WHERE operation_id=?2
        "#,
            )
            .bind(&id)
            .bind(old)
            .execute(&mut **tx)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO planner_handoff_coordinators(operation_id,track_id) VALUES(?1,?2)",
            )
            .bind(&id)
            .bind(input.request.track_id.as_str())
            .execute(&mut **tx)
            .await?;
        }
    }
    if let Some(cutover) = match &state {
        Progress::Committed {
            identity,
            resources,
        } if !matches!(resources, Resources::Unprepared) => Some(identity.evidence_owner(&id)),
        Progress::Activated { identity } => Some(identity.evidence_owner(&id)),
        _ => None,
    } {
        let copied=sqlx::query(r#"
        INSERT INTO
        planner_handoff_resources(operation_id,session_id,retired_source_session_id,retired_target_session_id,thread_id,token_hash)
        SELECT ?1,session_id,retired_source_session_id,retired_target_session_id,thread_id,token_hash
        FROM planner_handoff_activations WHERE cutover_operation_id=?2
        "#)
            .bind(&id).bind(cutover).execute(&mut **tx).await?.rows_affected();
        if copied != 1 {
            return Err(conflict());
        }
    }
    if state.terminal() {
        sqlx::query("UPDATE planner_handoff_coordinators SET active=0 WHERE operation_id=?1")
            .bind(&id)
            .execute(&mut **tx)
            .await?;
        let active: bool = sqlx::query_scalar(
            "SELECT active FROM planner_handoff_coordinators WHERE operation_id=?1",
        )
        .bind(&id)
        .fetch_one(&mut **tx)
        .await?;
        if active {
            return Err(conflict());
        }
    }
    Ok(id)
}

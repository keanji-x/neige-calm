//! Short SQL admission and journal ownership, never provider I/O.
use super::handoff_coordinator::{
    CoordinatorInput, HANDOFF_COORDINATOR, Progress, UserHandoffRequest,
};
use super::handoff_cutover::conflict;
use super::{Operation, Tx, TxOutput};
use crate::error::Result;
use crate::harness::handoff_pause::PauseRequest;

pub(super) async fn admit(
    pool: &crate::db::sqlite::SqliteDb,
    request: UserHandoffRequest,
) -> Result<super::handoff_coordinator::Admission> {
    let key = request.operation_key()?;
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    if let Some((id, hash)) = sqlx::query_as::<_, (String, String)>(
        "SELECT id,payload_hash FROM operations WHERE kind=?1 AND idempotency_key=?2",
    )
    .bind(HANDOFF_COORDINATOR)
    .bind(&key.idempotency_key)
    .fetch_optional(&mut **tx)
    .await?
    {
        if hash != key.payload_hash {
            return Err(conflict());
        }
        tx.rollback().await?;
        return Ok(super::handoff_coordinator::Admission::Replay(id));
    }
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM planner_handoff_coordinators WHERE track_id=?1 AND active=1)",
    )
    .bind(request.track_id.as_str())
    .fetch_one(&mut **tx)
    .await?;
    if busy {
        return Err(conflict());
    }
    super::handoff_cutover_sql::binding_facts(
        &mut tx,
        &request.track_id,
        &request.source_card_id,
        &request.source_session_id,
        &request.target_card_id,
        &request.target_session_id,
    )
    .await?;
    validate_generation(&mut tx, &request).await?;
    for (card, session, contract) in [
        (
            &request.source_card_id,
            request.source_session_id.as_str(),
            calm_types::worker::WorkerContract::Planner,
        ),
        (
            &request.target_card_id,
            request.target_session_id.as_str(),
            calm_types::worker::WorkerContract::Assistant,
        ),
    ] {
        let snapshot = super::handoff_cutover_sql::idle_snapshot(
            &mut tx,
            card,
            session,
            &request.track_id,
            contract,
        )
        .await?;
        if snapshot.handoff_pause.is_paused()
            || matches!(
                snapshot.handoff_pause,
                crate::harness::handoff_pause::PauseState::ConsumedByHandoff { .. }
            )
        {
            return Err(conflict());
        }
    }
    let input = CoordinatorInput {
        version: 2,
        source: PauseRequest::new(
            request.source_card_id.clone(),
            request.source_session_id.clone(),
            request.track_id.clone(),
        ),
        target: PauseRequest::new(
            request.target_card_id.clone(),
            request.target_session_id.clone(),
            request.track_id.clone(),
        ),
        request: request.clone(),
        plan_id: crate::model::new_id(),
        new_session_id: crate::model::new_id(),
    };
    let id = crate::model::new_id();
    super::repo_sqlite::insert_pending_row(
        &id,
        HANDOFF_COORDINATOR,
        &key,
        &serde_json::to_value(input)?,
    )?
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO planner_handoff_coordinators(operation_id,track_id) VALUES(?1,?2)")
        .bind(&id)
        .bind(request.track_id.as_str())
        .execute(&mut **tx)
        .await?;
    tx.commit().await?;
    Ok(super::handoff_coordinator::Admission::Original(id))
}

pub(super) async fn validate_reservation(
    tx: &mut Tx<'_>,
    op: &Operation,
    input: &CoordinatorInput,
) -> Result<()> {
    input.request.validate()?;
    if input.version != 2 {
        return Err(conflict());
    }
    let valid: bool = sqlx::query_scalar(
        r#"
            SELECT EXISTS(SELECT 1 FROM planner_handoff_coordinators WHERE operation_id=?1 AND
            track_id=?2 AND active=1)
        "#,
    )
    .bind(&op.id)
    .bind(input.request.track_id.as_str())
    .fetch_one(&mut **tx)
    .await?;
    if !valid
        || op.payload_hash != input.request.operation_key()?.payload_hash
        || !input.source.matches_binding(
            &input.request.source_card_id,
            &input.request.source_session_id,
            &input.request.track_id,
        )
        || !input.target.matches_binding(
            &input.request.target_card_id,
            &input.request.target_session_id,
            &input.request.track_id,
        )
        || input.plan_id.is_empty()
        || input.new_session_id.is_empty()
    {
        return Err(conflict());
    }
    Ok(())
}

pub(super) async fn cancelled(pool: &sqlx::SqlitePool, op: &Operation) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT cancel_requested FROM planner_handoff_coordinators WHERE operation_id=?1",
    )
    .bind(&op.id)
    .fetch_one(pool)
    .await?)
}

pub(super) fn output(input: &CoordinatorInput) -> Result<TxOutput> {
    Ok(TxOutput::new(
        "track",
        Some(input.request.track_id.to_string()),
        serde_json::to_value(Progress::reserved())?,
    ))
}

/// Reused at initial admission and the final cutover transaction; no timestamp inference.
pub(super) async fn validate_generation(
    tx: &mut Tx<'_>,
    request: &UserHandoffRequest,
) -> Result<()> {
    let actual: Option<(i64, i64, i64)> = sqlx::query_as(
        r#"
            SELECT s.generation,t.generation,r.generation FROM planner_handoff_card_generations
            s,planner_handoff_card_generations t,planner_handoff_track_generations r WHERE
            s.card_id=?1 AND t.card_id=?2 AND r.track_id=?3
        "#,
    )
    .bind(request.source_card_id.as_str())
    .bind(request.target_card_id.as_str())
    .bind(request.track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    if actual
        != Some((
            request.source_generation,
            request.target_generation,
            request.track_generation,
        ))
    {
        return Err(conflict());
    }
    Ok(())
}

/// An active reservation permits only its original persisted, uncancelled commit intent.
/// Existing standalone cutover remains available when no coordinator owns the Track.
pub(super) async fn validate_cutover_intent(
    tx: &mut Tx<'_>,
    plan: &super::handoff_cutover::ValidatedCutoverPlan,
) -> Result<()> {
    let row: Option<(String, String, bool)> = sqlx::query_as(
        r#"
            SELECT o.payload_json,o.tx_output_json,c.cancel_requested FROM
            planner_handoff_coordinators c JOIN operations o ON o.id=c.operation_id WHERE
            c.track_id=?1 AND c.active=1 AND o.kind='handoff-workflow'
        "#,
    )
    .bind(plan.scope.track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some((input, output, cancelled)) = row else {
        return Ok(());
    };
    let input: CoordinatorInput = serde_json::from_str(&input)?;
    let output: TxOutput = serde_json::from_str(&output)?;
    let progress: Progress = serde_json::from_value(output.result)?;
    if cancelled
        || !matches!(progress, Progress::Pausing { .. })
        || input.plan_id != plan.scope.plan_id
        || input.new_session_id != plan.scope.new_session_id
        || input.source != *plan.scope.source_pause.request()
        || input.target != *plan.scope.target_pause.request()
        || input.request.user_audit_id != plan.scope.user_audit_id
    {
        return Err(conflict());
    }
    validate_generation(tx, &input.request).await
}

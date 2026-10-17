//! Claim validation and writes owned by the operations repository.
use serde_json::{Value, json};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use super::{SqlxOperationRepo, operation_from_row, optional_json_text};
use crate::{
    db::sqlite::begin_immediate_tx,
    error::{CalmError, Result},
    model::now_ms,
    operation::{
        AppServerInteractKind, Operation, OperationId, OperationOutcome, OperationResult,
        ParkedCompletion, ParkedOutcome, Phase, PhaseTag, SpawnArtifacts, TimestampMs, Tx,
        TxOutput, required_lease_owner, required_output,
    },
};

/// Original accepted inputs remain the authority for control and recovery,
/// independently of the operation's current phase or lease.
pub(crate) async fn require_original_operation_tx(
    tx: &mut Tx<'_>,
    original: &Operation,
) -> Result<()> {
    let current: Option<i64> = original_operation_query(original)?
        .build_query_scalar()
        .fetch_optional(&mut **tx)
        .await?;
    if current.is_none() {
        return Err(CalmError::Conflict(
            "original operation inputs changed".into(),
        ));
    }
    Ok(())
}

fn original_operation_query(original: &Operation) -> Result<QueryBuilder<'static, Sqlite>> {
    let mut query = QueryBuilder::new("SELECT 1 FROM operations WHERE id=");
    query
        .push_bind(original.id.clone())
        .push(" AND operation_key=")
        .push_bind(original.operation_key.clone())
        .push(" AND kind=")
        .push_bind(original.kind.clone())
        .push(" AND idempotency_key IS ")
        .push_bind(original.idempotency_key.clone())
        .push(" AND payload_hash=")
        .push_bind(original.payload_hash.clone())
        .push(" AND payload_json=")
        .push_bind(serde_json::to_string(&original.payload)?);
    Ok(query)
}

/// The generic kernel owns the phase and live claim. Provider checkpoints may
/// change phase detail and owned output within that phase, so neither is a
/// frozen row-wide claim snapshot.
pub(super) async fn has_live_claim_tx(tx: &mut Tx<'_>, original: &Operation) -> Result<bool> {
    let mut query = original_operation_query(original)?;
    query
        .push(" AND phase=")
        .push_bind(original.phase.tag().as_str())
        .push(" AND lease_owner=")
        .push_bind(required_lease_owner(original)?.to_owned())
        .push(" AND lease_until_ms>=")
        .push_bind(now_ms())
        .push(" AND completed_at_ms IS NULL");
    Ok(query
        .build_query_scalar::<i64>()
        .fetch_optional(&mut **tx)
        .await?
        .is_some())
}

/// One predicate for effect reservation and all claimed operation writes.
pub(crate) async fn require_live_claim_tx(tx: &mut Tx<'_>, original: &Operation) -> Result<()> {
    if !has_live_claim_tx(tx, original).await? {
        return Err(CalmError::Conflict(
            "operation no longer owns its original live claim".into(),
        ));
    }
    Ok(())
}

pub(in crate::operation) async fn record_spawn_artifacts(
    pool: &SqlitePool,
    op: &Operation,
    artifacts: &SpawnArtifacts,
) -> Result<()> {
    let mut tx = begin_immediate_tx(pool).await?;
    require_live_claim_tx(&mut tx, op).await?;
    if !matches!(op.phase, Phase::SpawnStarted) {
        return Err(CalmError::Conflict(
            "spawn artifacts require spawn_started".into(),
        ));
    }
    sqlx::query("UPDATE operations SET spawn_artifacts_json=?1, updated_at_ms=?2 WHERE id=?3")
        .bind(serde_json::to_string(artifacts)?)
        .bind(now_ms())
        .bind(&op.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub(in crate::operation) async fn set_parked(
    pool: &SqlitePool,
    op: &Operation,
    deadline_ms: TimestampMs,
) -> Result<Option<Operation>> {
    let mut tx = begin_immediate_tx(pool).await?;
    if !matches!(op.phase, Phase::SpawnStarted) || !has_live_claim_tx(&mut tx, op).await? {
        return Ok(None);
    }
    let now = now_ms();
    let result = sqlx::query(
        "UPDATE operations SET phase='parked', phase_detail_json=NULL, parked_at_ms=?1, \
         parked_deadline_ms=?2, lease_owner=NULL, lease_until_ms=NULL, updated_at_ms=?1 \
         WHERE id=?3 AND spawn_artifacts_json IS NOT NULL",
    )
    .bind(now)
    .bind(deadline_ms)
    .bind(&op.id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    tx.commit().await?;
    SqlxOperationRepo::new(pool.clone())
        .find_by_id(&op.id)
        .await
}

pub(crate) async fn checkpoint_app_server_interact_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    kind: AppServerInteractKind,
    expected_output: &TxOutput,
    output: &TxOutput,
) -> Result<()> {
    require_live_claim_tx(tx, op).await?;
    if !matches!(op.phase, Phase::AppServerInteract { .. })
        || output_value(read_output_tx(tx, &op.id).await?.as_ref())?
            != output_value(Some(expected_output))?
    {
        return Err(CalmError::Conflict("operation checkpoint changed".into()));
    }
    let phase = Phase::AppServerInteract { kind };
    let (_tag, detail) = phase.serialize_split();
    let detail_text = optional_json_text(detail.as_ref())?;
    let mut output_for_db = output.clone();
    output_for_db.post_commit_events.clear();
    let result = sqlx::query(
        r#"UPDATE operations
           SET phase_detail_json = ?1,
               tx_output_json = CASE
                   WHEN json_type(tx_output_json, '$.data.terminal_launch') = 'object'
                   THEN json_set(?2, '$.data.terminal_launch', json_extract(tx_output_json, '$.data.terminal_launch'))
                   ELSE ?2 END,
               target_type = ?3,
               target_id = ?4,
               target_json = ?5,
               updated_at_ms = ?6
           WHERE id = ?7
             AND phase = 'app_server_interact'
             AND lease_owner = ?8"#,
    )
    .bind(detail_text)
    .bind(serde_json::to_string(&output_for_db)?)
    .bind(&output.target_type)
    .bind(&output.target_id)
    .bind(serde_json::to_string(&json!({
        "type": output.target_type,
        "id": output.target_id,
    }))?)
    .bind(now_ms())
    .bind(&op.id)
    .bind(required_lease_owner(op)?)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(CalmError::Internal(format!(
            "operation {} lost lease while checkpointing app_server_interact",
            op.id
        )));
    }
    Ok(())
}

/// The only unleased writer is the row just inserted by `commit_tx_only`
/// inside its owning transaction. This cannot be selected as a failed-claim
/// fallback.
pub(in crate::operation) enum PhaseWrite<'a> {
    Claimed(&'a Operation),
    Inserted(&'a Operation),
}

pub(in crate::operation) async fn write_phase_and_tx_output(
    tx: &mut Tx<'_>,
    authority: PhaseWrite<'_>,
    phase: &Phase,
    output: &TxOutput,
) -> Result<bool> {
    let op = match authority {
        PhaseWrite::Claimed(op) => {
            if !has_live_claim_tx(tx, op).await?
                || !settlement_output_matches_tx(tx, op, output).await?
            {
                return Ok(false);
            }
            op
        }
        PhaseWrite::Inserted(op) => {
            super::require_original_operation_tx(tx, op).await?;
            if op.lease_owner.is_some() || !matches!(op.phase, Phase::Pending) {
                return Err(CalmError::Conflict(
                    "inserted operation is already claimed".into(),
                ));
            }
            let unleased: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND phase='pending' \
                 AND lease_owner IS NULL AND lease_until_ms IS NULL AND completed_at_ms IS NULL)",
            )
            .bind(&op.id)
            .fetch_one(&mut **tx)
            .await?;
            if !unleased {
                return Ok(false);
            }
            op
        }
    };
    if matches!(phase, Phase::AwaitingRetry { .. })
        && !matches!(op.phase, Phase::AppServerInteract { .. })
    {
        return Ok(false);
    }
    let (tag, detail) = phase.serialize_split();
    let now = now_ms();
    let completed_at = matches!(
        phase,
        Phase::Succeeded | Phase::Failed | Phase::Stuck { .. }
    )
    .then_some(now);
    let mut output_for_db = output.clone();
    output_for_db.post_commit_events.clear();
    let result = sqlx::query(
        r#"UPDATE operations
           SET phase = ?1,
               phase_detail_json = ?2,
               tx_output_json = CASE
                       WHEN json_type(tx_output_json, '$.data.terminal_launch') = 'object'
                       THEN json_set(?3, '$.data.terminal_launch', json_extract(tx_output_json, '$.data.terminal_launch'))
                       ELSE ?3 END,
               target_type = ?4,
               target_id = ?5,
               target_json = ?6,
               lease_owner = NULL,
               lease_until_ms = NULL,
               completed_at_ms = COALESCE(?7, completed_at_ms),
               updated_at_ms = ?8
           WHERE id = ?9
"#,
    )
    .bind(tag.as_str())
    .bind(optional_json_text(detail.as_ref())?)
    .bind(serde_json::to_string(&output_for_db)?)
    .bind(&output.target_type)
    .bind(&output.target_id)
    .bind(serde_json::to_string(&json!({
        "type": output.target_type,
        "id": output.target_id,
    }))?)
    .bind(completed_at)
    .bind(now)
    .bind(&op.id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() > 0)
}

async fn read_output_tx(tx: &mut Tx<'_>, id: &str) -> Result<Option<TxOutput>> {
    let text: Option<String> =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    text.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(Into::into)
}

/// Compare the two explicit values a callback may legitimately have committed:
/// its original output, or the output it returned after its own checkpoint.
/// A third committed value is never overwritten. Events are skipped by serde;
/// only the already-released terminal launch field has independent ownership.
pub(super) async fn settlement_output_matches_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    output: &TxOutput,
) -> Result<bool> {
    let current = output_value(read_output_tx(tx, &op.id).await?.as_ref())?;
    Ok(current == output_value(op.tx_output.as_ref())? || current == output_value(Some(output))?)
}

fn output_value(output: Option<&TxOutput>) -> Result<Option<Value>> {
    output
        .map(|output| {
            let mut value = serde_json::to_value(output)?;
            if let Some(data) = value.get_mut("data").and_then(Value::as_object_mut) {
                data.remove("terminal_launch");
            }
            Ok(value)
        })
        .transpose()
}

pub(crate) async fn complete_parked_tx(
    tx: &mut Tx<'_>,
    op_id: &OperationId,
    outcome: &ParkedOutcome,
) -> Result<ParkedCompletion> {
    let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Err(CalmError::NotFound(format!("operation {op_id} not found")));
    };
    let op = operation_from_row(&row)?;
    if !matches!(op.phase, Phase::Parked) {
        return Ok(ParkedCompletion::AlreadyResolved {
            phase: op.phase.tag(),
        });
    }

    let mut output = required_output(&op)?.clone();
    output.post_commit_events.clear();
    let (phase, phase_detail, last_error) = match outcome {
        ParkedOutcome::Succeeded { result } => {
            output.result = result.clone();
            ("succeeded", None, None)
        }
        ParkedOutcome::Failed {
            last_error,
            last_error_class,
        } => (
            "failed",
            Some(serde_json::to_string(&json!({
                "from_phase": PhaseTag::Parked,
                "last_error_class": last_error_class,
            }))?),
            Some(last_error.clone()),
        ),
    };
    let output_text = serde_json::to_string(&output)?;
    let now = now_ms();
    let result = sqlx::query(
        r#"UPDATE operations
           SET phase = ?1,
               phase_detail_json = ?2,
               tx_output_json = ?3,
               last_error = ?4,
               lease_owner = NULL,
               lease_until_ms = NULL,
               parked_deadline_ms = NULL,
               completed_at_ms = ?5,
               updated_at_ms = ?5
           WHERE id = ?6
             AND phase = 'parked'"#,
    )
    .bind(phase)
    .bind(phase_detail)
    .bind(output_text)
    .bind(last_error.clone())
    .bind(now)
    .bind(op_id)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        let phase = sqlx::query_scalar::<_, String>("SELECT phase FROM operations WHERE id = ?1")
            .bind(op_id)
            .fetch_one(&mut **tx)
            .await?;
        return Ok(ParkedCompletion::AlreadyResolved {
            phase: PhaseTag::from_db_str(&phase)?,
        });
    }

    let completed = match outcome {
        ParkedOutcome::Succeeded { result } => OperationResult {
            op_id: op_id.clone(),
            outcome: OperationOutcome::Succeeded {
                result: result.clone(),
            },
        },
        ParkedOutcome::Failed {
            last_error,
            last_error_class,
        } => OperationResult {
            op_id: op_id.clone(),
            outcome: OperationOutcome::Failed {
                last_error: last_error.clone(),
                from_phase: PhaseTag::Parked,
                last_error_class: last_error_class.clone(),
            },
        },
    };
    Ok(ParkedCompletion::Completed(completed))
}

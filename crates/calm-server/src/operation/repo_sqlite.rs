use crate::db::sqlite::SqliteDb;
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{Row, Sqlite};

use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::model::{new_id, now_ms};

use super::{
    CompensationStateVersioned, OPERATION_LEASE_MS, Operation, OperationId, OperationKey,
    OperationOutcome, OperationRepo, OperationResult, Phase, PhaseTag, ProviderAdapter,
    SpawnArtifacts, TxOutput, idempotency_payload_conflict, operation_result_from,
    required_lease_owner,
};

#[cfg(any(test, feature = "fixtures"))]
use super::{ParkedCompletion, ParkedOutcome};

mod writes;

use writes::read_output_tx;
pub(super) use writes::{
    PhaseWrite, record_spawn_artifacts, set_parked, write_phase_and_tx_output,
};
pub(crate) use writes::{
    checkpoint_app_server_interact_tx, complete_parked_tx, has_live_claim_tx,
    require_live_claim_tx, require_original_operation_tx,
};

#[cfg(test)]
mod claim_tests;

#[derive(Clone)]
pub struct SqlxOperationRepo {
    pub(super) pool: SqliteDb,
}

impl SqlxOperationRepo {
    pub fn new(pool: SqliteDb) -> Self {
        Self { pool }
    }

    async fn claim_operation_for_boot_recovery(&self, op_id: &str) -> Result<Option<Operation>> {
        let now = now_ms();
        let lease_owner = new_id();
        let lease_until = now + OPERATION_LEASE_MS;
        let result = sqlx::query(
            r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase IN (
                   'pending',
                   'tx_committed',
                   'app_server_interact',
                   'spawn_started',
                   'spawn_succeeded',
                   'compensating'
                 )"#,
        )
        .bind(&lease_owner)
        .bind(lease_until)
        .bind(now)
        .bind(op_id)
        .execute(self.pool.writer_pool())
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_by_id(op_id).await
    }
}

#[async_trait]
impl OperationRepo for SqlxOperationRepo {
    fn sqlite_db(&self) -> SqliteDb {
        self.pool.clone()
    }

    async fn assert_sqlite_version(&self) -> Result<()> {
        let row = sqlx::query("SELECT sqlite_version() AS version")
            .fetch_one(self.pool.read_pool())
            .await?;
        let version: String = row.try_get("version")?;
        if sqlite_version_at_least(&version, 3, 30) {
            return Ok(());
        }
        Err(CalmError::Internal(
            "SQLite < 3.30 does not support partial unique index; upgrade required".into(),
        ))
    }

    async fn insert_operation(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> Result<OperationId> {
        if let Some(idempotency_key) = key.idempotency_key.as_deref()
            && let Some(existing) = self.find_by_kind_idempotency(kind, idempotency_key).await?
        {
            if existing.payload_hash == key.payload_hash {
                return Ok(existing.id);
            }
            return Err(idempotency_payload_conflict(Some(idempotency_key)));
        }
        #[cfg(feature = "fixtures")]
        if let Some(idempotency_key) = key.idempotency_key.as_deref() {
            crate::test_seams::pause_point(
                crate::test_seams::OPERATION_DEDUP_MISSED,
                idempotency_key,
            )
            .await;
        }

        let id = new_id();
        let inserted = insert_pending_row(&id, kind, &key, &payload)?
            .execute(self.pool.writer_pool())
            .await;

        match inserted {
            Ok(_) => Ok(id),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                if let Some(idempotency_key) = key.idempotency_key.as_deref()
                    && let Some(existing) =
                        self.find_by_kind_idempotency(kind, idempotency_key).await?
                {
                    if existing.payload_hash == key.payload_hash {
                        return Ok(existing.id);
                    }
                    return Err(idempotency_payload_conflict(Some(idempotency_key)));
                }
                Err(CalmError::Conflict(format!(
                    "operation key {} already exists",
                    key.operation_key
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn find_by_idempotency_key(
        &self,
        kind: &str,
        key: &OperationKey,
    ) -> Result<Option<Operation>> {
        let Some(idempotency_key) = key.idempotency_key.as_deref() else {
            return Ok(None);
        };
        self.find_by_kind_idempotency(kind, idempotency_key).await
    }

    async fn get_operation(&self, op_id: &str) -> Result<Option<Operation>> {
        self.find_by_id(op_id).await
    }

    async fn get_operation_on_track(
        &self,
        op_id: &str,
        kind: &str,
        track_id: &str,
    ) -> Result<Option<Operation>> {
        let row = sqlx::query(
            "SELECT * FROM operations
             WHERE id = ?1 AND kind = ?2 AND target_type = 'track' AND target_id = ?3",
        )
        .bind(op_id)
        .bind(kind)
        .bind(track_id)
        .fetch_optional(self.pool.read_pool())
        .await?;
        row.as_ref().map(operation_from_row).transpose()
    }

    async fn operation_result(&self, op_id: &str) -> Result<Option<OperationResult>> {
        let Some(op) = self.find_by_id(op_id).await? else {
            return Ok(None);
        };
        operation_result_from(&op)
    }

    async fn claim_drive_batch(&self, limit: i64) -> Result<Vec<Operation>> {
        let now = now_ms();
        let lease_owner = new_id();
        let lease_until = now + OPERATION_LEASE_MS;
        // A deferred read→write upgrade on `operations` deadlocks against any IMMEDIATE writer ("database is
        // deadlocked"); BEGIN IMMEDIATE parks at BEGIN holding nothing.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let ids = sqlx::query(
            r#"SELECT id
               FROM operations
               WHERE phase IN (
                 'pending',
                 'tx_committed',
                 'app_server_interact',
                 'spawn_started',
                 'spawn_succeeded',
                 'compensating'
               )
               AND (lease_until_ms IS NULL OR lease_until_ms < ?1)
               ORDER BY created_at_ms ASC
               LIMIT ?2"#,
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&mut **tx)
        .await?;

        let mut claimed = Vec::new();
        for row in ids {
            let id: String = row.try_get("id")?;
            let result = sqlx::query(
                r#"UPDATE operations
                   SET lease_owner = ?1,
                       lease_until_ms = ?2,
                       updated_at_ms = ?3
                   WHERE id = ?4
                     AND phase IN (
                       'pending',
                       'tx_committed',
                       'app_server_interact',
                     'spawn_started',
                     'spawn_succeeded',
                     'compensating'
                   )
                     AND (lease_until_ms IS NULL OR lease_until_ms < ?3)"#,
            )
            .bind(&lease_owner)
            .bind(lease_until)
            .bind(now)
            .bind(&id)
            .execute(&mut **tx)
            .await?;
            if result.rows_affected() == 1 {
                let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
                    .bind(&id)
                    .fetch_one(&mut **tx)
                    .await?;
                claimed.push(operation_from_row(&row)?);
            }
        }
        tx.commit().await?;
        Ok(claimed)
    }

    async fn abandoned_running_operations_on_boot(&self) -> Result<Vec<Operation>> {
        let rows = sqlx::query(
            r#"SELECT *
               FROM operations
               WHERE phase IN (
                 'pending',
                 'tx_committed',
                 'app_server_interact',
                 'spawn_started',
                 'spawn_succeeded',
                 'parked',
                 'compensating'
               )
               ORDER BY created_at_ms ASC"#,
        )
        .fetch_all(self.pool.read_pool())
        .await?;
        rows.iter().map(operation_from_row).collect()
    }

    async fn abandoned_running_operations_steady_state(&self) -> Result<Vec<Operation>> {
        let now = now_ms();
        let rows = sqlx::query(
            r#"SELECT *
               FROM operations
               WHERE phase IN (
                 'pending',
                 'tx_committed',
                 'app_server_interact',
                 'spawn_started',
                 'spawn_succeeded',
                 'compensating'
               )
               AND (lease_until_ms IS NULL OR lease_until_ms < ?1)
               ORDER BY created_at_ms ASC"#,
        )
        .bind(now)
        .fetch_all(self.pool.read_pool())
        .await?;
        rows.iter().map(operation_from_row).collect()
    }

    async fn claim_operation_for_recovery(&self, op_id: &str) -> Result<Option<Operation>> {
        self.claim_operation_for_boot_recovery(op_id).await
    }

    async fn prepare_tx_and_advance(
        &self,
        op: &Operation,
        adapter: &dyn ProviderAdapter,
        prepared: Option<Box<dyn super::PreparedTx>>,
    ) -> Result<super::PrepareAdvance> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        if !has_live_claim_tx(&mut tx, op).await? {
            return Ok(super::PrepareAdvance::ClaimLost);
        }
        let applied = match prepared {
            Some(prepared) => prepared.apply(&mut tx, op).await,
            None => adapter
                .prepare_tx(&mut tx, &op.payload, op)
                .await
                .map(super::PreparedTxResult::Ready),
        };
        let output = match applied {
            Ok(super::PreparedTxResult::Stale) => {
                tx.rollback().await?;
                return Ok(super::PrepareAdvance::Stale);
            }
            Ok(super::PreparedTxResult::Ready(output)) => output,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        let events = output.post_commit_events.clone();
        let written = write_phase_and_tx_output(
            &mut tx,
            PhaseWrite::Claimed(op),
            &Phase::TxCommitted,
            &output,
        )
        .await?;
        if !written {
            let _ = tx.rollback().await;
            return Ok(super::PrepareAdvance::ClaimLost);
        }
        tx.commit().await?;
        let next = self
            .find_by_id(&op.id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))?;
        Ok(super::PrepareAdvance::Committed(Box::new(next), events))
    }

    async fn set_phase(
        &self,
        op: &Operation,
        phase: Phase,
        owner_key: Option<&'static str>,
    ) -> Result<super::LeaveWrite<Operation>> {
        if matches!(phase, Phase::AwaitingRetry { .. })
            && !matches!(op.phase, Phase::AppServerInteract { .. })
        {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        let mut tx = begin_immediate_tx(&self.pool).await?;
        if !has_live_claim_tx(&mut tx, op).await? {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        if writes::retain_owned_tx(&mut tx, op, owner_key).await? {
            tx.commit().await?;
            return Ok(super::LeaveWrite::Retained);
        }
        let (tag, detail) = phase.serialize_split();
        let detail_text = optional_json_text(detail.as_ref())?;
        let completed_at = matches!(
            phase,
            Phase::Succeeded | Phase::Failed | Phase::Stuck { .. }
        )
        .then(now_ms);
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = ?1,
                   phase_detail_json = ?2,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   completed_at_ms = COALESCE(?3, completed_at_ms),
                   updated_at_ms = ?4
               WHERE id = ?5
                 AND lease_owner = ?6"#,
        )
        .bind(tag.as_str())
        .bind(detail_text)
        .bind(completed_at)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        tx.commit().await?;
        self.find_by_id(&op.id)
            .await?
            .map(super::LeaveWrite::Applied)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn set_phase_and_tx_output(
        &self,
        op: &Operation,
        phase: Phase,
        output: &TxOutput,
    ) -> Result<Option<Operation>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let written =
            write_phase_and_tx_output(&mut tx, PhaseWrite::Claimed(op), &phase, output).await?;
        if !written {
            return Ok(None);
        }
        tx.commit().await?;
        self.find_by_id(&op.id)
            .await?
            .map(Some)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn set_compensating(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
        owner_key: Option<&'static str>,
    ) -> Result<super::LeaveWrite<Operation>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        if !has_live_claim_tx(&mut tx, op).await? {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        if writes::retain_owned_tx(&mut tx, op, owner_key).await? {
            tx.commit().await?;
            return Ok(super::LeaveWrite::Retained);
        }
        // Compensation owns its plan and phase. Domain output remains the
        // owner's last committed checkpoint, including when a later checkpoint
        // failed after an external effect already happened.
        if read_output_tx(&mut tx, &op.id).await?.is_none() {
            return Err(CalmError::Internal(format!(
                "operation {} has no committed output for compensation",
                op.id
            )));
        }
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = 'compensating',
                   phase_detail_json = ?1,
                   compensation_state = ?2,
                   last_error = ?3,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   updated_at_ms = ?4
               WHERE id = ?5 AND lease_owner = ?6"#,
        )
        .bind(serde_json::to_string(&json!({
            "from_phase": state.from_phase, "reason": state.reason,
        }))?)
        .bind(serde_json::to_string(state)?)
        .bind(&state.reason)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        tx.commit().await?;
        self.find_by_id(&op.id)
            .await?
            .map(super::LeaveWrite::Applied)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn update_compensation_state(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
    ) -> Result<Option<Operation>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        if !has_live_claim_tx(&mut tx, op).await? {
            return Ok(None);
        }
        let result = sqlx::query(
            r#"UPDATE operations
               SET compensation_state = ?1,
                   updated_at_ms = ?2
               WHERE id = ?3
                 AND lease_owner = ?4"#,
        )
        .bind(serde_json::to_string(state)?)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        tx.commit().await?;
        self.find_by_id(&op.id)
            .await?
            .map(Some)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn mark_failed(
        &self,
        op: &Operation,
        last_error: String,
        from_phase: PhaseTag,
        last_error_class: Option<String>,
        owner_key: Option<&'static str>,
    ) -> Result<super::LeaveWrite<OperationResult>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        if !has_live_claim_tx(&mut tx, op).await? {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        if writes::retain_owned_tx(&mut tx, op, owner_key).await? {
            tx.commit().await?;
            return Ok(super::LeaveWrite::Retained);
        }
        let now = now_ms();
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = 'failed',
                   phase_detail_json = ?1,
                   last_error = ?2,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   completed_at_ms = ?3,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND lease_owner = ?5"#,
        )
        .bind(serde_json::to_string(&json!({
            "from_phase": from_phase,
            "last_error_class": last_error_class,
        }))?)
        .bind(&last_error)
        .bind(now)
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        tx.commit().await?;
        Ok(super::LeaveWrite::Applied(OperationResult {
            op_id: op.id.clone(),
            outcome: OperationOutcome::Failed {
                last_error,
                from_phase,
                last_error_class,
            },
        }))
    }

    async fn mark_stuck(
        &self,
        op: &Operation,
        reason: String,
        from_phase: PhaseTag,
        owner_key: Option<&'static str>,
    ) -> Result<super::LeaveWrite<OperationResult>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        if !has_live_claim_tx(&mut tx, op).await? {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        if writes::retain_owned_tx(&mut tx, op, owner_key).await? {
            tx.commit().await?;
            return Ok(super::LeaveWrite::Retained);
        }
        let now = now_ms();
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = 'stuck',
                   phase_detail_json = ?1,
                   last_error = ?2,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   completed_at_ms = ?3,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND lease_owner = ?5"#,
        )
        .bind(serde_json::to_string(&json!({
            "reason": reason,
            "since": now,
            "from_phase": from_phase,
        }))?)
        .bind(&reason)
        .bind(now)
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(super::LeaveWrite::ClaimLost);
        }
        tx.commit().await?;
        Ok(super::LeaveWrite::Applied(OperationResult {
            op_id: op.id.clone(),
            outcome: OperationOutcome::Stuck { reason, from_phase },
        }))
    }
}

impl SqlxOperationRepo {
    async fn find_by_id(&self, op_id: &str) -> Result<Option<Operation>> {
        let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
            .bind(op_id)
            .fetch_optional(self.pool.read_pool())
            .await?;
        row.as_ref().map(operation_from_row).transpose()
    }

    async fn find_by_kind_idempotency(
        &self,
        kind: &str,
        idempotency_key: &str,
    ) -> Result<Option<Operation>> {
        let row = sqlx::query(
            "SELECT * FROM operations WHERE kind = ?1 AND idempotency_key = ?2 LIMIT 1",
        )
        .bind(kind)
        .bind(idempotency_key)
        .fetch_optional(self.pool.read_pool())
        .await?;
        row.as_ref().map(operation_from_row).transpose()
    }
}

#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub async fn complete_parked_for_test(
    pool: &crate::db::sqlite::SqliteDb,
    op_id: &OperationId,
    outcome: &ParkedOutcome,
) -> Result<ParkedCompletion> {
    let mut tx = begin_immediate_tx(pool).await?;
    let completion = complete_parked_tx(&mut tx, op_id, outcome).await?;
    tx.commit().await?;
    Ok(completion)
}

pub(super) async fn fetch_claimed_parked(
    pool: &SqliteDb,
    op_id: &str,
    lease_owner: &str,
) -> Result<Option<Operation>> {
    let row = sqlx::query(
        r#"SELECT *
           FROM operations
           WHERE id = ?1
             AND lease_owner = ?2
             AND phase = 'parked'"#,
    )
    .bind(op_id)
    .bind(lease_owner)
    .fetch_optional(pool.read_pool())
    .await?;
    row.as_ref().map(operation_from_row).transpose()
}

/// The one `operations` INSERT: a `pending` row `id` under `key`.
pub(super) fn insert_pending_row(
    id: &str,
    kind: &str,
    key: &OperationKey,
    payload: &Value,
) -> Result<Query<'static, Sqlite, SqliteArguments<'static>>> {
    let (target_type, target_id, target_json) = target_from_payload(payload);
    Ok(sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               phase, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?10)"#,
    )
    .bind(id.to_string())
    .bind(key.operation_key.clone())
    .bind(kind.to_string())
    .bind(key.idempotency_key.clone())
    .bind(key.payload_hash.clone())
    .bind(target_type)
    .bind(target_id)
    .bind(serde_json::to_string(&target_json)?)
    .bind(serde_json::to_string(payload)?)
    .bind(now_ms()))
}

pub(super) fn operation_from_row(row: &SqliteRow) -> Result<Operation> {
    let target_json: String = row.try_get("target_json")?;
    let payload_json: String = row.try_get("payload_json")?;
    let phase_text: String = row.try_get("phase")?;
    let phase_detail_json: Option<String> = row.try_get("phase_detail_json")?;
    let phase_detail = phase_detail_json
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;
    let tx_output_json: Option<String> = row.try_get("tx_output_json")?;
    let tx_output = tx_output_json
        .as_deref()
        .map(serde_json::from_str::<TxOutput>)
        .transpose()?;
    let compensation_state_text: Option<String> = row.try_get("compensation_state")?;
    let compensation_state = compensation_state_text
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;
    let spawn_artifacts_text: Option<String> = row.try_get("spawn_artifacts_json")?;
    let spawn_artifacts =
        spawn_artifacts_text.as_deref().and_then(|text| {
            match serde_json::from_str::<SpawnArtifacts>(text) {
                Ok(artifacts) => Some(artifacts),
                Err(e) => {
                    tracing::warn!(
                        operation_id = ?row.try_get::<String, _>("id").ok(),
                        error = %e,
                        "operation row has invalid spawn_artifacts_json"
                    );
                    None
                }
            }
        });
    Ok(Operation {
        id: row.try_get("id")?,
        operation_key: row.try_get("operation_key")?,
        kind: row.try_get("kind")?,
        idempotency_key: row.try_get("idempotency_key")?,
        payload_hash: row.try_get("payload_hash")?,
        target_type: row.try_get("target_type")?,
        target_id: row.try_get("target_id")?,
        target: serde_json::from_str(&target_json)?,
        payload: serde_json::from_str(&payload_json)?,
        tx_output,
        phase: Phase::deserialize_join(&phase_text, phase_detail.as_ref())?,
        phase_detail,
        attempt: row.try_get("attempt")?,
        last_error: row.try_get("last_error")?,
        compensation_state,
        lease_owner: row.try_get("lease_owner")?,
        lease_until_ms: row.try_get("lease_until_ms")?,
        spawn_artifacts,
        parked_at_ms: row.try_get("parked_at_ms")?,
        parked_deadline_ms: row.try_get("parked_deadline_ms")?,
    })
}

fn optional_json_text(value: Option<&Value>) -> Result<Option<String>> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

fn target_from_payload(payload: &Value) -> (String, Option<String>, Value) {
    if let Some(runtime_id) = payload.get("runtime_id").and_then(Value::as_str) {
        return (
            "runtime".to_string(),
            Some(runtime_id.to_string()),
            json!({ "type": "runtime", "id": runtime_id }),
        );
    }
    let track_id = payload.get("track_id").and_then(Value::as_str).or_else(|| {
        payload
            .get("request")
            .and_then(|request| request.get("track_id"))
            .and_then(Value::as_str)
    });
    if let Some(track_id) = track_id {
        return (
            "track".to_string(),
            Some(track_id.to_string()),
            json!({ "type": "track", "id": track_id }),
        );
    }
    (
        "unknown".to_string(),
        None,
        json!({ "type": "unknown", "id": Value::Null }),
    )
}

fn sqlite_version_at_least(version: &str, want_major: u64, want_minor: u64) -> bool {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    (major, minor) >= (want_major, want_minor)
}

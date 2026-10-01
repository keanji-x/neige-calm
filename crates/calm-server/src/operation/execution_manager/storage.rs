//! The manager's reservation and release authority over durable native execution records.
use super::{Owner, Record};
use crate::error::Result;
use crate::operation::workspace_lease::execution_guard::{ExecutionReadGuard, ExecutionWriteGuard, NativeProvider, NativeTaskGuard};
use crate::operation::workspace_lease::task_guard::{PreparedTaskAccess, prepared_task_access};
use calm_types::workspace_access::WorkspaceAccess;
use sqlx::SqlitePool;

pub(super) struct Reservation(NativeTaskGuard);
impl Reservation {
    pub(super) async fn acquire(pool: &SqlitePool, owner: &Owner) -> Result<Self> {
        let guard = match prepared_task_access(pool, &owner.card).await? {
            PreparedTaskAccess::Read => NativeTaskGuard::Read(ExecutionReadGuard::acquire_native(
                pool, &owner.card, &owner.holder, NativeProvider::Codex,
            ).await?),
            PreparedTaskAccess::Write { attempt } => NativeTaskGuard::Write(ExecutionWriteGuard::acquire_native(
                pool, &owner.card, &owner.holder, &attempt, NativeProvider::Codex,
            ).await?),
            PreparedTaskAccess::Independent => NativeTaskGuard::Write(ExecutionWriteGuard::acquire_native(
                pool, &owner.card, &owner.holder, "", NativeProvider::Codex,
            ).await?),
        };
        Ok(Self(guard))
    }
    pub(super) fn id(&self) -> &str { self.0.execution_id() }
    pub(super) async fn nonce(&self, preferred: Option<&str>) -> Result<String> { self.0.client_nonce(preferred).await }
    pub(super) async fn started(self, pool: &SqlitePool, identity: &str) -> Result<bool> {
        if identity.is_empty() { return Err(crate::error::CalmError::Conflict("empty backend launch identity".into())); }
        let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
        let changed = sqlx::query(
            "UPDATE workspace_leases SET native_observed_turn_id=?2,lease_owner=?2, \
             holder_phase=CASE WHEN holder_phase='stopping' THEN 'stopping' ELSE 'running' END \
             WHERE lease_id=?1 AND state='held' AND holder_phase IN ('issuing','stopping') \
             AND (native_observed_turn_id IS NULL OR native_observed_turn_id=?2)"
        ).bind(self.id()).bind(identity).execute(&mut *tx).await?.rows_affected();
        if changed == 1 { tx.commit().await?; return Ok(false); }
        let row: Option<(String,String,Option<String>)> = sqlx::query_as(
            "SELECT state,holder_phase,native_observed_turn_id FROM workspace_leases WHERE lease_id=?1"
        ).bind(self.id()).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        match row {
            Some((state, phase, Some(observed))) if observed==identity && state=="held" && phase=="running" => Ok(false),
            Some((state, phase, Some(observed))) if observed==identity && state=="released" && phase=="stopped" => Ok(true),
            _ => Err(crate::error::CalmError::Conflict("launch acknowledgement differs from current execution generation".into())),
        }
    }
    pub(super) async fn reject(self) -> Result<()> { self.0.rejected().await }
}

pub(super) async fn load(pool: &SqlitePool, execution: &str) -> Result<Option<Record>> {
    let row: Option<(String, String, Option<String>, Option<String>, String, String)> = sqlx::query_as(
        "SELECT holder_id,holder_phase,native_client_id,native_observed_turn_id,path,access_mode \
         FROM workspace_leases WHERE lease_id=?1 AND holder_kind='native' AND native_provider='codex' AND state='held'",
    ).bind(execution).fetch_optional(pool).await?;
    row.map(|(holder,phase,nonce,observed,cwd,access)| {
        Ok(Record { id: execution.to_owned(), holder, phase, nonce, observed, cwd,
            access: serde_json::from_value(serde_json::Value::String(access))? })
    }).transpose()
}

pub(super) async fn request_stop(pool: &SqlitePool, execution: &str) -> Result<()> {
    sqlx::query("UPDATE workspace_leases SET holder_phase='stopping' WHERE lease_id=?1 \
        AND holder_kind='native' AND native_provider='codex' AND state='held'")
        .bind(execution).execute(pool).await?;
    Ok(())
}

pub(super) async fn observe(pool: &SqlitePool, record: &Record, identity: &str) -> Result<()> {
    sqlx::query("UPDATE workspace_leases SET lease_owner=?2,native_observed_turn_id=?2, \
        holder_phase=CASE WHEN holder_phase='stopping' THEN 'stopping' ELSE 'running' END \
        WHERE lease_id=?1 AND holder_kind='native' AND native_provider='codex' AND state='held'")
        .bind(&record.id).bind(identity).execute(pool).await?;
    Ok(())
}

pub(super) async fn release_confirmed(pool: &SqlitePool, record: &Record, identity: &str, now: i64) -> Result<bool> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let changed = sqlx::query("UPDATE workspace_leases SET state='released',holder_phase='stopped', \
        released_at_ms=?2,updated_at_ms=?2 WHERE lease_id=?1 AND holder_kind='native' \
        AND native_provider='codex' AND state='held' AND native_observed_turn_id=?3")
        .bind(&record.id).bind(now).bind(identity).execute(&mut *tx).await?.rows_affected();
    if changed == 1 && record.access == WorkspaceAccess::ReadOnly {
        sqlx::query("UPDATE workspace_leases AS task SET read_stop_confirmed_at_ms=?2 \
            WHERE task.holder_kind='task' AND task.access_mode='read_only' AND task.state='held' \
            AND EXISTS(SELECT 1 FROM workspace_leases native JOIN tasks current ON current.worker_card_id=native.card_id \
            JOIN operations attempt ON attempt.id=task.lease_owner AND attempt.idempotency_key=current.id \
            WHERE native.lease_id=?1 AND native.card_id=task.card_id \
            AND COALESCE(native.canonical_path,native.path)=COALESCE(task.canonical_path,task.path) \
            AND current.status IN ('done','failed','canceled'))")
            .bind(&record.id).bind(now).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(changed == 1)
}

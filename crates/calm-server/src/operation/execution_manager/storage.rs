//! The manager's reservation and release authority over durable native execution records.
use super::{BackendKind, Owner, Record};
use crate::error::Result;
use crate::operation::workspace_lease::execution_guard::{
    ExecutionReadGuard, ExecutionWriteGuard, NativeProvider, NativeTaskGuard,
};
use crate::operation::workspace_lease::task_guard::{PreparedTaskAccess, prepared_task_access};
use calm_types::workspace_access::WorkspaceAccess;
use sqlx::SqlitePool;

pub(super) struct Reservation(NativeTaskGuard);
impl Reservation {
    pub(super) async fn acquire(pool: &SqlitePool, owner: &Owner) -> Result<Self> {
        let guard = match prepared_task_access(pool, &owner.card).await? {
            PreparedTaskAccess::Read => NativeTaskGuard::Read(
                ExecutionReadGuard::acquire_native(
                    pool,
                    &owner.card,
                    &owner.holder,
                    NativeProvider::Codex,
                )
                .await?,
            ),
            PreparedTaskAccess::Write { attempt } => NativeTaskGuard::Write(
                ExecutionWriteGuard::acquire_native(
                    pool,
                    &owner.card,
                    &owner.holder,
                    &attempt,
                    NativeProvider::Codex,
                )
                .await?,
            ),
            PreparedTaskAccess::Independent => NativeTaskGuard::Write(
                ExecutionWriteGuard::acquire_native(
                    pool,
                    &owner.card,
                    &owner.holder,
                    "",
                    NativeProvider::Codex,
                )
                .await?,
            ),
        };
        Ok(Self(guard))
    }
    pub(super) fn id(&self) -> &str {
        self.0.execution_id()
    }
    pub(super) async fn nonce(&self, preferred: Option<&str>) -> Result<String> {
        self.0.client_nonce(preferred).await
    }
    pub(super) async fn started(self, pool: &SqlitePool, identity: &str) -> Result<bool> {
        if identity.is_empty() {
            return Err(crate::error::CalmError::Conflict(
                "empty backend launch identity".into(),
            ));
        }
        let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
        let changed = sqlx::query(
            "UPDATE workspace_leases SET native_observed_turn_id=?2,lease_owner=?2, \
             holder_phase=CASE WHEN holder_phase='stopping' THEN 'stopping' ELSE 'running' END \
             WHERE lease_id=?1 AND state='held' AND holder_phase IN ('issuing','stopping') \
             AND (native_observed_turn_id IS NULL OR native_observed_turn_id=?2)",
        )
        .bind(self.id())
        .bind(identity)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 1 {
            tx.commit().await?;
            return Ok(false);
        }
        let row: Option<(String,String,Option<String>)> = sqlx::query_as(
            "SELECT state,holder_phase,native_observed_turn_id FROM workspace_leases WHERE lease_id=?1"
        ).bind(self.id()).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        match row {
            Some((state, phase, Some(observed)))
                if observed == identity && state == "held" && phase == "running" =>
            {
                Ok(false)
            }
            Some((state, phase, Some(observed)))
                if observed == identity && state == "released" && phase == "stopped" =>
            {
                Ok(true)
            }
            _ => Err(crate::error::CalmError::Conflict(
                "launch acknowledgement differs from current execution generation".into(),
            )),
        }
    }
    pub(super) async fn reject(self) -> Result<()> {
        self.0.rejected().await
    }
}

pub(super) async fn load(pool: &SqlitePool, execution: &str) -> Result<Option<Record>> {
    let row: Option<(String,String,Option<String>,Option<String>,String,String,String)> = sqlx::query_as(
        "SELECT holder_id,holder_phase,native_client_id,native_observed_turn_id,path,access_mode,holder_kind \
         FROM workspace_leases WHERE lease_id=?1 AND state='held' AND ( \
         (holder_kind='native' AND native_provider='codex') OR (holder_kind='terminal' AND EXISTS( \
         SELECT 1 FROM operations o WHERE o.kind IN ('codex-create','codex-worker') \
         AND json_extract(o.tx_output_json,'$.data.terminal_id')=workspace_leases.holder_id)))"
    ).bind(execution).fetch_optional(pool).await?;
    row.map(|(holder, phase, nonce, observed, cwd, access, kind)| {
        let backend = if kind == "native" {
            BackendKind::NativeTurn
        } else {
            BackendKind::NativeSession
        };
        let observed = if backend == BackendKind::NativeSession && phase != "issuing" {
            Some(holder.clone())
        } else {
            observed
        };
        Ok(Record {
            id: execution.to_owned(),
            backend,
            holder,
            phase,
            nonce,
            observed,
            cwd,
            access: serde_json::from_value(serde_json::Value::String(access))?,
        })
    })
    .transpose()
}

pub(super) async fn request_stop(pool: &SqlitePool, execution: &str) -> Result<()> {
    sqlx::query(
        "UPDATE workspace_leases SET holder_phase='stopping' WHERE lease_id=?1 \
        AND state='held' AND (holder_kind='terminal' OR (holder_kind='native' AND native_provider='codex'))",
    )
    .bind(execution)
    .execute(pool)
    .await?;
    Ok(())
}

pub(super) async fn observe(pool: &SqlitePool, record: &Record, identity: &str) -> Result<()> {
    if record.backend == BackendKind::NativeSession {
        if identity != record.holder {
            return Err(crate::error::CalmError::Conflict(
                "session proof has another identity".into(),
            ));
        }
        sqlx::query("UPDATE workspace_leases SET holder_phase=CASE WHEN holder_phase='stopping' \
            THEN 'stopping' ELSE 'running' END WHERE lease_id=?1 AND holder_kind='terminal' AND state='held'")
            .bind(&record.id).execute(pool).await?;
    } else {
        sqlx::query("UPDATE workspace_leases SET lease_owner=?2,native_observed_turn_id=?2, \
            holder_phase=CASE WHEN holder_phase='stopping' THEN 'stopping' ELSE 'running' END \
            WHERE lease_id=?1 AND holder_kind='native' AND native_provider='codex' AND state='held'")
            .bind(&record.id).bind(identity).execute(pool).await?;
    }
    Ok(())
}

pub(super) async fn release_confirmed(
    pool: &SqlitePool,
    record: &Record,
    identity: &str,
    now: i64,
) -> Result<bool> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let changed = sqlx::query(
        "UPDATE workspace_leases SET state='released',holder_phase='stopped', \
        released_at_ms=?2,updated_at_ms=?2 WHERE lease_id=?1 AND state='held' AND ( \
        (holder_kind='native' AND native_provider='codex' AND native_observed_turn_id=?3) \
        OR (holder_kind='terminal' AND holder_id=?3))",
    )
    .bind(&record.id)
    .bind(now)
    .bind(identity)
    .execute(&mut *tx)
    .await?
    .rows_affected();
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
    if changed == 1 {
        let card: String =
            sqlx::query_scalar("SELECT card_id FROM workspace_leases WHERE lease_id=?1")
                .bind(&record.id)
                .fetch_one(&mut *tx)
                .await?;
        let events = task_ended_tx(
            &mut tx,
            &card,
            crate::operation::workspace_lease::ReleaseDelivery::CommitAsTaskEnded,
        )
        .await?;
        crate::operation::workspace_lease::append_workspace_events_tx(&mut tx, events).await?;
    }
    tx.commit().await?;
    Ok(changed == 1)
}

/// Business completion may publish a delivery only after the owned execution has stopped.
pub(super) async fn task_ended_tx(
    tx: &mut crate::operation::Tx<'_>,
    card: &str,
    delivery: crate::operation::workspace_lease::ReleaseDelivery,
) -> Result<
    Vec<(
        crate::ids::ActorId,
        crate::event::EventScope,
        crate::event::Event,
    )>,
> {
    let native_task: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspace_leases task JOIN operations attempt ON attempt.id=task.lease_owner \
         WHERE task.card_id=?1 AND task.holder_kind='task' AND attempt.kind='codex-worker')"
    ).bind(card).fetch_one(&mut **tx).await?;
    if native_task {
        let ended: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tasks t JOIN operations attempt ON attempt.idempotency_key=t.id \
             JOIN workspace_leases task ON task.lease_owner=attempt.id WHERE task.card_id=?1 \
             AND task.holder_kind='task' AND t.status IN ('done','failed','canceled'))"
        ).bind(card).fetch_one(&mut **tx).await?;
        if !ended {
            return Ok(Vec::new());
        }

        let stopped: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspace_leases native JOIN workspace_leases task ON task.card_id=native.card_id \
             WHERE native.card_id=?1 AND native.holder_kind='native' AND native.native_provider='codex' \
             AND native.state='released' AND native.holder_phase='stopped' \
             AND (native.native_observed_turn_id IS NOT NULL OR native.native_client_id IS NULL) \
             AND COALESCE(native.canonical_path,native.path)=COALESCE(task.canonical_path,task.path)) \
             AND NOT EXISTS(SELECT 1 FROM workspace_leases live WHERE live.card_id=?1 \
             AND live.holder_kind IN ('native','terminal','forge') AND live.state IN ('held','releasing'))"
        ).bind(card).fetch_one(&mut **tx).await?;
        if !stopped {
            return Ok(Vec::new());
        }
    }
    crate::operation::workspace_lease::release_workspace_lease_for_card_tx(tx, card, delivery).await
}

pub(super) async fn claim_session(pool: &SqlitePool, execution: &str) -> Result<()> {
    let changed = sqlx::query(
        "UPDATE workspace_leases SET holder_phase='running' WHERE lease_id=?1 \
        AND holder_kind='terminal' AND state='held' AND holder_phase='issuing'",
    )
    .bind(execution)
    .execute(pool)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(crate::error::CalmError::Conflict(
            "session launch capability already consumed".into(),
        ));
    }
    Ok(())
}
pub(super) async fn session_started(
    pool: &SqlitePool,
    execution: &str,
    identity: &str,
) -> Result<bool> {
    let row:Option<(String,String,String)>=sqlx::query_as(
        "SELECT holder_id,state,holder_phase FROM workspace_leases WHERE lease_id=?1 AND holder_kind='terminal'"
    ).bind(execution).fetch_optional(pool).await?;
    match row {
        Some((holder, state, phase))
            if holder == identity && state == "held" && phase == "running" =>
        {
            Ok(false)
        }
        Some((holder, state, phase))
            if holder == identity && state == "released" && phase == "stopped" =>
        {
            Ok(true)
        }
        _ => Err(crate::error::CalmError::Conflict(
            "session acknowledgement differs from reservation".into(),
        )),
    }
}
pub(super) async fn reject_session(pool: &SqlitePool, execution: &str) -> Result<()> {
    sqlx::query(
        "UPDATE workspace_leases SET state='released',holder_phase='stopped' WHERE lease_id=?1 \
        AND holder_kind='terminal' AND state='held' AND holder_phase='running'",
    )
    .bind(execution)
    .execute(pool)
    .await?;
    Ok(())
}

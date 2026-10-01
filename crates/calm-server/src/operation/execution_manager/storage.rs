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
    let mut connection = pool.acquire().await?;
    load_in(&mut connection, execution).await
}
pub(super) async fn load_in(
    connection: &mut sqlx::SqliteConnection,
    execution: &str,
) -> Result<Option<Record>> {
    type StoredExecution = (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
    );
    let row: Option<StoredExecution> = sqlx::query_as(
        "SELECT holder_id,holder_phase,native_client_id,native_observed_turn_id,path,access_mode,holder_kind \
         FROM workspace_leases WHERE lease_id=?1 AND state='held' AND ( \
         (holder_kind='native' AND native_provider='codex') OR (holder_kind='terminal' AND EXISTS( \
         SELECT 1 FROM operations o WHERE o.kind IN ('codex-create','codex-worker') \
         AND json_extract(o.tx_output_json,'$.data.terminal_id')=workspace_leases.holder_id)))"
    ).bind(execution).fetch_optional(connection).await?;
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
        let changed=sqlx::query("UPDATE workspace_leases SET lease_owner=?2,native_observed_turn_id=?2, \
            holder_phase=CASE WHEN holder_phase='stopping' THEN 'stopping' ELSE 'running' END \
            WHERE lease_id=?1 AND holder_kind='native' AND native_provider='codex' AND state='held' \
            AND (native_observed_turn_id IS NULL OR native_observed_turn_id=?2)")
            .bind(&record.id).bind(identity).execute(pool).await?.rows_affected();
        if changed != 1 && !native_turn_stopped(pool, &record.holder, identity).await? {
            return Err(crate::error::CalmError::Conflict(
                "native observed generation changed".into(),
            ));
        }
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
    let released = release_confirmed_tx(&mut tx, record, identity, now).await?;
    tx.commit().await?;
    Ok(released)
}
pub(super) async fn release_confirmed_tx(
    tx: &mut crate::operation::Tx<'_>,
    record: &Record,
    identity: &str,
    now: i64,
) -> Result<bool> {
    let changed = sqlx::query(
        "UPDATE workspace_leases SET state='released',holder_phase='stopped', \
        released_at_ms=?2,updated_at_ms=?2 WHERE lease_id=?1 AND state='held' AND ( \
        (holder_kind='native' AND native_provider='codex' AND native_observed_turn_id=?3) \
        OR (holder_kind='terminal' AND holder_id=?3))",
    )
    .bind(&record.id)
    .bind(now)
    .bind(identity)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed == 1 {
        let card: String =
            sqlx::query_scalar("SELECT card_id FROM workspace_leases WHERE lease_id=?1")
                .bind(&record.id)
                .fetch_one(&mut **tx)
                .await?;
        confirm_stopped_read_intent_tx(tx, &card, &record.cwd, now).await?;
    }
    if changed == 1 {
        let card: String =
            sqlx::query_scalar("SELECT card_id FROM workspace_leases WHERE lease_id=?1")
                .bind(&record.id)
                .fetch_one(&mut **tx)
                .await?;
        let events = task_ended_tx(
            tx,
            &card,
            crate::operation::workspace_lease::ReleaseDelivery::CommitAsTaskEnded,
        )
        .await?;
        crate::operation::workspace_lease::append_workspace_events_tx(tx, events).await?;
    }
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

/// The manager records the same positive session proof in the owning operation checkpoint.
pub(super) async fn session_stop_record_tx(
    tx: &mut crate::operation::Tx<'_>,
    record: &Record,
    supervisor_socket: &std::path::Path,
) -> Result<()> {
    if record.backend != BackendKind::NativeSession {
        return Err(crate::error::CalmError::Conflict(
            "terminal checkpoint belongs to a session backend".into(),
        ));
    }
    let stopped =
        serde_json::to_string(&crate::operation::terminal_launch::RequestState::Stopped {
            version: 1,
            terminal_id: record.holder.clone(),
            supervisor_sock: supervisor_socket.to_owned(),
        })?;
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch',json(?1)) \
        WHERE kind IN ('codex-create','codex-worker') AND json_extract(tx_output_json,'$.data.terminal_id')=?2")
        .bind(stopped).bind(&record.holder).execute(&mut **tx).await?;
    Ok(())
}

/// Match the exact running generation; caches cannot authorize control.
pub(super) async fn running_native_turn(
    pool: &SqlitePool,
    thread: &str,
    turn: &str,
) -> Result<Option<Record>> {
    let mut connection = pool.acquire().await?;
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT lease_id FROM workspace_leases WHERE holder_kind='native' AND native_provider='codex' \
         AND holder_id=?1 AND native_observed_turn_id=?2 AND state='held' AND holder_phase='running' \
         LIMIT 2",
    )
    .bind(thread)
    .bind(turn)
    .fetch_all(&mut *connection)
    .await?;
    match ids.as_slice() {
        [] => Ok(None),
        [id] => Ok(load_in(&mut connection, id).await?.filter(|record| {
            record.phase == "running" && record.observed.as_deref() == Some(turn)
        })),
        _ => Err(crate::error::CalmError::Conflict(
            "native control generation has multiple owners".into(),
        )),
    }
}

pub(super) async fn native_executions(pool: &SqlitePool, thread: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT lease_id FROM workspace_leases WHERE holder_kind='native' AND native_provider='codex' \
         AND holder_id=?1 AND state='held' ORDER BY created_at_ms,lease_id",
    )
    .bind(thread)
    .fetch_all(pool)
    .await?)
}

pub(super) async fn native_writer_available(pool: &SqlitePool, owner: &Owner) -> Result<bool> {
    let mut connection = pool.acquire().await?;
    crate::operation::workspace_lease::execution_guard::native_write_available_tx(
        &mut connection,
        &owner.card,
        &owner.holder,
        "",
        NativeProvider::Codex,
    )
    .await
}

pub(super) async fn native_turn_stopped(
    pool: &SqlitePool,
    thread: &str,
    turn: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspace_leases WHERE holder_kind='native' \
         AND native_provider='codex' AND holder_id=?1 AND native_observed_turn_id=?2 \
         AND state='released' AND holder_phase='stopped')",
    )
    .bind(thread)
    .bind(turn)
    .fetch_one(pool)
    .await?)
}

/// A legacy owner without execution evidence cannot be mistaken for an absent process.
pub(super) async fn native_thread_requires_recovery(
    pool: &SqlitePool,
    thread: &str,
) -> Result<bool> {
    let mut connection = pool.acquire().await?;
    native_thread_requires_recovery_in(&mut connection, thread).await
}

pub(super) async fn native_thread_requires_recovery_in(
    connection: &mut sqlx::SqliteConnection,
    thread: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions session JOIN cards card ON card.session_id=session.id \
         WHERE session.provider='codex' AND session.thread_id=?1 \
         AND session.state IN ('starting','running','idle','turn_pending') \
         AND NOT EXISTS(SELECT 1 FROM workspace_execution_bindings binding \
             WHERE binding.provider='codex' AND binding.holder_id=?1 AND binding.card_id=card.id \
             AND binding.scope_phase='new') \
         AND NOT EXISTS(SELECT 1 FROM workspace_leases execution WHERE execution.holder_kind='native' \
             AND execution.native_provider='codex' AND execution.holder_id=?1 \
             AND execution.state='released' AND execution.holder_phase='stopped')) \
         OR EXISTS(SELECT 1 FROM workspace_execution_bindings binding WHERE binding.provider='codex' \
             AND binding.holder_id=?1 AND (binding.scope_phase='recovering' OR (binding.scope_phase='closed' \
         AND NOT EXISTS(SELECT 1 FROM workspace_leases proof WHERE proof.holder_kind='native' \
         AND proof.native_provider='codex' AND proof.holder_id=?1 AND proof.state='released' AND proof.holder_phase='stopped'))))",
    ).bind(thread).fetch_one(&mut *connection).await?)
}

/// Recovery obtains ownership from durable attribution and scope from the provider.
pub(super) async fn adopt_discovered_native_scope(
    pool: &SqlitePool,
    thread: &str,
    cwd: &str,
    observed: Option<&str>,
    stopped: bool,
    close_scope: bool,
) -> Result<()> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let owners: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT owner.card_id FROM ( \
         SELECT binding.card_id FROM workspace_execution_bindings binding \
             WHERE binding.provider='codex' AND binding.holder_id=?1 \
         UNION SELECT session.card_id FROM worker_sessions session \
             JOIN cards current ON current.session_id=session.id \
             WHERE session.provider='codex' AND session.thread_id=?1 \
         UNION SELECT json_extract(operation.tx_output_json,'$.data.card_id') FROM operations operation \
             WHERE operation.kind IN ('codex-worker','codex-create','planner-harness-start') \
             AND json_extract(operation.tx_output_json,'$.data.thread_id')=?1 \
         ) owner JOIN cards card ON card.id=owner.card_id LIMIT 2",
    ).bind(thread).fetch_all(&mut *tx).await?;
    let [card] = owners.as_slice() else {
        return Err(crate::error::CalmError::Conflict(
            "native recovery requires one durable owner".into(),
        ));
    };
    crate::operation::workspace_lease::execution_guard::adopt_native_scope_tx(
        &mut tx,
        card,
        thread,
        NativeProvider::Codex,
        cwd,
        WorkspaceAccess::ReadWrite,
        observed,
        stopped,
    )
    .await?;
    if close_scope {
        sqlx::query("UPDATE workspace_execution_bindings SET scope_phase='closed' WHERE provider='codex' AND holder_id=?1")
            .bind(thread).execute(&mut *tx).await?;
    }
    if stopped {
        let evidence: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspace_leases WHERE holder_kind='native' \
            AND native_provider='codex' AND holder_id=?1 AND state IN ('held','released'))",
        )
        .bind(thread)
        .fetch_one(&mut *tx)
        .await?;
        if !evidence {
            crate::operation::workspace_lease::execution_guard::record_stopped_native_scope_tx(
                &mut tx,
                card,
                thread,
                NativeProvider::Codex,
                cwd,
                WorkspaceAccess::ReadWrite,
                observed,
            )
            .await?;
        }
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT lease_id FROM workspace_leases WHERE holder_kind='native' \
             AND native_provider='codex' AND holder_id=?1 AND state='held' AND native_client_id IS NULL",
        ).bind(thread).fetch_all(&mut *tx).await?;
        for id in ids {
            let Some(record) = load_in(&mut tx, &id).await? else {
                continue;
            };
            if let Some(observed) = observed {
                sqlx::query(
                    "UPDATE workspace_leases SET native_observed_turn_id=?2 WHERE lease_id=?1 \
                    AND native_client_id IS NULL AND native_observed_turn_id IS NULL",
                )
                .bind(&id)
                .bind(observed)
                .execute(&mut *tx)
                .await?;
                release_confirmed_tx(&mut tx, &record, observed, crate::model::now_ms()).await?;
            } else {
                sqlx::query("UPDATE workspace_leases SET state='released',holder_phase='stopped', \
                    released_at_ms=?2,updated_at_ms=?2 WHERE lease_id=?1 AND native_client_id IS NULL \
                    AND native_observed_turn_id IS NULL AND state='held'")
                    .bind(&id).bind(crate::model::now_ms()).execute(&mut *tx).await?;
            }
        }
    }
    if stopped {
        confirm_stopped_read_intent_tx(&mut tx, card, cwd, crate::model::now_ms()).await?;
        let events = task_ended_tx(
            &mut tx,
            card,
            crate::operation::workspace_lease::ReleaseDelivery::CommitAsTaskEnded,
        )
        .await?;
        crate::operation::workspace_lease::append_workspace_events_tx(&mut tx, events).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Sealing and the never-issued proof share the same transaction as launch admission.
pub(super) async fn close_native_scope(pool: &SqlitePool, thread: &str) -> Result<()> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let binding:Option<(String,String,String)>=sqlx::query_as(
        "SELECT card_id,cwd,scope_phase FROM workspace_execution_bindings WHERE provider='codex' AND holder_id=?1"
    ).bind(thread).fetch_optional(&mut *tx).await?;
    if let Some((card, cwd, phase)) = binding {
        let ever_issued: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspace_leases WHERE holder_kind='native' \
            AND native_provider='codex' AND holder_id=?1)",
        )
        .bind(thread)
        .fetch_one(&mut *tx)
        .await?;
        if phase == "new" && !ever_issued {
            crate::operation::workspace_lease::execution_guard::record_stopped_native_scope_tx(
                &mut tx,
                &card,
                thread,
                NativeProvider::Codex,
                &cwd,
                WorkspaceAccess::ReadWrite,
                None,
            )
            .await?;
        }
        sqlx::query("UPDATE workspace_execution_bindings SET scope_phase='closed' WHERE provider='codex' AND holder_id=?1")
            .bind(thread).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Deleting a native projection cannot invalidate an admitted execution's durable owner.
pub(super) async fn require_worker_cleanup_tx(
    tx: &mut crate::operation::Tx<'_>,
    card: &str,
) -> Result<()> {
    let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_leases execution WHERE card_id=?1 \
        AND state IN ('held','releasing') AND ((holder_kind='native' AND native_provider='codex') OR \
        (holder_kind='terminal' AND EXISTS(SELECT 1 FROM operations operation WHERE operation.kind IN ('codex-create','codex-worker') \
        AND json_extract(operation.tx_output_json,'$.data.card_id')=?1 \
        AND json_extract(operation.tx_output_json,'$.data.terminal_id')=execution.holder_id))))")
        .bind(card).fetch_one(&mut **tx).await?;
    if live {
        return Err(crate::error::CalmError::Conflict(
            "native cleanup still owns an execution reference".into(),
        ));
    }
    let threads:Vec<String>=sqlx::query_scalar("SELECT holder_id FROM workspace_execution_bindings WHERE provider='codex' AND card_id=?1 \
        UNION SELECT thread_id FROM worker_sessions WHERE provider='codex' AND card_id=?1 AND thread_id IS NOT NULL")
        .bind(card).fetch_all(&mut **tx).await?;
    for thread in threads {
        if native_thread_requires_recovery_in(tx, &thread).await? {
            return Err(crate::error::CalmError::Conflict(
                "native cleanup scope remains unconfirmed".into(),
            ));
        }
    }
    Ok(())
}

/// Scope stop is evidence, not a retrospective inference of provider permissions.
/// Only the exact current terminal task's persisted read intent receives the handoff.
async fn confirm_stopped_read_intent_tx(
    tx: &mut crate::operation::Tx<'_>,
    card: &str,
    cwd: &str,
    now: i64,
) -> Result<()> {
    let cwd = std::fs::canonicalize(cwd)?;
    let cwd = cwd
        .to_str()
        .ok_or_else(|| crate::error::CalmError::Conflict("stopped scope is not UTF-8".into()))?;
    sqlx::query("UPDATE workspace_leases AS intent SET read_stop_confirmed_at_ms=?3 \
        WHERE intent.card_id=?1 AND intent.holder_kind='task' AND intent.access_mode='read_only' AND intent.state='held' \
        AND COALESCE(intent.canonical_path,intent.path)=?2 \
        AND EXISTS(SELECT 1 FROM current_tasks task JOIN operations attempt ON attempt.idempotency_key=task.id \
        WHERE attempt.id=intent.lease_owner AND task.worker_card_id=?1 AND task.status IN ('done','failed','canceled')) \
        AND NOT EXISTS(SELECT 1 FROM workspace_leases live WHERE live.card_id=?1 \
        AND live.holder_kind IN ('native','terminal','forge') AND live.state IN ('held','releasing'))")
        .bind(card).bind(cwd).bind(now).execute(&mut **tx).await?;
    Ok(())
}

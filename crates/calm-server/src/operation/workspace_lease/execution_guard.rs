//! Durable execution references for execution that can outlive a model turn.
use super::*;
use crate::db::sqlite::begin_immediate_tx;
use calm_types::workspace_access::{WorkspaceAccess, WorkspaceScopePhase};

/// Closed protocol identities supported by native execution producers.
#[derive(Clone, Copy)]
pub(crate) enum NativeProvider {
    Codex,
    Claude,
}
impl NativeProvider {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

/// The execution directory is persisted by the provider launch, never inferred from card ownership.
pub(crate) async fn bind_execution(
    pool: &SqlitePool,
    provider: NativeProvider,
    card: &str,
    holder: &str,
    cwd: &str,
) -> Result<()> {
    let cwd = std::fs::canonicalize(cwd)
        .map_err(|e| CalmError::Conflict(format!("execution cwd unavailable: {e}")))?;
    let cwd = cwd
        .to_str()
        .ok_or_else(|| CalmError::Conflict("execution cwd is not UTF-8".into()))?;
    let mut conn = pool.acquire().await?;
    persist_execution_scope(
        &mut conn,
        provider,
        card,
        holder,
        cwd,
        WorkspaceScopePhase::New,
    )
    .await
}

/// Producers supply provider-confirmed cwd; the binding cannot transfer to another card.
pub(crate) async fn persist_execution_scope(
    conn: &mut sqlx::SqliteConnection,
    provider: NativeProvider,
    card: &str,
    holder: &str,
    cwd: &str,
    phase: WorkspaceScopePhase,
) -> Result<()> {
    let changed = sqlx::query(
        "INSERT INTO workspace_execution_bindings(provider,holder_id,card_id,cwd,scope_phase) \
        VALUES(?1,?2,?3,?4,?5) ON CONFLICT(provider,holder_id) DO UPDATE SET cwd=excluded.cwd,scope_phase=excluded.scope_phase \
        WHERE workspace_execution_bindings.card_id=excluded.card_id",
    )
    .bind(provider.wire()).bind(holder).bind(card).bind(cwd).bind(phase.as_db_str())
    .execute(conn).await?.rows_affected();
    if changed != 1 {
        return Err(CalmError::Conflict(
            "native scope belongs to another owner".into(),
        ));
    }
    Ok(())
}

struct ExecutionLeaseGuard {
    pool: SqlitePool,
    id: String,
}
impl ExecutionLeaseGuard {
    pub(crate) async fn acquire_native(
        pool: &SqlitePool,
        card: &str,
        thread: &str,
        except_attempt: &str,
        provider: NativeProvider,
        access: WorkspaceAccess,
    ) -> Result<Self> {
        let mut tx = begin_immediate_tx(pool).await?;
        let context = native_write_context_tx(&mut tx, card, thread, provider).await?;
        let task_attempt = verify_task_intent_tx(&mut tx, card, &context.cwd, access).await?;
        let except_attempt = task_attempt.as_deref().unwrap_or(except_attempt);
        if !native_context_available_for_access_tx(&mut tx, card, except_attempt, &context, access)
            .await?
        {
            return Err(CalmError::Conflict(
                "workspace write guard is waiting for current readers or writers".into(),
            ));
        }
        let id = new_id();
        let root = context.root.as_deref().unwrap_or(&id);
        insert_execution_reference(
            &mut tx,
            ExecutionReference {
                track: &context.track,
                card,
                holder: thread,
                kind: "native",
                path: &context.cwd,
                access: match access {
                    WorkspaceAccess::ReadOnly => ExecutionAccess::Read,
                    WorkspaceAccess::ReadWrite => ExecutionAccess::Write(root),
                },
            },
            &id,
        )
        .await?;
        sqlx::query("UPDATE workspace_leases SET native_provider=?2,native_client_id=lease_id WHERE lease_id=?1")
            .bind(&id)
            .bind(provider.wire())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Self {
            pool: pool.clone(),
            id,
        })
    }
    /// Persist the exact request identity before any provider issuance; rejected requests may retry.
    pub(crate) async fn client_nonce(&self, preferred: Option<&str>) -> Result<String> {
        let nonce = preferred.unwrap_or(&self.id);
        if nonce.is_empty() {
            return Err(CalmError::BadRequest(
                "native client identity is empty".into(),
            ));
        }
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let reused:bool=sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspace_leases prior JOIN workspace_leases current \
             ON prior.holder_id=current.holder_id AND prior.native_provider=current.native_provider \
             WHERE current.lease_id=?1 AND prior.lease_id<>current.lease_id AND prior.native_client_id=?2)"
        ).bind(&self.id).bind(nonce).fetch_one(&mut *tx).await?;
        if reused {
            return Err(CalmError::Conflict(
                "native client identity was already issued".into(),
            ));
        }
        let changed = sqlx::query(
            "UPDATE workspace_leases SET native_client_id=?2 WHERE lease_id=?1 \
            AND state='held' AND holder_phase='issuing'",
        )
        .bind(&self.id)
        .bind(nonce)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(CalmError::Conflict(
                "native issuance guard changed before request".into(),
            ));
        }
        tx.commit().await?;
        Ok(nonce.to_owned())
    }
    /// Consumed only when issuance was never attempted or the provider rejected it.
    pub(crate) async fn rejected(self) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE workspace_leases SET state='released',holder_phase='stopped', \
            released_at_ms=?2,updated_at_ms=?2,native_client_id=NULL WHERE lease_id=?1 AND holder_kind='native' \
            AND state='held' AND holder_phase IN ('issuing','stopping')",
        )
        .bind(&self.id)
        .bind(now_ms())
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(CalmError::Conflict(
                "unissued guard changed before rejection".into(),
            ));
        }
        Ok(())
    }
    pub(crate) async fn started(self, turn: &str) -> Result<()> {
        if turn.is_empty() {
            return Err(CalmError::Conflict(
                "provider returned empty turn identity".into(),
            ));
        }
        let changed=sqlx::query(
            r#"
UPDATE workspace_leases SET holder_phase=CASE WHEN holder_phase='stopping' THEN 'stopping' ELSE 'running' END, lease_owner=?2,native_observed_turn_id=?2, updated_at_ms=?3 WHERE
lease_id=?1 AND state='held' AND holder_phase IN ('issuing','stopping')
"#,
        )
        .bind(&self.id)
        .bind(turn)
        .bind(now_ms())
        .execute(&self.pool)
        .await?.rows_affected();
        if changed != 1 {
            return Err(CalmError::Conflict(
                "native guard changed before issuance acknowledgment".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) struct ExecutionWriteGuard(ExecutionLeaseGuard);
impl ExecutionWriteGuard {
    pub(crate) async fn acquire_native(
        pool: &SqlitePool,
        card: &str,
        thread: &str,
        except: &str,
        provider: NativeProvider,
    ) -> Result<Self> {
        Ok(Self(
            ExecutionLeaseGuard::acquire_native(
                pool,
                card,
                thread,
                except,
                provider,
                WorkspaceAccess::ReadWrite,
            )
            .await?,
        ))
    }
    pub(crate) async fn started(self, turn: &str) -> Result<()> {
        self.0.started(turn).await
    }
    #[cfg(test)]
    pub(crate) async fn rejected(self) -> Result<()> {
        self.0.rejected().await
    }
}
pub(crate) struct ExecutionReadGuard(ExecutionLeaseGuard);
impl ExecutionReadGuard {
    pub(crate) async fn acquire_native(
        pool: &SqlitePool,
        card: &str,
        thread: &str,
        provider: NativeProvider,
    ) -> Result<Self> {
        Ok(Self(
            ExecutionLeaseGuard::acquire_native(
                pool,
                card,
                thread,
                "",
                provider,
                WorkspaceAccess::ReadOnly,
            )
            .await?,
        ))
    }
}
/// Move-only task capability; storage and physical completion are owned by one lease lifecycle.
pub(crate) enum NativeTaskGuard {
    Read(ExecutionReadGuard),
    Write(ExecutionWriteGuard),
}
impl NativeTaskGuard {
    fn lease(&self) -> &ExecutionLeaseGuard {
        match self {
            Self::Read(guard) => &guard.0,
            Self::Write(guard) => &guard.0,
        }
    }
    fn into_lease(self) -> ExecutionLeaseGuard {
        match self {
            Self::Read(guard) => guard.0,
            Self::Write(guard) => guard.0,
        }
    }
    pub(crate) async fn client_nonce(&self, preferred: Option<&str>) -> Result<String> {
        self.lease().client_nonce(preferred).await
    }
    pub(crate) async fn started(self, turn: &str) -> Result<()> {
        self.into_lease().started(turn).await
    }
    pub(crate) async fn rejected(self) -> Result<()> {
        self.into_lease().rejected().await
    }
}

/// Task-bound issuance rechecks authority and the frozen directory in the reservation transaction.
async fn verify_task_intent_tx(
    conn: &mut sqlx::SqliteConnection,
    card: &str,
    cwd: &str,
    access: WorkspaceAccess,
) -> Result<Option<String>> {
    let tasks: Vec<(String, String)> =
        sqlx::query_as("SELECT id,status FROM tasks WHERE worker_card_id=?1 LIMIT 2")
            .bind(card)
            .fetch_all(&mut *conn)
            .await?;
    if tasks.len() > 1 {
        return Err(CalmError::Conflict(
            "ambiguous native task ownership".into(),
        ));
    }
    if tasks
        .first()
        .is_some_and(|(_, state)| !matches!(state.as_str(), "dispatched" | "running" | "verifying"))
    {
        return Err(CalmError::Conflict(
            "ended or unclaimed task cannot issue native execution".into(),
        ));
    }
    let lease:Option<(String,String,String,Option<String>)>=sqlx::query_as(
        "SELECT l.access_mode,l.state,COALESCE(l.canonical_path,l.path),o.idempotency_key \
         FROM workspace_leases l LEFT JOIN operations o ON o.id=l.lease_owner \
         WHERE l.card_id=?1 AND l.holder_kind='task' ORDER BY l.created_at_ms DESC,l.lease_id DESC LIMIT 1"
    ).bind(card).fetch_optional(&mut *conn).await?;
    let Some((mode, state, path, attempt)) = lease else {
        if access == WorkspaceAccess::ReadOnly || !tasks.is_empty() {
            return Err(CalmError::Conflict(
                "native task has no held workspace intent".into(),
            ));
        }
        return Ok(None);
    };
    if state != "held"
        || mode
            != match access {
                WorkspaceAccess::ReadOnly => "read_only",
                WorkspaceAccess::ReadWrite => "read_write",
            }
    {
        return Err(CalmError::Conflict(
            "native task workspace intent closed or changed access".into(),
        ));
    }
    if std::fs::canonicalize(path)? != std::fs::canonicalize(cwd)? {
        return Err(CalmError::Conflict(
            "native binding differs from the frozen task directory".into(),
        ));
    }
    if let Some((task, _)) = tasks.first() {
        if attempt.as_deref() != Some(task.as_str()) {
            return Err(CalmError::Conflict(
                "native task intent is not held by its claimed attempt".into(),
            ));
        }
        return Ok(Some(task.clone()));
    }
    if access == WorkspaceAccess::ReadOnly {
        return Err(CalmError::Conflict(
            "native read intent requires an actual claimed task".into(),
        ));
    }
    Ok(None)
}

/// A live descendant retains its authenticated root even after the root's own execution stopped.
/// Card ownership alone grants nothing: the root must be a task/native writer of this resource.
pub(crate) async fn owned_write_root_tx(
    conn: &mut sqlx::SqliteConnection,
    card: &str,
    cwd: &str,
) -> Result<Option<String>> {
    let root=sqlx::query_scalar(
        "SELECT origin.lease_id FROM workspace_leases descendant \
         JOIN workspace_leases origin ON origin.lease_id=descendant.write_root_id \
         WHERE descendant.state IN ('held','releasing') AND descendant.access_mode='read_write' \
         AND origin.card_id=?1 AND origin.access_mode='read_write' \
         AND origin.holder_kind IN ('task','native') AND COALESCE(origin.canonical_path,origin.path)=?2 \
         AND (COALESCE(descendant.canonical_path,descendant.path)=?2 OR ?2='/' \
         OR COALESCE(descendant.canonical_path,descendant.path)='/' \
         OR substr(COALESCE(descendant.canonical_path,descendant.path),1,length(?2)+1)=?2||'/' \
         OR substr(?2,1,length(COALESCE(descendant.canonical_path,descendant.path))+1) \
         =COALESCE(descendant.canonical_path,descendant.path)||'/') \
         ORDER BY origin.created_at_ms DESC,origin.lease_id DESC LIMIT 1"
    ).bind(card).bind(cwd).fetch_optional(&mut *conn).await?;
    Ok(root)
}

struct NativeWriteContext {
    track: String,
    cwd: String,
    root: Option<String>,
}
async fn native_write_context_tx(
    conn: &mut sqlx::SqliteConnection,
    card: &str,
    thread: &str,
    provider: NativeProvider,
) -> Result<NativeWriteContext> {
    let (track, cwd): (String, String) = sqlx::query_as(
        "SELECT c.track_id,b.cwd FROM workspace_execution_bindings b \
         JOIN cards c ON c.id=b.card_id WHERE b.provider=?1 AND b.holder_id=?2 AND b.card_id=?3 \
         AND b.scope_phase IN ('new','ready')",
    )
    .bind(provider.wire())
    .bind(thread)
    .bind(card)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| CalmError::Conflict("native execution has no bound workspace".into()))?;
    let cwd = std::fs::canonicalize(cwd)
        .map_err(|e| CalmError::Conflict(format!("native workspace unavailable: {e}")))?
        .to_str()
        .ok_or_else(|| CalmError::Conflict("native workspace is not UTF-8".into()))?
        .to_owned();
    let root = owned_write_root_tx(conn, card, &cwd).await?;
    Ok(NativeWriteContext { track, cwd, root })
}
async fn native_context_available_for_access_tx(
    conn: &mut sqlx::SqliteConnection,
    card: &str,
    except: &str,
    context: &NativeWriteContext,
    access: WorkspaceAccess,
) -> Result<bool> {
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspace_leases WHERE holder_kind='native' \
         AND card_id=?1 AND state IN ('held','releasing'))",
    )
    .bind(card)
    .fetch_one(&mut *conn)
    .await?;
    if duplicate {
        return Ok(false);
    }
    Ok(crate::db::sqlite::workspace_available(
        conn,
        &context.track,
        except,
        access,
        Some(&context.cwd),
        if access == WorkspaceAccess::ReadWrite {
            context.root.as_deref()
        } else {
            None
        },
    )
    .await?)
}
/// Queue preflight consumes the same bound cwd and lineage rules as atomic native acquisition.
pub(crate) async fn native_write_available_tx(
    conn: &mut sqlx::SqliteConnection,
    card: &str,
    thread: &str,
    except: &str,
    provider: NativeProvider,
) -> Result<bool> {
    let context = native_write_context_tx(conn, card, thread, provider).await?;
    native_context_available_for_access_tx(conn, card, except, &context, WorkspaceAccess::ReadWrite)
        .await
}

/// The caller's transaction reserves the Track checkout before spawning any write execution.
pub(crate) async fn acquire_execution_write_tx(
    tx: &mut Tx<'_>,
    track: &str,
    card: &str,
    holder: &str,
    kind: &str,
    cwd: &Path,
) -> Result<String> {
    if !matches!(kind, "native" | "terminal" | "forge") {
        return Err(CalmError::Internal("invalid execution guard kind".into()));
    }
    let path = std::fs::canonicalize(cwd).map_err(|error| {
        CalmError::Conflict(format!("workspace guard path unavailable: {error}"))
    })?;
    let path = path
        .to_str()
        .ok_or_else(|| CalmError::Conflict("workspace guard path is not UTF-8".into()))?;
    let id = new_id();
    let parent = owned_write_root_tx(tx, card, path).await?;
    let root = parent.unwrap_or_else(|| id.clone());
    insert_execution_reference(
        tx,
        ExecutionReference {
            track,
            card,
            holder,
            kind,
            path,
            access: ExecutionAccess::Write(&root),
        },
        &id,
    )
    .await?;
    Ok(id)
}

struct ExecutionReference<'a> {
    track: &'a str,
    card: &'a str,
    holder: &'a str,
    kind: &'a str,
    path: &'a str,
    access: ExecutionAccess<'a>,
}
enum ExecutionAccess<'a> {
    Read,
    Write(&'a str),
}
enum InitialExecutionState {
    Issuing,
    Stopped,
}
async fn insert_execution_reference(
    tx: &mut Tx<'_>,
    reference: ExecutionReference<'_>,
    id: &str,
) -> Result<()> {
    insert_execution_reference_at(tx, reference, id, InitialExecutionState::Issuing).await
}
async fn insert_execution_reference_at(
    tx: &mut Tx<'_>,
    reference: ExecutionReference<'_>,
    id: &str,
    initial: InitialExecutionState,
) -> Result<()> {
    let (state, phase, released) = match initial {
        InitialExecutionState::Issuing => ("held", "issuing", None),
        InitialExecutionState::Stopped => ("released", "stopped", Some(now_ms())),
    };
    let ExecutionReference {
        track,
        card,
        holder,
        kind,
        path,
        access,
    } = reference;
    let (mode, root) = match access {
        ExecutionAccess::Read => ("read_only", None),
        ExecutionAccess::Write(root) => ("read_write", Some(root)),
    };
    sqlx::query(
        r#"
INSERT INTO workspace_leases(lease_id, card_id, track_id, path, state, lease_owner, boot_id,
created_at_ms, updated_at_ms, access_mode, holder_kind, holder_id, holder_phase,write_root_id,released_at_ms) VALUES(?1, ?2,
?3, ?4, ?11, ?5, ?6, ?7, ?7, ?10, ?8, ?5, ?12,?9,?13)
"#,
    )
    .bind(&id)
    .bind(card)
    .bind(track)
    .bind(path)
    .bind(holder)
    .bind(read_boot_id())
    .bind(now_ms())
    .bind(kind)
    .bind(root)
    .bind(mode)
    .bind(state)
    .bind(phase)
    .bind(released)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A probe receives a child reference only from its specific live Forge parent.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn acquire_execution_child_tx(
    tx: &mut Tx<'_>,
    track: &str,
    card: &str,
    holder: &str,
    kind: &str,
    cwd: &Path,
    parent: &str,
    expected_owner: &str,
) -> Result<String> {
    super::super::owned_parked::require_owner_tx(tx, parent, expected_owner).await?;
    if kind != "forge" {
        return Err(CalmError::Internal(
            "unsupported child execution kind".into(),
        ));
    }
    let path = std::fs::canonicalize(cwd).map_err(|error| {
        CalmError::Conflict(format!("child execution cwd unavailable: {error}"))
    })?;
    let path = path
        .to_str()
        .ok_or_else(|| CalmError::Conflict("child execution cwd is not UTF-8".into()))?;
    let root: String = sqlx::query_scalar(
        "SELECT write_root_id FROM workspace_leases WHERE holder_kind='forge' AND holder_id=?1 \
         AND card_id=?2 AND track_id=?3 AND state='held' AND path=?4",
    )
    .bind(parent)
    .bind(card)
    .bind(track)
    .bind(path)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::Conflict("child execution has no matching live parent".into()))?;
    let id = new_id();
    insert_execution_reference(
        tx,
        ExecutionReference {
            track,
            card,
            holder,
            kind,
            path,
            access: ExecutionAccess::Write(&root),
        },
        &id,
    )
    .await?;
    sqlx::query("UPDATE workspace_leases SET execution_parent_holder_id=?2 WHERE lease_id=?1")
        .bind(&id)
        .bind(parent)
        .execute(&mut **tx)
        .await?;
    Ok(id)
}

/// Only a provider's confirmed process stop may call this; completion notifications are insufficient.
pub(crate) async fn release_stopped_execution(
    pool: &SqlitePool,
    kind: &str,
    holder: &str,
) -> Result<()> {
    let mut tx = begin_immediate_tx(pool).await?;
    release_stopped_execution_tx(&mut tx, kind, holder).await?;
    tx.commit().await?;
    Ok(())
}

/// The caller has confirmed the execution stopped; one state transition serves all providers.
pub(crate) async fn release_stopped_execution_tx(
    tx: &mut Tx<'_>,
    kind: &str,
    holder: &str,
) -> Result<()> {
    sqlx::query(
        r#"
UPDATE workspace_leases SET state='released', holder_phase='stopped', released_at_ms=?3,
updated_at_ms=?3 WHERE holder_kind=?1 AND holder_id=?2 AND state IN ('held','releasing')
"#,
    )
    .bind(kind)
    .bind(holder)
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn record_execution_artifacts(
    pool: &SqlitePool,
    kind: &str,
    holder: &str,
    artifacts: &super::super::SpawnArtifacts,
) -> Result<()> {
    let value = serde_json::to_string(artifacts)?;
    let changed=sqlx::query("UPDATE workspace_leases SET execution_artifacts_json=?3,holder_phase='running',updated_at_ms=?4 \
        WHERE holder_kind=?1 AND holder_id=?2 AND state='held'")
        .bind(kind).bind(holder).bind(value).bind(now_ms()).execute(pool).await?.rows_affected();
    if changed != 1 {
        return Err(CalmError::Conflict(
            "execution guard is not held while recording spawn".into(),
        ));
    }
    Ok(())
}

/// Captured by an authenticated caller; remains separate from user request idempotency.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWriteOrigin {
    pub(crate) lease_id: String,
    pub(crate) card_id: String,
}
pub(crate) async fn capture_write_origin(
    pool: &SqlitePool,
    card: &str,
) -> Result<WorkspaceWriteOrigin> {
    let lease_id:String=sqlx::query_scalar(
        "SELECT lease_id FROM workspace_leases WHERE card_id=?1 AND access_mode='read_write' \
         AND state='held' AND holder_kind='native' ORDER BY created_at_ms DESC,lease_id DESC LIMIT 1"
    ).bind(card).fetch_optional(pool).await?
        .ok_or_else(||CalmError::Conflict("terminal delegation requires its caller's held write guard".into()))?;
    Ok(WorkspaceWriteOrigin {
        lease_id,
        card_id: card.to_owned(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn acquire_execution_delegated_tx(
    tx: &mut Tx<'_>,
    track: &str,
    card: &str,
    holder: &str,
    kind: &str,
    cwd: &Path,
    origin: &WorkspaceWriteOrigin,
) -> Result<String> {
    let (root, parent_path): (String, String) = sqlx::query_as(
        "SELECT write_root_id,path FROM workspace_leases WHERE lease_id=?1 AND card_id=?2 \
         AND access_mode='read_write' AND state='held' AND holder_kind='native'",
    )
    .bind(&origin.lease_id)
    .bind(&origin.card_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::Conflict("delegated execution lost its parent write guard".into()))?;
    let path = std::fs::canonicalize(cwd)
        .map_err(|e| CalmError::Conflict(format!("delegated cwd unavailable: {e}")))?;
    let path = path
        .to_str()
        .ok_or_else(|| CalmError::Conflict("delegated cwd is not UTF-8".into()))?;
    if !Path::new(path).starts_with(&parent_path) && !Path::new(&parent_path).starts_with(path) {
        return acquire_execution_write_tx(tx, track, card, holder, kind, cwd).await;
    }
    let id = new_id();
    insert_execution_reference(
        tx,
        ExecutionReference {
            track,
            card,
            holder,
            kind,
            path,
            access: ExecutionAccess::Write(&root),
        },
        &id,
    )
    .await?;
    Ok(id)
}

#[path = "execution_guard/legacy.rs"]
mod legacy;
pub(crate) use legacy::{adopt_native_scope_tx, record_stopped_terminal_tx};

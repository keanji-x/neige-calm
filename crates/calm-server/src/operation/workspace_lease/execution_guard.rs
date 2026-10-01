//! Durable write references for execution that can outlive a model turn.
use super::*;
use crate::db::sqlite::begin_immediate_tx;
use calm_types::workspace_access::WorkspaceAccess;

/// Never cloned or released by Drop: an uncertain provider outcome retains its lease.
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
    sqlx::query(
        "INSERT INTO workspace_execution_bindings(provider,holder_id,card_id,cwd) \
        VALUES(?1,?2,?3,?4) ON CONFLICT(provider,holder_id) DO UPDATE SET cwd=excluded.cwd \
        WHERE workspace_execution_bindings.card_id=excluded.card_id",
    )
    .bind(provider.wire())
    .bind(holder)
    .bind(card)
    .bind(cwd)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) struct ExecutionWriteGuard {
    pool: SqlitePool,
    id: String,
}
impl ExecutionWriteGuard {
    pub(crate) async fn acquire_native(
        pool: &SqlitePool,
        card: &str,
        thread: &str,
        except_attempt: &str,
        provider: NativeProvider,
    ) -> Result<Self> {
        let mut tx = begin_immediate_tx(pool).await?;
        let context = native_write_context_tx(&mut tx, card, thread, provider).await?;
        if !native_context_available_tx(&mut tx, card, except_attempt, &context).await? {
            return Err(CalmError::Conflict(
                "workspace write guard is waiting for current readers or writers".into(),
            ));
        }
        let id = acquire_execution_write_tx(
            &mut tx,
            &context.track,
            card,
            thread,
            "native",
            Path::new(&context.cwd),
        )
        .await?;
        sqlx::query("UPDATE workspace_leases SET native_provider=?2 WHERE lease_id=?1")
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
    /// Consumed only when issuance was never attempted or the provider rejected it.
    pub(crate) async fn rejected(self) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE workspace_leases SET state='released',holder_phase='stopped', \
            released_at_ms=?2,updated_at_ms=?2 WHERE lease_id=?1 AND holder_kind='native' \
            AND state='held' AND holder_phase='issuing'",
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
        sqlx::query(
            r#"
UPDATE workspace_leases SET holder_phase='running', lease_owner=?2, updated_at_ms=?3 WHERE
lease_id=?1 AND state='held' AND holder_phase='issuing'
"#,
        )
        .bind(&self.id)
        .bind(turn)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
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
         JOIN cards c ON c.id=b.card_id WHERE b.provider=?1 AND b.holder_id=?2 AND b.card_id=?3",
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
async fn native_context_available_tx(
    conn: &mut sqlx::SqliteConnection,
    card: &str,
    except: &str,
    context: &NativeWriteContext,
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
        WorkspaceAccess::ReadWrite,
        Some(&context.cwd),
        context.root.as_deref(),
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
    native_context_available_tx(conn, card, except, &context).await
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
            root: &root,
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
    root: &'a str,
}
async fn insert_execution_reference(
    tx: &mut Tx<'_>,
    reference: ExecutionReference<'_>,
    id: &str,
) -> Result<()> {
    let ExecutionReference {
        track,
        card,
        holder,
        kind,
        path,
        root,
    } = reference;
    sqlx::query(
        r#"
INSERT INTO workspace_leases(lease_id, card_id, track_id, path, state, lease_owner, boot_id,
created_at_ms, updated_at_ms, access_mode, holder_kind, holder_id, holder_phase,write_root_id) VALUES(?1, ?2,
?3, ?4, 'held', ?5, ?6, ?7, ?7, 'read_write', ?8, ?5, 'issuing',?9)
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
            root: &root,
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
            root: &root,
        },
        &id,
    )
    .await?;
    Ok(id)
}

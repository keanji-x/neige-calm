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
        let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
            .bind(card)
            .fetch_one(&mut *tx)
            .await?;
        if !crate::db::sqlite::track_available(
            &mut tx,
            &track,
            except_attempt,
            WorkspaceAccess::ReadWrite,
        )
        .await?
        {
            return Err(CalmError::Conflict(
                "workspace write guard is waiting for current readers or writers".into(),
            ));
        }
        let duplicate: bool = sqlx::query_scalar(
            r#"
SELECT EXISTS(SELECT 1 FROM workspace_leases WHERE holder_kind='native' AND card_id=?1 AND
state IN ('held', 'releasing'))
"#,
        )
        .bind(card)
        .fetch_one(&mut *tx)
        .await?;
        if duplicate {
            return Err(CalmError::Conflict(
                "native execution still holds its write guard".into(),
            ));
        }
        let cwd:String=sqlx::query_scalar("SELECT cwd FROM workspace_execution_bindings WHERE provider=?1 AND holder_id=?2 AND card_id=?3")
            .bind(provider.wire()).bind(thread).bind(card).fetch_optional(&mut *tx).await?
            .ok_or_else(||CalmError::Conflict("native execution has no bound workspace".into()))?;
        let id =
            acquire_execution_write_tx(&mut tx, &track, card, thread, "native", Path::new(&cwd))
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
    let parent:Option<String>=sqlx::query_scalar(
        "SELECT write_root_id FROM workspace_leases WHERE card_id=?1 AND access_mode='read_write' \
         AND state='held' AND holder_kind IN ('task','native') AND COALESCE(canonical_path,path)=?2 \
         ORDER BY created_at_ms DESC LIMIT 1"
    ).bind(card).bind(path).fetch_optional(&mut **tx).await?.flatten();
    let root = parent.unwrap_or_else(|| id.clone());
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
    Ok(id)
}

/// Only a provider's confirmed process stop may call this; completion notifications are insufficient.
pub(crate) async fn release_stopped_execution(
    pool: &SqlitePool,
    kind: &str,
    holder: &str,
) -> Result<()> {
    sqlx::query(
        r#"
UPDATE workspace_leases SET state='released', holder_phase='stopped', released_at_ms=?3,
updated_at_ms=?3 WHERE holder_kind=?1 AND holder_id=?2 AND state IN ('held', 'releasing')
"#,
    )
    .bind(kind)
    .bind(holder)
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

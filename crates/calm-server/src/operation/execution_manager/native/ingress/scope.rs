use super::*;

pub(super) async fn prepare(
    pool: &SqlitePool,
    record: &Record,
    socket: &Path,
    provider: &Path,
) -> Result<Scope> {
    let canonical = std::fs::canonicalize(&record.cwd)?;
    let cwd = canonical
        .to_str()
        .ok_or_else(|| CalmError::Conflict("native workspace is not UTF-8".into()))?
        .to_owned();
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let row: (String, String, String) = sqlx::query_as(
        "SELECT lease.card_id,lease.track_id,terminal.cwd FROM workspace_leases lease \
         JOIN terminals terminal ON terminal.id=lease.holder_id AND terminal.card_id=lease.card_id \
         JOIN cards card ON card.id=lease.card_id AND card.track_id=lease.track_id \
         WHERE lease.lease_id=?1 AND lease.holder_kind='terminal' AND lease.state='held' \
         AND lease.holder_phase='issuing'",
    )
    .bind(&record.id)
    .fetch_one(&mut *tx)
    .await?;
    if std::fs::canonicalize(&row.2)? != Path::new(&cwd) {
        return Err(CalmError::Conflict(
            "managed session scope changed before preparation".into(),
        ));
    }
    let permissions = PermissionsChoice::SandboxMode("workspace-write".into());
    sqlx::query(
        "INSERT INTO native_session_ingresses(session_execution_id,terminal_id,card_id,track_id,canonical_cwd, \
         socket_path,provider_socket_path,permissions_json,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) \
         ON CONFLICT(session_execution_id) DO NOTHING"
    ).bind(&record.id).bind(&record.holder).bind(&row.0).bind(&row.1).bind(&cwd)
        .bind(socket.to_string_lossy().as_ref()).bind(provider.to_string_lossy().as_ref())
        .bind(serde_json::to_string(&permissions)?).bind(crate::model::now_ms()).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO native_session_threads(session_execution_id,thread_id) SELECT ?1,holder_id \
         FROM workspace_execution_bindings WHERE provider='codex' AND card_id=?2 AND cwd=?3 \
         AND scope_phase IN ('new','ready') ON CONFLICT DO NOTHING",
    )
    .bind(&record.id)
    .bind(&row.0)
    .bind(&cwd)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO native_session_executions(session_execution_id,execution_id) SELECT ?1,lease_id \
         FROM workspace_leases WHERE holder_kind='native' AND native_provider='codex' AND card_id=?2 \
         AND COALESCE(canonical_path,path)=?3 AND state='held' \
         AND NOT EXISTS(SELECT 1 FROM native_session_executions WHERE execution_id=lease_id)"
    ).bind(&record.id).bind(&row.0).bind(&cwd).execute(&mut *tx).await?;
    tx.commit().await?;
    let stored = load(pool, &record.id).await?;
    if stored.card != row.0
        || stored.track != row.1
        || stored.cwd != cwd
        || stored.terminal != record.holder
        || stored.socket != socket
        || stored.provider != provider
        || stored.permissions != permissions
    {
        return Err(CalmError::Conflict(
            "native session preparation differs from its frozen plan".into(),
        ));
    }
    Ok(stored)
}

pub(super) async fn load(pool: &SqlitePool, execution: &str) -> Result<Scope> {
    let row: (String, String, String, String, String, String, String) = sqlx::query_as(
        "SELECT terminal_id,card_id,track_id,canonical_cwd,socket_path,provider_socket_path,permissions_json \
         FROM native_session_ingresses WHERE session_execution_id=?1"
    ).bind(execution).fetch_optional(pool).await?
        .ok_or_else(|| CalmError::Conflict("native session has no managed ingress checkpoint".into()))?;
    Ok(Scope {
        execution: execution.into(),
        terminal: row.0,
        card: row.1,
        track: row.2,
        cwd: row.3,
        socket: row.4.into(),
        provider: row.5.into(),
        permissions: serde_json::from_str(&row.6)?,
    })
}

pub(super) async fn require_open(pool: &SqlitePool, scope: &Scope) -> Result<()> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT card.track_id,terminal.card_id,terminal.cwd,COALESCE(lease.canonical_path,lease.path) \
         FROM workspace_leases lease JOIN native_session_ingresses ingress ON ingress.session_execution_id=lease.lease_id \
         JOIN cards card ON card.id=ingress.card_id JOIN terminals terminal ON terminal.id=ingress.terminal_id \
         WHERE lease.lease_id=?1 AND lease.state='held' AND lease.holder_phase IN ('issuing','running') \
         AND lease.card_id=ingress.card_id AND lease.holder_id=ingress.terminal_id \
         AND NOT EXISTS(SELECT 1 FROM native_session_client_observations observation \
         WHERE observation.session_execution_id=ingress.session_execution_id AND observation.proof='unmanaged-client')"
    ).bind(&scope.execution).fetch_optional(pool).await?;
    let Some((track, card, terminal_cwd, lease_cwd)) = row else {
        return Err(CalmError::Conflict(
            "native session admission is closed".into(),
        ));
    };
    if track != scope.track
        || card != scope.card
        || std::fs::canonicalize(terminal_cwd)? != Path::new(&scope.cwd)
        || std::fs::canonicalize(lease_cwd)? != Path::new(&scope.cwd)
    {
        return Err(CalmError::Conflict(
            "native session owner or workspace changed".into(),
        ));
    }
    Ok(())
}

pub(super) async fn require_thread(pool: &SqlitePool, scope: &Scope, thread: &str) -> Result<()> {
    let binding: Option<(String, String, String)> = sqlx::query_as(
        "SELECT card_id,cwd,scope_phase FROM workspace_execution_bindings WHERE provider='codex' AND holder_id=?1"
    ).bind(thread).fetch_optional(pool).await?;
    match binding {
        Some((card, cwd, phase))
            if card == scope.card
                && matches!(phase.as_str(), "new" | "ready")
                && std::fs::canonicalize(&cwd)? == Path::new(&scope.cwd) =>
        {
            Ok(())
        }
        Some((card, _, _)) => Err(CalmError::Conflict(format!(
            "resume belongs to another managed scope; open card {card}"
        ))),
        None => Err(CalmError::Conflict(
            "resume has no managed owner; open its owning card".into(),
        )),
    }
}

pub(super) async fn associate_thread(pool: &SqlitePool, scope: &Scope, thread: &str) -> Result<()> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let changed = sqlx::query(
        "INSERT INTO native_session_threads(session_execution_id,thread_id) \
         SELECT ingress.session_execution_id,binding.holder_id FROM native_session_ingresses ingress \
         JOIN workspace_leases lease ON lease.lease_id=ingress.session_execution_id \
         JOIN cards card ON card.id=ingress.card_id AND card.track_id=ingress.track_id \
         JOIN terminals terminal ON terminal.id=ingress.terminal_id AND terminal.card_id=ingress.card_id \
         JOIN workspace_execution_bindings binding ON binding.provider='codex' AND binding.card_id=ingress.card_id \
         AND binding.cwd=ingress.canonical_cwd AND binding.scope_phase IN ('new','ready') \
         WHERE ingress.session_execution_id=?1 AND binding.holder_id=?2 AND lease.state='held' \
         AND lease.holder_phase IN ('issuing','running') \
         ON CONFLICT(session_execution_id,thread_id) DO UPDATE SET thread_id=excluded.thread_id"
    ).bind(&scope.execution).bind(thread).execute(&mut *tx).await?.rows_affected();
    if changed != 1 {
        return Err(CalmError::Conflict(
            "native thread association is closed".into(),
        ));
    }
    tx.commit().await?;
    Ok(())
}

pub(super) async fn admit_execution_tx(
    tx: &mut crate::operation::Tx<'_>,
    owner: &super::super::super::Owner,
    execution: &str,
    expected: Option<&str>,
) -> Result<()> {
    let cwd: String = sqlx::query_scalar(
        "SELECT COALESCE(canonical_path,path) FROM workspace_leases WHERE lease_id=?1",
    )
    .bind(execution)
    .fetch_one(&mut **tx)
    .await?;
    let Some(group) = live_group(tx, owner, &cwd, expected).await? else {
        return Ok(());
    };
    let inserted = sqlx::query(
        "INSERT INTO native_session_threads(session_execution_id,thread_id) \
         SELECT ingress.session_execution_id,binding.holder_id FROM native_session_ingresses ingress \
         JOIN cards card ON card.id=ingress.card_id AND card.track_id=ingress.track_id \
         JOIN terminals terminal ON terminal.id=ingress.terminal_id AND terminal.card_id=ingress.card_id \
         JOIN workspace_execution_bindings binding ON binding.provider='codex' AND binding.card_id=ingress.card_id \
         AND binding.cwd=ingress.canonical_cwd AND binding.scope_phase IN ('new','ready') \
         WHERE ingress.session_execution_id=?1 AND binding.holder_id=?2 \
         ON CONFLICT(session_execution_id,thread_id) DO UPDATE SET thread_id=excluded.thread_id"
    ).bind(&group).bind(&owner.holder).execute(&mut **tx).await?.rows_affected();
    if inserted != 1 {
        return Err(CalmError::Conflict(
            "native session frozen owner or scope changed".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO native_session_executions(session_execution_id,execution_id) VALUES(?1,?2)",
    )
    .bind(&group)
    .bind(execution)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn live_group(
    tx: &mut crate::operation::Tx<'_>,
    owner: &super::super::super::Owner,
    cwd: &str,
    expected: Option<&str>,
) -> Result<Option<String>> {
    let groups: Vec<(String,String,bool)> = sqlx::query_as(
        "SELECT ingress.session_execution_id,parent.holder_phase, \
         card.id IS NOT NULL AND terminal.id IS NOT NULL AND card.track_id=ingress.track_id \
         AND terminal.card_id=ingress.card_id AND parent.card_id=ingress.card_id \
         FROM native_session_ingresses ingress JOIN workspace_leases parent \
         ON parent.lease_id=ingress.session_execution_id LEFT JOIN cards card ON card.id=ingress.card_id \
         LEFT JOIN terminals terminal ON terminal.id=ingress.terminal_id \
         WHERE ingress.card_id=?1 AND ingress.canonical_cwd=?2 AND parent.state='held' LIMIT 2"
    ).bind(&owner.card).bind(cwd).fetch_all(&mut **tx).await?;
    match groups.as_slice() {
        [] if expected.is_none() => Ok(None),
        [(id, phase, true)]
            if expected.is_none_or(|expected| expected == id)
                && (phase == "running" || (expected.is_none() && phase == "issuing")) =>
        {
            Ok(Some(id.clone()))
        }
        _ => Err(CalmError::Conflict(
            "native session group is closed, ambiguous or differs from its caller".into(),
        )),
    }
}
pub(super) async fn available_tx(
    tx: &mut crate::operation::Tx<'_>,
    owner: &super::super::super::Owner,
) -> Result<bool> {
    let cwd: Option<String> = sqlx::query_scalar(
        "SELECT cwd FROM workspace_execution_bindings WHERE provider='codex' AND holder_id=?1 AND card_id=?2"
    ).bind(&owner.holder).bind(&owner.card).fetch_optional(&mut **tx).await?;
    let Some(cwd) = cwd else {
        return Ok(false);
    };
    let canonical = std::fs::canonicalize(cwd)?;
    let cwd = canonical
        .to_str()
        .ok_or_else(|| CalmError::Conflict("native scope is not UTF-8".into()))?;
    match live_group(tx, owner, cwd, None).await {
        Ok(_) => Ok(true),
        Err(CalmError::Conflict(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

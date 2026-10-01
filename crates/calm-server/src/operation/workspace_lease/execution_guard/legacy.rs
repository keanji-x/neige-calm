use super::*;

/// Resume completes resource discovery atomically with reservation or positive stop evidence.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn adopt_native_scope_tx(
    tx: &mut Tx<'_>,
    card: &str,
    holder: &str,
    provider: NativeProvider,
    cwd: &str,
    access: WorkspaceAccess,
    observed_turn: Option<&str>,
    stopped: bool,
) -> Result<()> {
    let path = std::fs::canonicalize(cwd)?;
    let path = path
        .to_str()
        .ok_or_else(|| CalmError::Conflict("native discovered cwd is not UTF-8".into()))?;
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(card)
        .fetch_one(&mut **tx)
        .await?;
    let held: Option<(String, String)> = sqlx::query_as(
        "SELECT lease_id,path FROM workspace_leases WHERE holder_kind='native' AND native_provider=?1 AND \
        holder_id=?2 AND card_id=?3 AND state IN ('held','releasing') LIMIT 1",
    )
    .bind(provider.wire())
    .bind(holder)
    .bind(card)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((lease, held)) = held {
        if std::fs::canonicalize(held)? != std::fs::canonicalize(path)? {
            return Err(CalmError::Conflict(
                "resumed native scope differs from held execution".into(),
            ));
        }
        if let Some(turn) = observed_turn {
            // Only an unidentified legacy reference may adopt a provider-observed turn.
            // Requests with a nonce retain their exact request identity across resume.
            sqlx::query(
                "UPDATE workspace_leases SET native_observed_turn_id=?2,lease_owner=?2, \
                holder_phase=CASE WHEN holder_phase='stopping' THEN 'stopping' ELSE 'running' END \
                WHERE lease_id=?1 AND native_client_id IS NULL AND native_observed_turn_id IS NULL",
            )
            .bind(lease)
            .bind(turn)
            .execute(&mut **tx)
            .await?;
        }
    } else if !stopped {
        let id = new_id();
        let root = owned_write_root_tx(tx, card, path)
            .await?
            .unwrap_or_else(|| id.clone());
        insert_execution_reference(
            tx,
            ExecutionReference {
                track: &track,
                card,
                holder,
                kind: "native",
                path,
                access: match access {
                    WorkspaceAccess::ReadOnly => ExecutionAccess::Read,
                    WorkspaceAccess::ReadWrite => ExecutionAccess::Write(&root),
                },
            },
            &id,
        )
        .await?;
        sqlx::query("UPDATE workspace_leases SET native_provider=?2,native_observed_turn_id=?3,lease_owner=COALESCE(?3,holder_id), \
            holder_phase=CASE WHEN ?3 IS NULL THEN 'issuing' ELSE 'running' END WHERE lease_id=?1")
            .bind(&id).bind(provider.wire()).bind(observed_turn).execute(&mut **tx).await?;
    }
    persist_execution_scope(tx, provider, card, holder, path, WorkspaceScopePhase::Ready).await
}

/// The caller must hold the supervisor's positive StopAndConfirm proof for this immutable holder.
/// Recording evidence starts released, so existing readers never conflict with a stopped writer.
pub(crate) async fn record_stopped_terminal_tx(
    tx: &mut Tx<'_>,
    track: &str,
    card: &str,
    holder: &str,
    cwd: &Path,
) -> Result<()> {
    let owner: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cards WHERE id=?1 AND track_id=?2)")
            .bind(card)
            .bind(track)
            .fetch_one(&mut **tx)
            .await?;
    if !owner {
        return Err(CalmError::Conflict(
            "stopped terminal owner differs from its track".into(),
        ));
    }
    if !cwd.is_absolute() {
        return Err(CalmError::Conflict(
            "stopped terminal requires its persisted absolute cwd".into(),
        ));
    }
    // Positive stop belongs to the immutable holder, even if its saved directory was removed.
    let path = match std::fs::canonicalize(cwd) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => cwd.to_path_buf(),
        Err(error) => return Err(error.into()),
    };
    let path = path
        .to_str()
        .ok_or_else(|| CalmError::Conflict("stopped terminal cwd is not UTF-8".into()))?;
    let id = new_id();
    insert_execution_reference_at(
        tx,
        ExecutionReference {
            track,
            card,
            holder,
            kind: "terminal",
            path,
            access: ExecutionAccess::Write(&id),
        },
        &id,
        InitialExecutionState::Stopped,
    )
    .await
}

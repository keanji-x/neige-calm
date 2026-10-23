//! Disposable checkout lifecycle. No process result is inferred from checkout files.
use super::*;

pub(super) async fn create(frozen: &Frozen) -> Result<()> {
    let frozen = frozen.clone();
    tokio::task::spawn_blocking(move || {
        // Without an execution record, a previous boot may have interrupted
        // checkout halfway through a filter. Never accept that partial tree.
        if frozen.cwd.exists() {
            workspace::worktree::remove_worktree(&frozen.repo, &frozen.cwd)?;
        }
        workspace::worktree::detached_worktree_with_timeout(
            &frozen.repo,
            &frozen.cwd,
            &frozen.revision,
            std::time::Duration::from_secs(frozen.timeout_secs.min(30) as u64),
        )
    })
    .await
    .map_err(|e| CalmError::Internal(e.to_string()))?
    .map_err(|e| CalmError::Internal(e.to_string()))
}
pub(crate) async fn remove(frozen: &Frozen) -> Result<()> {
    let frozen = frozen.clone();
    tokio::task::spawn_blocking(move || {
        workspace::worktree::remove_worktree(&frozen.repo, &frozen.cwd)
    })
    .await
    .map_err(|e| CalmError::Internal(e.to_string()))?
    .map_err(|e| CalmError::Internal(e.to_string()))
}

/// The projector's boot/tick reconciliation retries cleanup after a crash between
/// operation settlement and unlink. Live rows retain their directories.
pub(crate) async fn cleanup_track(
    db: &calm_truth::db::sqlite::SqliteDb,
    track: &str,
) -> Result<()> {
    let outputs: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,tx_output_json FROM operations WHERE kind=? AND target_type='track' \
         AND target_id=? AND phase IN ('succeeded','failed') AND tx_output_json IS NOT NULL \
         AND json_extract(tx_output_json,'$.data.checkout_removed') IS NOT 1",
    )
    .bind(KIND)
    .bind(track)
    .fetch_all(db.read_pool())
    .await?;
    for (id, raw) in outputs {
        let output: TxOutput = serde_json::from_str(&raw)?;
        let frozen = Frozen::from_output(&output)?;
        if let Err(error) = cleanup_operation(db, &id, &frozen).await {
            // Cleanup must not prevent bounded admission retries or Planner wakes.
            // Leave the acknowledgement absent for the next reconciliation.
            tracing::warn!(op_id=%id, %error, "objective checkout cleanup deferred");
        }
    }
    Ok(())
}

pub(super) async fn cleanup_operation(
    db: &calm_truth::db::sqlite::SqliteDb,
    id: &str,
    frozen: &Frozen,
) -> Result<()> {
    remove(frozen).await?;
    sqlx::query("UPDATE operations SET \
         tx_output_json=json_set(tx_output_json,'$.data.checkout_removed',json('true')) WHERE id=? AND \
         kind=? AND phase IN ('succeeded','failed')")
        .bind(id).bind(KIND).execute(db.writer_pool()).await?;
    Ok(())
}

//! Provision before launching Claude, using the same Git isolation as Codex.
use super::*;
use crate::operation::workspace_lease::{WorkspaceLeaseTarget, provision_workspace_worktree};

pub(super) async fn provision(
    adapter: &ClaudeWorkerAdapter,
    ctx: &SpawnCtx,
    output: &TxOutput,
) -> Result<()> {
    let repo_root = output.output_optional_string("repo_root", "claude-worker")?;
    let branch = output.output_optional_string("slice_branch", "claude-worker")?;
    let (repo_root, branch) = match (repo_root, branch) {
        (Some(root), Some(branch)) => (root, branch),
        // Frozen pre-upgrade operations own plain directories. Recovery must
        // preserve their recorded cwd, not invent a new execution workspace.
        (None, None) => return Ok(()),
        _ => {
            return Err(CalmError::Internal(
                "incomplete Claude worktree target".into(),
            ));
        }
    };
    let path = output.output_string("cwd", "claude-worker")?;
    provision_workspace_worktree(&WorkspaceLeaseTarget {
        repo_root: PathBuf::from(repo_root),
        path: PathBuf::from(&path),
        branch,
    })?;
    let card_id = output.output_string("card_id", "claude-worker")?;
    let track_id = output.output_string("track_id", "claude-worker")?;
    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.clone()),
        TrackId::from(track_id.clone()),
    )
    .await?;
    let write = WriteContext::new(
        adapter.card_role_cache.clone(),
        adapter.track_area_cache.clone(),
    );
    let recorded_result = write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::KernelDispatcher,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                // Retries may provision the same worktree more than once. The
                // ready event, unlike the filesystem check, is committed once.
                let recorded: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE kind = 'worktree.provisioned' \
                 AND json_extract(payload, '$.card_id') = ?1)",
                )
                .bind(&card_id)
                .fetch_one(&mut **tx)
                .await?;
                let events = if recorded {
                    return Err(CalmError::Conflict(
                        "claude workspace already recorded".into(),
                    ));
                } else {
                    vec![(
                        scope,
                        Event::WorktreeProvisioned {
                            track_id: TrackId::from(track_id),
                            card_id: CardId::from(card_id),
                            path,
                        },
                    )]
                };
                Ok(((), events))
            })
        },
    )
    .await;
    match recorded_result {
        Ok(_) => Ok(()),
        Err(CalmError::Conflict(reason)) if reason == "claude workspace already recorded" => Ok(()),
        Err(error) => Err(error),
    }
}

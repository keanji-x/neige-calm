//! Transactional facts and typed receipts for shared runtime worktree reclamation.
use crate::{
    error::{CalmError, Result},
    event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION},
    ids::ActorId,
};
use calm_truth::db::sqlite::{
    SqliteDb, append_decision_event_in_tx, begin_immediate_tx,
    worktree_events::unmatched_provisioned_worktrees,
};
use std::path::{Path, PathBuf};
use workspace_runtime::reclaim::{ExtraWorktree, ReclaimReceipts};

#[derive(Clone)]
pub(crate) struct ServerReclaimPorts {
    pub db: SqliteDb,
    pub events: EventBus,
}

#[async_trait::async_trait]
impl ReclaimReceipts for ServerReclaimPorts {
    type Error = CalmError;
    async fn refresh(
        &self,
        target: &ExtraWorktree,
    ) -> Result<Option<workspace::reclaim::ReclaimRequest>> {
        let mut conn = self.db.read_pool().acquire().await?;
        let claims = unmatched_provisioned_worktrees(&mut conn).await?;
        if !claims.iter().any(|r| r.event_id == target.provision_id) {
            return Ok(None);
        }
        let (protected, terminal) = protection_facts(&mut conn).await?;
        let mut request = target.request.clone();
        request.unknown_terminal = terminal;
        request.protected.extend(protected);
        request.protected.extend(
            claims
                .iter()
                .filter(|r| r.event_id != target.provision_id)
                .map(|r| PathBuf::from(&r.path)),
        );
        Ok(Some(request))
    }

    async fn removed(&self, target: &ExtraWorktree) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.db).await?;
        // Do not cancel a newer lifecycle if an external producer changed the event stream.
        if !unmatched_provisioned_worktrees(&mut tx)
            .await?
            .iter()
            .any(|r| r.event_id == target.provision_id)
        {
            return Err(CalmError::Conflict(
                "extra worktree receipt changed during removal".into(),
            ));
        }
        let actor = ActorId::KernelDispatcher;
        let scope = EventScope::Card {
            card: target.card_id.clone().into(),
            track: target.track_id.clone().into(),
            area: target.area_id.clone().into(),
        };
        let event = Event::WorktreeRemoved {
            track_id: target.track_id.clone().into(),
            card_id: target.card_id.clone().into(),
            path: target.request.path.to_string_lossy().into_owned(),
        };
        let id = append_decision_event_in_tx(&mut tx, &actor, &scope, None, &event).await?;
        tx.commit().await?;
        self.events.emit_envelope(BroadcastEnvelope {
            id,
            actor,
            scope,
            event,
            event_version: SYNC_EVENT_VERSION,
        });
        Ok(())
    }
}

impl workspace_runtime::sweep::CandidateRefs for ServerReclaimPorts {
    fn delete_for_track(&self, track_id: &str, dirs: &[PathBuf]) {
        crate::git_candidate::refs::delete_candidate_refs_for_track(
            track_id,
            dirs.iter().map(PathBuf::as_path),
        );
    }
}

/// Read before rows or leases disappear; the returned values require no post-delete join.
pub(crate) async fn snapshot(
    conn: &mut sqlx::SqliteConnection,
    track_id: &str,
    deleting: &[String],
) -> Result<Vec<ExtraWorktree>> {
    let Some(track) =
        sqlx::query_as::<_, calm_truth::db::rows::TrackRow>("SELECT * FROM tracks WHERE id=?")
            .bind(track_id)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(Vec::new());
    };
    let track = crate::model::Track::from(track);
    let claims = unmatched_provisioned_worktrees(conn).await?;
    let (mut protected, terminal) = protection_facts(conn).await?;
    let mut excluded_refs = Vec::new();
    for id in deleting {
        excluded_refs.extend(removed_refs(id)?);
    }
    // A primary checkout is never a removable linked tree (.git must be a regular file).
    // Other claims protect their own path and every ancestor, including unknown relative claims.
    protected.extend(
        claims
            .iter()
            .filter(|r| r.track_id != track_id)
            .map(|r| PathBuf::from(&r.path)),
    );
    Ok(claims
        .iter()
        .filter(|r| r.track_id == track_id)
        .map(|r| {
            let path = PathBuf::from(&r.path);
            let mut guards = protected.clone();
            guards.extend(
                claims
                    .iter()
                    .filter(|other| other.event_id != r.event_id)
                    .map(|other| PathBuf::from(&other.path)),
            );
            ExtraWorktree {
                provision_id: r.event_id,
                track_id: r.track_id.clone(),
                card_id: r.card_id.clone(),
                area_id: r.area_id.clone(),
                request: workspace::reclaim::ReclaimRequest {
                    repo: PathBuf::from(&track.workspace.path),
                    path,
                    protected: guards,
                    excluded_refs: excluded_refs.clone(),
                    unknown_terminal: terminal,
                    ownership_conflict: claims
                        .iter()
                        .any(|other| other.track_id != track_id && other.path == r.path),
                },
            }
        })
        .collect())
}

/// Existing kernel teardown/recycle is forceful. Refuse before any external effects if an
/// outstanding extra claim (of any track) could be swallowed by an ancestor being removed.
/// Historical relative claims have no trustworthy anchor and conservatively block deletion.
pub(crate) async fn protect_delete(
    conn: &mut sqlx::SqliteConnection,
    workspace: &crate::model::TrackWorkspace,
) -> Result<()> {
    use calm_types::model::WorkspaceIsolation;
    if workspace.isolation() == WorkspaceIsolation::None {
        return Ok(());
    }
    let root = Path::new(workspace.agent_cwd());
    let real = std::fs::canonicalize(root).ok();
    for claim in unmatched_provisioned_worktrees(conn).await? {
        let path = Path::new(&claim.path);
        if !path.is_absolute()
            || path.starts_with(root)
            || real.as_ref().is_some_and(|r| {
                std::fs::canonicalize(path)
                    .ok()
                    .is_some_and(|p| p.starts_with(r))
            })
        {
            tracing::warn!(path=%path.display(), "delete retained ancestor of extra worktree");
            return Err(CalmError::Conflict("workspace contains an outstanding extra worktree; remove it safely before retrying deletion".into()));
        }
    }
    Ok(())
}

/// Declared by the same owners that perform the ensuing branch/ref deletion.
pub(crate) fn removed_refs(id: &str) -> Result<Vec<String>> {
    Ok(std::iter::once(format!(
        "refs/heads/{}",
        workspace::worktree::track_branch_for(id)?
    ))
    .chain(crate::git_candidate::refs::teardown_ref_prefixes(id))
    .collect())
}

async fn protection_facts(conn: &mut sqlx::SqliteConnection) -> Result<(Vec<PathBuf>, bool)> {
    let protected: Vec<PathBuf> = sqlx::query_scalar::<_, String>(
        "SELECT workspace_path FROM tracks \
         UNION SELECT workspace_worktree_path FROM tracks WHERE workspace_worktree_path IS NOT NULL \
         UNION SELECT path FROM workspace_leases WHERE state NOT IN ('released','lost')",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(PathBuf::from)
    .collect();
    let terminal: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM terminals WHERE exit_code IS NULL AND signal_killed=0)",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok((protected, terminal))
}

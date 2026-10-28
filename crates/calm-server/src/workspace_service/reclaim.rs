//! Transactional facts and typed receipts for shared runtime worktree reclamation.
pub(crate) mod operation;
use crate::{
    error::{CalmError, Result},
    event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION},
    ids::ActorId,
};
use calm_truth::db::sqlite::{
    SqliteDb, append_decision_event_in_tx, worktree_events::unmatched_provisioned_worktrees,
};
use std::path::{Path, PathBuf};
use workspace_runtime::reclaim::{ExtraWorktree, ReclaimReceipts};

#[derive(Clone)]
pub(crate) struct ServerReclaimPorts {
    pub runtime: std::sync::Arc<crate::operation::OperationRuntime>,
    pub db: SqliteDb,
}

#[async_trait::async_trait]
impl ReclaimReceipts for ServerReclaimPorts {
    type Error = CalmError;
    async fn reclaim(
        &self,
        target: &ExtraWorktree,
        observe: workspace_runtime::track_worktree::ObservePoint,
    ) -> Result<()> {
        operation::run(self.runtime.reclaim_context(), target, observe).await
    }
}

pub(super) async fn refresh(
    db: &SqliteDb,
    target: &ExtraWorktree,
) -> Result<Option<workspace::reclaim::ReclaimRequest>> {
    let mut conn = db.read_pool().acquire().await?;
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
    tx: &mut crate::operation::Tx<'_>,
    track_id: &str,
    deleting: &[String],
) -> Result<Vec<ExtraWorktree>> {
    let conn = &mut **tx;
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
    let targets: Vec<_> = claims
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
                operation_id: format!(
                    "workspace-reclaim:{}:{}",
                    r.event_id,
                    if deleting.is_empty() {
                        "close"
                    } else {
                        "delete"
                    }
                ),
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
        .collect();
    for target in &targets {
        operation::register(tx, target, !deleting.is_empty()).await?;
    }
    Ok(targets)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::location_admission::LocationAdmission;

    #[tokio::test]
    async fn reclaim_owned_workspace_locations_registered_before_admission() {
        let repo = calm_truth::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let target = ExtraWorktree {
            operation_id: "reclaim-registration".into(),
            provision_id: 1,
            track_id: "track".into(),
            card_id: "card".into(),
            area_id: "area".into(),
            request: workspace::reclaim::ReclaimRequest {
                repo: root.path().into(),
                path: root.path().join("extra"),
                protected: Vec::new(),
                excluded_refs: Vec::new(),
                unknown_terminal: false,
                ownership_conflict: false,
            },
        };
        let mut tx = repo.pool().begin().await.unwrap();
        operation::register(&mut tx, &target, true).await.unwrap();
        tx.commit().await.unwrap();
        let admission = LocationAdmission {
            managed_root: root.path().join("managed"),
            objective_checkout_root: root.path().join("objective"),
            teardown_targets: Default::default(),
            area_locks: crate::per_card_lock::new_keyed_locks(),
            track_locks: crate::per_card_lock::new_keyed_locks(),
        };
        let registration = admission.teardown_targets.registration();
        registration.register_location(&target.track_id, target.request.path.clone());
        let mut conn = repo.pool().acquire().await.unwrap();
        let fence = admission
            .try_fence(&mut conn, &target.request.repo, Some(&target.track_id))
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        fence.recheck(&mut tx).await.unwrap();
        tx.rollback().await.unwrap();
        // Before execution admission, the teardown registry supplies the track owner.
        let _held =
            crate::per_card_lock::try_lock_key(&admission.track_locks, &target.track_id).unwrap();
        assert!(matches!(
            admission
                .try_fence(&mut conn, &target.request.path, None)
                .await,
            Err(CalmError::WorkspaceBusy(_))
        ));
    }
}

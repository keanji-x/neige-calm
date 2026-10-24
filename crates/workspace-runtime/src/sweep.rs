//! Postcommit teardown policy.
use crate::track_worktree::{self, ObservePoint};
use calm_types::model::{TrackWorkspace, WorkspaceIsolation};
use std::path::PathBuf;

/// In-process ownership of worktrees whose deleting rows may already be gone.
/// The short std mutex is never held across an await or a filesystem operation.
#[derive(Clone, Debug, Default)]
pub struct TeardownTargets(std::sync::Arc<std::sync::Mutex<TargetCounts>>);
type TargetCounts = std::collections::BTreeMap<(String, PathBuf), usize>;

impl TeardownTargets {
    pub fn snapshot(&self) -> Vec<PathBuf> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .map(|(_, p)| p.clone())
            .collect()
    }

    pub fn registration(&self) -> TeardownRegistration {
        TeardownRegistration(std::sync::Arc::new(Registration {
            targets: self.clone(),
            roots: std::sync::Mutex::new(Vec::new()),
        }))
    }
}

#[derive(Debug)]
struct Registration {
    targets: TeardownTargets,
    roots: std::sync::Mutex<Vec<(String, PathBuf)>>,
}

/// Clones keep registration alive through transaction rollback compensation. On
/// success the last guard belongs to the blocking sweep, even if its await is cancelled.
#[derive(Clone, Debug)]
pub struct TeardownRegistration(std::sync::Arc<Registration>);

impl TeardownRegistration {
    pub fn register(&self, sweep: &WorkspaceTrackSweep) {
        let mut paths: Vec<PathBuf> = sweep
            .extra_worktrees
            .iter()
            .map(|target| target.request.path.clone())
            .filter(|path| path.is_absolute())
            .collect();
        if sweep.workspace.isolation() == WorkspaceIsolation::Worktree {
            paths.push(PathBuf::from(sweep.workspace.agent_cwd()));
        }
        for path in paths {
            let key = (sweep.track_id.clone(), path);
            *self
                .0
                .targets
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(key.clone())
                .or_default() += 1;
            self.0
                .roots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(key);
        }
    }

    pub fn sweeps(self, sweeps: Vec<WorkspaceTrackSweep>) -> RegisteredSweeps {
        RegisteredSweeps {
            sweeps,
            registration: self,
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut targets = self.targets.0.lock().unwrap_or_else(|e| e.into_inner());
        for key in self
            .roots
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            let count = targets.get_mut(key).expect("registered teardown target");
            *count -= 1;
            if *count == 0 {
                targets.remove(key);
            }
        }
    }
}

/// A sweep requires a registration acquired before its delete commits.
pub struct RegisteredSweeps {
    sweeps: Vec<WorkspaceTrackSweep>,
    registration: TeardownRegistration,
}

pub trait CandidateRefs: crate::reclaim::ReclaimReceipts + Clone + Send + Sync + 'static {
    fn delete_for_track(&self, track_id: &str, git_common_dirs: &[PathBuf]);
}

/// What a track teardown removes after its commit (D10): the track worktree and its branch
/// (#1830 S1), and the candidate refs in every common dir a lease of the track recorded.
#[derive(Clone, Debug)]
pub struct WorkspaceTrackSweep {
    pub track_id: String,
    /// Workspace decoded from the track row inside the deletion transaction.
    pub workspace: TrackWorkspace,
    pub git_common_dirs: Vec<PathBuf>,
    pub extra_worktrees: Vec<crate::reclaim::ExtraWorktree>,
    pub removed_refs: Vec<String>,
}

/// The post-commit half of a track or area teardown, best effort: the track worktree, then the
/// candidate refs, addressed by the lease rows' common dirs (not by the track cwd, which may be a
/// moved linked worktree). Never fails the caller: the track rows are already gone.
pub async fn sweep_workspace_worktrees_for_tracks<P: CandidateRefs>(
    sweeps: RegisteredSweeps,
    ports: P,
    observe: ObservePoint,
) {
    if let Err(error) = tokio::spawn(async move {
        let RegisteredSweeps {
            sweeps,
            registration,
        } = sweeps;
        // A ref removed by any member of this area sweep cannot protect another member's HEAD.
        let excluded: Vec<String> = sweeps.iter().flat_map(|s| s.removed_refs.clone()).collect();
        for mut sweep in sweeps {
            for target in &mut sweep.extra_worktrees {
                target.request.excluded_refs.extend(excluded.clone());
            }

            if let Err(error) = crate::reclaim::reclaim_extra_worktrees(
                sweep.extra_worktrees.clone(),
                &ports,
                observe,
            )
            .await
            {
                tracing::warn!(%error, "extra worktree sweep failed");
            }
            let ports = ports.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || {
                sweep_workspace_worktrees_blocking(vec![sweep], ports, observe)
            })
            .await
            {
                tracing::warn!(%error, "track teardown Git sweep task failed");
            }
        }
        drop(registration);
    })
    .await
    {
        tracing::warn!(%error, "track teardown Git sweep task failed");
    }
}

fn sweep_workspace_worktrees_blocking<P: CandidateRefs>(
    sweeps: Vec<WorkspaceTrackSweep>,
    ports: P,
    observe: ObservePoint,
) {
    for sweep in sweeps {
        match sweep.workspace.isolation() {
            WorkspaceIsolation::Managed => {}
            WorkspaceIsolation::Worktree => {
                track_worktree::remove_track_worktree(&sweep.track_id, &sweep.workspace, observe)
            }
            // Guarantee G: only historical Neige-owned refs below may be removed.
            WorkspaceIsolation::None => {}
        }
        ports.delete_for_track(&sweep.track_id, &sweep.git_common_dirs);
    }
}

#[cfg(test)]
mod isolation_none_tests {
    use super::*;
    use calm_types::model::TrackWorkspaceKind;

    #[test]
    fn isolation_none_teardown_registration_is_counted_and_rollback_scoped() {
        let targets = TeardownTargets::default();
        let sweep = WorkspaceTrackSweep {
            track_id: "owner".into(),
            workspace: TrackWorkspace {
                kind: TrackWorkspaceKind::Attached,
                path: "/repo".into(),
                frozen_at: Some(1),
                worktree: Some("/owned/worktree".into()),
            },
            git_common_dirs: Vec::new(),
            extra_worktrees: Vec::new(),
            removed_refs: Vec::new(),
        };
        let first = targets.registration();
        let rollback = targets.registration();
        first.register(&sweep);
        rollback.register(&sweep);
        assert_eq!(targets.snapshot(), vec![PathBuf::from("/owned/worktree")]);
        drop(rollback);
        assert_eq!(targets.snapshot(), vec![PathBuf::from("/owned/worktree")]);
        let last = first.clone();
        drop(first);
        assert_eq!(targets.snapshot(), vec![PathBuf::from("/owned/worktree")]);
        drop(last);
        assert!(targets.snapshot().is_empty());
        let mut none = sweep;
        none.workspace.worktree = None;
        let guard = targets.registration();
        guard.register(&none);
        assert!(targets.snapshot().is_empty());
    }
}

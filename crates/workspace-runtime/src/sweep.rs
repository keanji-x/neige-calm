//! Scoped ownership while logical retirement commits. Physical cleanup is deferred.
use calm_types::model::{TrackWorkspace, WorkspaceIsolation};
use std::path::PathBuf;

/// In-process ownership of worktrees whose deleting rows may already be gone.
/// The short std mutex is never held across an await or a filesystem operation.
#[derive(Clone, Debug, Default)]
pub struct TeardownTargets(std::sync::Arc<std::sync::Mutex<TargetCounts>>);
type TargetCounts = std::collections::BTreeMap<(String, PathBuf), usize>;

impl TeardownTargets {
    pub fn snapshot_with_owners(&self) -> Vec<(String, PathBuf)> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

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
            .cloned()
            .filter(|path| path.is_absolute())
            .collect();
        if sweep.workspace.isolation() == WorkspaceIsolation::Worktree {
            paths.push(PathBuf::from(sweep.workspace.agent_cwd()));
        }
        for path in paths {
            self.register_location(&sweep.track_id, path);
        }
    }

    /// Close/recovery reclamation uses the same counted ownership as post-delete sweeps.
    pub fn register_location(&self, track: &str, path: PathBuf) {
        let key = (track.to_owned(), path);
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

    pub fn sweeps(self, sweeps: Vec<WorkspaceTrackSweep>) -> RegisteredSweeps {
        RegisteredSweeps {
            _sweeps: sweeps,
            _registration: self,
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
    _sweeps: Vec<WorkspaceTrackSweep>,
    _registration: TeardownRegistration,
}

/// What a track teardown removes after its commit (D10): the track worktree and its branch
/// (#1830 S1), and the candidate refs in every common dir a lease of the track recorded.
#[derive(Clone, Debug)]
pub struct WorkspaceTrackSweep {
    pub track_id: String,
    /// Workspace decoded from the track row inside the deletion transaction.
    pub workspace: TrackWorkspace,
    pub git_common_dirs: Vec<PathBuf>,
    pub extra_worktrees: Vec<PathBuf>,
    pub removed_refs: Vec<String>,
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

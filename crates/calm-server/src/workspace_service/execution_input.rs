//! A declared original directory input and its actual retained executor objects.
//! Logical selection validation and physical FD qualification are separate facts.

use crate::{
    db::sqlite::{WorkspaceObservation, WorkspaceOwnership, workspace_observation_snapshot},
    error::{CalmError, Result},
    operation::Tx,
};
use calm_session::control::execution::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use workspace::{
    directory::DirectoryIdentity,
    execution::objects::HeldSourceObjects,
    identity::{WorkspaceCwd, WorkspaceId},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum WorkspaceInputOwnership {
    Managed,
    TrackWorktree,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum WorkspaceExecutionInput {
    SelectedWorktree {
        track_id: String,
        workspace_id: WorkspaceId,
        selection_version: i64,
        path: String,
        ownership: WorkspaceInputOwnership,
    },
    ExternalDirectory {
        path: String,
    },
}

impl WorkspaceExecutionInput {
    pub(crate) fn selected(snapshot: &WorkspaceObservation) -> Result<Self> {
        let ownership = match snapshot.ownership {
            WorkspaceOwnership::Managed => WorkspaceInputOwnership::Managed,
            WorkspaceOwnership::TrackWorktree => WorkspaceInputOwnership::TrackWorktree,
            WorkspaceOwnership::Borrowed => {
                return Err(CalmError::Conflict(
                    "borrowed directory requires its declared ExternalDirectory contract".into(),
                ));
            }
        };
        Ok(Self::SelectedWorktree {
            track_id: snapshot.track_id.clone(),
            workspace_id: snapshot.workspace_id.clone(),
            selection_version: snapshot.selection_version,
            path: snapshot.selected_path.clone(),
            ownership,
        })
    }

    pub(crate) fn path(&self) -> &str {
        match self {
            Self::SelectedWorktree { path, .. } | Self::ExternalDirectory { path } => path,
        }
    }

    pub(crate) async fn require_current_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        if let Self::SelectedWorktree { track_id, .. } = self {
            let current = workspace_observation_snapshot(tx, track_id).await?;
            if Self::selected(&current)? != *self {
                return Err(CalmError::Conflict(
                    "original workspace selection changed".into(),
                ));
            }
        }
        Ok(())
    }

    /// This opens the declared contract only. It never upgrades an external
    /// directory by sniffing .git or downgrades a failed worktree qualification.
    pub(crate) fn open(&self) -> Result<OpenedExecutionInput> {
        use std::os::unix::fs::MetadataExt;
        let path = Path::new(self.path());
        if !path.is_absolute() {
            return Err(CalmError::BadRequest(
                "registered cwd must be an absolute declared directory".into(),
            ));
        }
        let ownership = match self {
            Self::SelectedWorktree {
                track_id,
                ownership,
                ..
            } => {
                let identity = DirectoryIdentity::open(path, true)?;
                InputOwnershipObservation::Owned(match ownership {
                    WorkspaceInputOwnership::Managed => identity.verify_managed_owner(track_id)?,
                    WorkspaceInputOwnership::TrackWorktree => {
                        identity.verify_track_worktree_owner(track_id)?
                    }
                })
            }
            Self::ExternalDirectory { .. } => InputOwnershipObservation::External,
        };
        let objects = HeldSourceObjects::open(
            path,
            matches!(ownership, InputOwnershipObservation::Owned(_)),
        )?;
        if let InputOwnershipObservation::Owned(identity) = &ownership {
            identity.validate()?;
            let root = objects.root().metadata()?;
            if i64::try_from(root.dev()).ok() != Some(identity.device())
                || i64::try_from(root.ino()).ok() != Some(identity.inode())
            {
                return Err(CalmError::Conflict(
                    "workspace changed while retaining its declared objects".into(),
                ));
            }
            objects.qualify_local().map_err(|error| {
                CalmError::Conflict(format!("workspace object qualification: {error:?}"))
            })?;
        }
        Ok(OpenedExecutionInput {
            objects,
            _ownership: ownership,
        })
    }

    pub(crate) fn fresh_registration(
        &self,
        objects: &HeldSourceObjects,
        authority: ExecutionAuthority,
    ) -> Result<(ExecutionScope, ExecutionRegistration)> {
        use std::os::unix::fs::MetadataExt;
        let metadata = objects.root().metadata()?;
        let root = DirectoryRegistration {
            path: Path::new(self.path()).into(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        Ok(match self {
            Self::SelectedWorktree {
                workspace_id,
                selection_version,
                ..
            } => {
                let binding_id = WorkspaceBindingId::fresh()?;
                let binding_version = u64::try_from(*selection_version).map_err(|_| {
                    CalmError::Conflict("invalid original selection version".into())
                })?;
                // This is the first installed live source, not restoration from
                // a serialized inode. Lost continuity requires explicit recovery.
                (
                    ExecutionScope::Worktree {
                        workspace_id: workspace_id.clone(),
                        binding_id: binding_id.clone(),
                        binding_version,
                        generation: 1,
                        install_epoch: 1,
                        cwd: WorkspaceCwd::root(),
                    },
                    ExecutionRegistration::Worktree {
                        workspace_id: workspace_id.clone(),
                        root,
                        binding_id,
                        binding_version,
                        generation: 1,
                        install_epoch: 1,
                        admission_open: true,
                        authority,
                    },
                )
            }
            Self::ExternalDirectory { .. } => {
                let directory_id = ExternalDirectoryId::fresh()?;
                (
                    ExecutionScope::ExternalDir {
                        directory_id: directory_id.clone(),
                        registration_version: 1,
                        cwd: WorkspaceCwd::root(),
                    },
                    ExecutionRegistration::ExternalDir {
                        directory_id,
                        root,
                        registration_version: 1,
                        admission_open: true,
                        authority,
                    },
                )
            }
        })
    }
}

/// Both real directory observations remain alive through the actual transfer.
/// This is not namespace exclusion, RuntimeSafety, or a Ready receipt.
pub(crate) struct OpenedExecutionInput {
    pub(crate) objects: HeldSourceObjects,
    _ownership: InputOwnershipObservation,
}

enum InputOwnershipObservation {
    External,
    Owned(DirectoryIdentity),
    Transferred,
}

impl OpenedExecutionInput {
    pub(crate) fn validate(&self) -> Result<()> {
        if let InputOwnershipObservation::Owned(identity) = &self._ownership {
            identity.validate()?;
        }
        self.objects.validate()?;
        if matches!(
            self.objects.shape().layout,
            workspace::execution::objects::ObjectLayout::Worktree { .. }
        ) {
            self.objects.qualify_local().map_err(|error| {
                CalmError::Conflict(format!("retained input qualification: {error:?}"))
            })?;
        }
        Ok(())
    }
    pub(crate) fn transferred(objects: HeldSourceObjects) -> Self {
        Self {
            objects,
            _ownership: InputOwnershipObservation::Transferred,
        }
    }
}

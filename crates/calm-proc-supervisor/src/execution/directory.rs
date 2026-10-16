use calm_session::control::execution::*;
use std::{fs::File, os::unix::fs::MetadataExt};
use workspace::{
    execution::objects::{HeldSourceObjects, ObjectLayout},
    identity::WorkspaceCwd,
};

pub(super) struct RegisteredRoot {
    pub registration: ExecutionRegistration,
    pub receipt: ObjectRegistrationReceipt,
    pub objects: HeldSourceObjects,
}
impl RegisteredRoot {
    pub fn receive(
        receipt: ObjectRegistrationReceipt,
        objects: HeldSourceObjects,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            receipt.id == receipt.transfer.registration_id
                && receipt.contract == EXECUTION_ENVELOPE_VERSION,
            "attachment receipt provenance mismatch"
        );
        let registration = receipt.transfer.registration.clone();
        let (expected, worktree) = match &registration {
            ExecutionRegistration::Worktree { root, .. } => (root, true),
            ExecutionRegistration::ExternalDir { root, .. } => (root, false),
        };
        anyhow::ensure!(
            expected.path == objects.shape().root
                && receipt.transfer.shape == *objects.shape()
                && worktree == matches!(objects.shape().layout, ObjectLayout::Worktree { .. })
                && worktree
                    == matches!(
                        receipt.transfer.incarnation,
                        SourceObjectIncarnation::Worktree(_)
                    ),
            "attachment resource/layout/incarnation mismatch"
        );
        let metadata = objects.root().metadata()?;
        anyhow::ensure!(
            expected.device == metadata.dev() && expected.inode == metadata.ino(),
            "registered root differs from transferred object"
        );
        let root = Self {
            registration,
            receipt,
            objects,
        };
        root.validate()?;
        Ok(root)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        self.objects.validate()?;
        Ok(())
    }
    /// Resolve and retain each component beneath the actual SCM_RIGHTS root; no later path chdir.
    pub fn cwd(&self, cwd: &WorkspaceCwd) -> anyhow::Result<File> {
        Ok(self.objects.open_cwd(cwd)?)
    }
}

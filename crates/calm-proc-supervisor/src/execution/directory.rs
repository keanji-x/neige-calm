use calm_session::control::execution::*;
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::MetadataExt,
};
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
        self.validate()?;
        let mut directory = self.objects.root().try_clone()?;
        for part in cwd.as_str().split('/').filter(|part| *part != ".") {
            let name = std::ffi::CString::new(part)?;
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            anyhow::ensure!(
                fd >= 0,
                "open registered cwd: {}",
                std::io::Error::last_os_error()
            );
            directory = unsafe { File::from_raw_fd(fd) };
        }
        Ok(directory)
    }
}

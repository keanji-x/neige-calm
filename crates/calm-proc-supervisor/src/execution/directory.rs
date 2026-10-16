use std::{
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

use calm_session::control::execution::{DirectoryRegistration, ExecutionRegistration};
use workspace::{directory::DirectoryIdentity, identity::WorkspaceCwd};

pub(super) struct RegisteredRoot {
    pub registration: ExecutionRegistration,
    root: File,
    git: Option<DirectoryIdentity>,
}

impl RegisteredRoot {
    pub fn open(registration: ExecutionRegistration) -> anyhow::Result<Self> {
        let (expected, worktree) = match &registration {
            ExecutionRegistration::Worktree { root, .. } => (root, true),
            ExecutionRegistration::ExternalDir { root, .. } => (root, false),
        };
        anyhow::ensure!(
            expected.path.is_absolute(),
            "registered root must be absolute"
        );
        let git = if worktree {
            Some(DirectoryIdentity::open(&expected.path, true)?)
        } else {
            None
        };
        let root = open_directory(&expected.path)?;
        verify(&root, expected)?;
        let registered = Self {
            registration,
            root,
            git,
        };
        registered.validate()?;
        Ok(registered)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let expected = match &self.registration {
            ExecutionRegistration::Worktree { root, .. }
            | ExecutionRegistration::ExternalDir { root, .. } => root,
        };
        verify(&self.root, expected)?;
        verify(&open_directory(&expected.path)?, expected)?;
        if let Some(git) = &self.git {
            git.validate()?;
        }
        Ok(())
    }

    /// Each component stays descriptor-relative; the returned fd is the actual child chdir target.
    pub fn cwd(&self, cwd: &WorkspaceCwd) -> anyhow::Result<File> {
        self.validate()?;
        let mut directory = self.root.try_clone()?;
        for part in cwd.as_str().split('/').filter(|part| *part != ".") {
            let name = std::ffi::CString::new(part)?;
            // SAFETY: directory and name remain alive, and a successful fd is uniquely owned.
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
            // SAFETY: successful openat transferred this new descriptor.
            directory = unsafe { File::from_raw_fd(fd) };
        }
        Ok(directory)
    }
}

fn open_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn verify(directory: &File, expected: &DirectoryRegistration) -> anyhow::Result<()> {
    let actual = directory.metadata()?;
    anyhow::ensure!(
        actual.is_dir() && actual.dev() == expected.device && actual.ino() == expected.inode,
        "registered directory identity changed; recovery required"
    );
    Ok(())
}

//! Controlled directory facts for kernel registration. No admission policy lives here.

use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use crate::{
    base,
    command::neige_git_command,
    error::{Error, Result},
    identity::WorkspaceCwd,
};

/// An open Git worktree root, including the kernel-selected cwd relative to that root.
/// Holding the descriptor lets registration verify that the named root was not replaced.
pub struct DirectoryIdentity {
    directory: File,
    selected_path: PathBuf,
    root: PathBuf,
    common_dir: PathBuf,
    cwd: WorkspaceCwd,
    device: i64,
    inode: i64,
    owner: Option<String>,
}

impl DirectoryIdentity {
    /// Missing, never-registered selections can remain Pending without creating a directory.
    /// Other failures are not absence and must be handled explicitly by the kernel.
    pub fn open_if_present(selected: &Path, require_root: bool) -> Result<Option<Self>> {
        if !selected.is_absolute() {
            return Err(Error::Internal(
                "workspace selection must be an absolute directory".into(),
            ));
        }
        match std::fs::symlink_metadata(selected) {
            Ok(_) => Self::open(selected, require_root).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Internal(format!(
                "probe workspace directory {}: {error}",
                selected.display()
            ))),
        }
    }

    /// Git reports the owning worktree root. Distinct linked worktrees share a common-dir,
    /// but retain distinct root directory identity. Subdirs and symlinks converge here.
    pub fn open(selected: &Path, require_root: bool) -> Result<Self> {
        if !selected.is_absolute() {
            return Err(Error::Internal(
                "workspace selection must be an absolute directory".into(),
            ));
        }
        let selected_path = selected.to_path_buf();
        let selected = std::fs::canonicalize(selected).map_err(|error| {
            Error::Internal(format!(
                "workspace directory {} requires recovery: {error}",
                selected.display()
            ))
        })?;
        let output = neige_git_command()
            .arg("-C")
            .arg(&selected)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|error| Error::Internal(format!("probe workspace Git root: {error}")))?;
        if !output.status.success() {
            return Err(crate::git::git_failed(
                "git rev-parse --show-toplevel",
                &selected,
                &output,
            ));
        }
        let reported = base::printed_path(&output.stdout);
        let root = std::fs::canonicalize(&reported).map_err(|error| {
            Error::Internal(format!(
                "workspace Git root {} requires recovery: {error}",
                reported.display()
            ))
        })?;
        if require_root && selected != root {
            return Err(Error::Internal(
                "owned workspace directory is not its Git worktree root".into(),
            ));
        }
        let relative = selected.strip_prefix(&root).map_err(|_| {
            Error::Internal("selected workspace cwd is outside the Git-reported root".into())
        })?;
        let cwd = if relative.as_os_str().is_empty() {
            WorkspaceCwd::root()
        } else {
            WorkspaceCwd::try_from(base::utf8_path(relative, "workspace relative cwd")?.to_owned())?
        };
        base::utf8_path(&root, "workspace canonical root")?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&root)
            .map_err(|error| Error::Internal(format!("open workspace root: {error}")))?;
        let metadata = directory
            .metadata()
            .map_err(|error| Error::Internal(format!("stat workspace root: {error}")))?;
        let common_dir = base::lease_git_common_dir(&root)?;
        let identity = Self {
            directory,
            selected_path,
            root,
            common_dir,
            cwd,
            device: i64::try_from(metadata.dev())
                .map_err(|_| Error::Internal("workspace device is out of range".into()))?,
            inode: i64::try_from(metadata.ino())
                .map_err(|_| Error::Internal("workspace inode is out of range".into()))?,
            owner: None,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn selected_path(&self) -> &Path {
        &self.selected_path
    }
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }
    pub fn cwd(&self) -> &WorkspaceCwd {
        &self.cwd
    }
    pub fn device(&self) -> i64 {
        self.device
    }
    pub fn inode(&self) -> i64 {
        self.inode
    }

    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    pub fn verify_managed_owner(mut self, owner: &str) -> Result<Self> {
        if !crate::materialize::owned_by(&self.root, owner) {
            return Err(Error::Internal(
                "managed workspace ownership requires recovery".into(),
            ));
        }
        self.owner = Some(owner.into());
        Ok(self)
    }

    pub fn verify_track_worktree_owner(mut self, owner: &str) -> Result<Self> {
        let selected = base::utf8_path(&self.selected_path, "selected track worktree")?;
        let target = crate::worktree::track_worktree_target(owner, selected)?;
        if base::is_symlink_leaf(&target.path)?
            || crate::git::git_worktree_registration(&target)?
                != crate::git::GitWorktreeRegistration::Present
        {
            return Err(Error::Internal(
                "track worktree ownership requires recovery".into(),
            ));
        }
        self.owner = Some(owner.into());
        Ok(self)
    }

    /// Check both the retained descriptor and the currently named root. A rename/replacement
    /// is recovery, even when the replacement contains an otherwise valid repository.
    pub fn is_current(&self) -> Result<bool> {
        let opened = self
            .directory
            .metadata()
            .map_err(|error| Error::Internal(format!("stat open workspace: {error}")))?;
        let named = match std::fs::symlink_metadata(&self.root) {
            Ok(named) => named,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(Error::Internal(format!("stat named workspace: {error}"))),
        };
        if !named.is_dir()
            || named.file_type().is_symlink()
            || opened.dev() != named.dev()
            || opened.ino() != named.ino()
            || i64::try_from(opened.dev()).ok() != Some(self.device)
            || i64::try_from(opened.ino()).ok() != Some(self.inode)
        {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.is_current()? {
            return Err(Error::Internal(
                "workspace directory identity changed; recovery required".into(),
            ));
        }
        Ok(())
    }
}

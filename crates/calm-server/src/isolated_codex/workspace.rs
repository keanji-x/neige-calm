//! Fresh kernel-owned workspace, retained after execution. Never source Git files.
use crate::error::{CalmError, Result};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

pub(crate) fn prepare_root(root: &Path) -> Result<()> {
    if !root.is_absolute() {
        return Err(CalmError::BadRequest(
            "isolated workspace root must be absolute".into(),
        ));
    }
    let parent = root
        .parent()
        .ok_or_else(|| CalmError::BadRequest("isolated root parent missing".into()))?;
    open(parent)?;
    match std::fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let directory = open(root)?;
    let metadata = directory.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        return Err(CalmError::Conflict(
            "isolated workspace must be kernel-owned and private".into(),
        ));
    }
    directory.sync_all()?;
    open(parent)?.sync_all()?;
    Ok(())
}
fn open(path: &Path) -> Result<File> {
    use nix::fcntl::{OFlag, OpenHow, ResolveFlag, openat2};
    use std::os::fd::FromRawFd;
    let fd = openat2(
        libc::AT_FDCWD,
        path,
        OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC)
            .resolve(ResolveFlag::RESOLVE_NO_SYMLINKS),
    )
    .map_err(std::io::Error::from)?;
    Ok(unsafe { File::from_raw_fd(fd) })
}
/// Private creation marker stays beside the workspace, outside the Worker mount.
/// Replay verifies ownership and never recreates or empties a used workspace.
pub(crate) fn prepare(root: &Path, op_id: &str) -> Result<std::path::PathBuf> {
    if op_id.is_empty()
        || !op_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(CalmError::Conflict(
            "invalid isolated workspace operation identity".into(),
        ));
    }
    let path = root.join(op_id);
    let marker = root.join(format!("{op_id}.owner"));
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(path.join(".codex"))?;
            open(&path.join(".codex"))?.sync_all()?;
            open(&path)?.sync_all()?;
            let meta = open(&path)?.metadata()?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&marker)?;
            write!(file, "{}:{}", meta.dev(), meta.ino())?;
            file.sync_all()?;
            open(root)?.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = open(&path)?.metadata()?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&marker)?;
            let marker_meta = file.metadata()?;
            if !marker_meta.is_file()
                || marker_meta.uid() != unsafe { libc::geteuid() }
                || marker_meta.mode() & 0o077 != 0
                || marker_meta.len() > 128
            {
                return Err(CalmError::Conflict(
                    "isolated workspace marker is invalid".into(),
                ));
            }
            use std::io::Read;
            let mut value = String::new();
            file.take(129).read_to_string(&mut value)?;
            if value != format!("{}:{}", meta.dev(), meta.ino()) {
                return Err(CalmError::Conflict(
                    "isolated workspace identity changed".into(),
                ));
            }
            open(&path.join(".codex"))?;
            open(root)?.sync_all()?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(path)
}

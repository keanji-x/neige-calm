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

/// Read-only authority for an existing retained workspace. The returned descriptor
/// must reach the final file open; resolving its pathname again loses this proof.
pub(crate) fn open_retained(root: &Path, op_id: &str, workspace: &Path) -> Result<File> {
    use nix::fcntl::{OFlag, OpenHow, ResolveFlag, openat2};
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    let denied = || CalmError::Conflict("Task workspace ownership is unavailable.".into());
    if op_id.is_empty()
        || !op_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || !root.is_absolute()
        || !workspace.components().all(|part| {
            matches!(
                part,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
        || workspace != root.join(op_id)
    {
        return Err(denied());
    }
    let root = open(root).map_err(|_| denied())?;
    let root_meta = root.metadata().map_err(|_| denied())?;
    let uid = unsafe { libc::geteuid() };
    if root_meta.uid() != uid || root_meta.mode() & 0o077 != 0 {
        return Err(denied());
    }
    let resolve = ResolveFlag::RESOLVE_BENEATH
        | ResolveFlag::RESOLVE_NO_SYMLINKS
        | ResolveFlag::RESOLVE_NO_MAGICLINKS;
    let directory = openat2(
        root.as_raw_fd(),
        op_id,
        OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC)
            .resolve(resolve),
    )
    .map_err(|_| denied())?;
    // SAFETY: each syscall returns a fresh descriptor transferred to one File.
    let directory = unsafe { File::from_raw_fd(directory) };
    let meta = directory.metadata().map_err(|_| denied())?;
    if meta.uid() != uid {
        return Err(denied());
    }
    let marker = openat2(
        root.as_raw_fd(),
        root_marker(op_id).as_str(),
        OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW)
            .resolve(resolve),
    )
    .map_err(|_| denied())?;
    let marker = unsafe { File::from_raw_fd(marker) };
    let marker_meta = marker.metadata().map_err(|_| denied())?;
    if !marker_meta.is_file()
        || marker_meta.uid() != uid
        || marker_meta.mode() & 0o077 != 0
        || marker_meta.nlink() != 1
        || marker_meta.len() > 128
    {
        return Err(denied());
    }
    let mut value = String::new();
    marker
        .take(129)
        .read_to_string(&mut value)
        .map_err(|_| denied())?;
    if value != format!("{}:{}", meta.dev(), meta.ino()) {
        return Err(denied());
    }
    Ok(directory)
}

fn root_marker(op_id: &str) -> String {
    format!("{op_id}.owner")
}

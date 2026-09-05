use crate::{Digest, Error, Result};
use nix::fcntl::{OFlag, OpenHow, ResolveFlag, openat2};
use nix::sys::stat::Mode;
use sha2::{Digest as _, Sha256};
use std::{
    ffi::CString,
    fs::{self, DirBuilder, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
};

pub(crate) fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .as_os_str()
            .as_bytes()
            .split(|b| *b == b'/')
            .any(|part| part == b"." || part == b"..")
    {
        return Err(Error::Invalid(
            "expected absolute path without traversal".into(),
        ));
    }
    Ok(())
}

/// Open through the kernel, refusing symlinks in EVERY component. Relative reads
/// are anchored to an already-open directory; mount escapes are also refused.
pub(crate) fn open_beneath(root: &File, relative: &str) -> Result<File> {
    let fd = openat2(
        root.as_raw_fd(),
        relative,
        OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK)
            .mode(Mode::empty())
            .resolve(
                ResolveFlag::RESOLVE_BENEATH
                    | ResolveFlag::RESOLVE_NO_SYMLINKS
                    | ResolveFlag::RESOLVE_NO_XDEV,
            ),
    )
    .map_err(std::io::Error::from)?;
    // SAFETY: openat2 returned a new descriptor; this is its unique owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn open_dir(path: &Path) -> Result<File> {
    absolute(path)?;
    let fd = openat2(
        nix::libc::AT_FDCWD,
        path,
        OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY)
            .mode(Mode::empty())
            .resolve(ResolveFlag::RESOLVE_NO_SYMLINKS),
    )
    .map_err(std::io::Error::from)?;
    // SAFETY: openat2 returned a new descriptor; this is its unique owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn regular(file: &File) -> Result<()> {
    if !file.metadata()?.is_file() {
        return Err(Error::Unsupported("non-regular file".into()));
    }
    Ok(())
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    open_dir(
        path.parent()
            .ok_or_else(|| Error::Invalid("directory parent".into()))?,
    )?;
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {
            sync_dir(
                path.parent()
                    .ok_or_else(|| Error::Invalid("directory has no parent".into()))?,
            )?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    require_private_dir(path)
}

/// Validate existing store metadata without creating a replacement directory.
pub(crate) fn require_private_dir(path: &Path) -> Result<()> {
    let meta = open_dir(path)?.metadata()?;
    // SAFETY: geteuid has no pointer arguments or side effects.
    if meta.uid() != unsafe { nix::libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(Error::Invalid(
            "store directories must be caller-owned and private (0700)".into(),
        ));
    }
    Ok(())
}
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(test)]
    faults::before_sync(path)?;
    open_dir(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
pub(crate) fn read_bounded(file: File, limit: u64) -> Result<Vec<u8>> {
    regular(&file)?;
    if file.metadata()?.len() > limit {
        return Err(Error::Limit("manifest bytes".into()));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Limit("manifest bytes".into()));
    }
    Ok(bytes)
}

/// Streaming verification is shared by capture, snapshot reads and preparation.
/// It never relies on length alone, and output is private until the caller commits.
pub(crate) fn copy_hash(
    mut input: File,
    output: &mut impl Write,
    limit: u64,
) -> Result<(Digest, u64)> {
    regular(&input)?;
    if input.metadata()?.len() > limit {
        return Err(Error::Limit("file bytes".into()));
    }
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| Error::Limit("file bytes".into()))?;
        if total > limit {
            return Err(Error::Limit("file bytes".into()));
        }
        hash.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    Ok((Digest::from_hash(hash), total))
}
pub(crate) fn verify_identity(actual: &(Digest, u64), expected: &Digest, bytes: u64) -> Result<()> {
    if &actual.0 != expected || actual.1 != bytes {
        return Err(Error::Integrity(format!("object {expected}")));
    }
    Ok(())
}

/// Atomic rename with no overwrite, including an existing empty directory.
pub(crate) fn rename_new(source: &Path, destination: &Path) -> Result<()> {
    let from = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| Error::Invalid("NUL path".into()))?;
    let to = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| Error::Invalid("NUL path".into()))?;
    // SAFETY: both C strings live through the syscall and the integer arguments
    // match renameat2(AT_FDCWD, path, AT_FDCWD, path, RENAME_NOREPLACE).
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            nix::libc::AT_FDCWD,
            from.as_ptr(),
            nix::libc::AT_FDCWD,
            to.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(Error::DestinationExists(destination.to_path_buf()));
        }
        return Err(err.into());
    }
    Ok(())
}
pub(crate) fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
pub(crate) fn set_mode(file: &File, mode: u32) -> Result<()> {
    file.set_permissions(Permissions::from_mode(mode))?;
    file.sync_all()?;
    Ok(())
}
pub(crate) fn fd_path(dir: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}

/// Only names issued by tempfile::Builder for this store's unpublished captures
/// are reclaimable. No recursive scan of retained snapshots or arbitrary paths.
pub(crate) fn clean_staging(staging: &Path) -> Result<()> {
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|n| {
            n.strip_prefix("capture-")
                .or_else(|| n.strip_prefix("prepare-"))
        }) else {
            return Err(Error::Integrity("unexpected staging entry".into()));
        };
        if suffix.len() != 12
            || !suffix.bytes().all(|b| b.is_ascii_alphanumeric())
            || !entry.file_type()?.is_dir()
        {
            return Err(Error::Integrity("unexpected staging directory".into()));
        }
        fs::remove_dir_all(entry.path())?;
    }
    sync_dir(staging)
}

#[cfg(test)]
#[path = "filesystem_faults.rs"]
pub(crate) mod faults;

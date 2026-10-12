//! The data-dir lock: at most one process holds SDK auth state or writes the
//! archive (#2575 I10).
//!
//! The lock is an exclusive `flock` on `<data_dir>/archive.lock`, taken
//! without blocking. `std::fs::File::try_lock` is `flock(LOCK_EX | LOCK_NB)`
//! on Unix. A `flock` belongs to the open file description, so a second
//! attempt conflicts whether it comes from another process or from another
//! open of the file in this one, and the kernel drops the lock when the
//! process exits, however it exits. The file is never deleted: deleting a
//! lock file lets two holders lock two different inodes.
//!
//! `capture` takes the lock before anything else; `auth import` and
//! `auth login` take it and exit `LOCKED` while it is held; `capture` exits
//! `BUSY` (0).

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// The lock file's name inside the data dir.
pub const LOCK_FILE: &str = "archive.lock";

/// A held lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct DataDirLock {
    _file: File,
}

/// Why the lock was not taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another holder has it: `capture` exits `BUSY`, an operator command
    /// `LOCKED`.
    #[error("{0} is held by another process")]
    Busy(PathBuf),
    #[error("cannot lock {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

impl DataDirLock {
    /// Take the lock without waiting. Creates `data_dir` (mode 0700) and the
    /// lock file (mode 0600) when they are missing.
    pub fn try_acquire(data_dir: &Path) -> Result<DataDirLock, LockError> {
        let path = data_dir.join(LOCK_FILE);
        let io_err = |source| LockError::Io {
            path: path.clone(),
            source,
        };
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(data_dir)
            .map_err(io_err)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(io_err)?;
        match file.try_lock() {
            Ok(()) => Ok(DataDirLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(LockError::Busy(path)),
            Err(TryLockError::Error(source)) => Err(io_err(source)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_busy_until_the_first_drops() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let first = DataDirLock::try_acquire(&data_dir).unwrap();
        assert!(matches!(
            DataDirLock::try_acquire(&data_dir),
            Err(LockError::Busy(_))
        ));
        drop(first);
        let again = DataDirLock::try_acquire(&data_dir).unwrap();
        drop(again);
        assert!(data_dir.join(LOCK_FILE).is_file());
    }
}

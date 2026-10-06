//! Cooperative metadata lock for kernel Track create/delete only. Forge shell and external
//! Git do not participate. Never unlink this file: all participants must lock the same inode.
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::{CalmError, Result};

const LOCK_NAME: &std::ffi::CStr = c"neige-track-metadata.lock";
const WAIT: Duration = Duration::from_secs(30);

/// Closing the owned descriptor releases flock, including during unwinding. The blocking
/// Git task owns this guard; cancellation of its async join cannot release it early.
pub(super) struct GitMetadataLock {
    _file: File,
}

impl GitMetadataLock {
    /// Call only on a blocking thread, outside database transactions. Root and linked-root
    /// resolve to the same canonical common dir, independently of RouteState or process.
    pub(super) fn acquire(repo_root: &Path) -> Result<Self> {
        let common = super::base::lease_git_common_dir(repo_root)?;
        Self::open_and_lock(&common, || {
            #[cfg(feature = "fixtures")]
            super::metadata_test_pause("track-metadata-contended", repo_root);
            #[cfg(test)]
            tests::observe_contention();
        })
        .map_err(|error| {
            CalmError::Internal(format!(
                "lock Track Git metadata in {}: {error}",
                common.display()
            ))
        })
    }

    fn open_and_lock(common: &Path, on_contended: impl Fn()) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(common)?;
        // Anchor the leaf open to the verified directory descriptor. No truncation or unlink,
        // and no following a repository-supplied symlink (even to a regular single-link file).
        let fd = loop {
            // SAFETY: directory and the static NUL-terminated name live throughout openat.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    LOCK_NAME.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd >= 0 {
                break fd;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        // SAFETY: openat returned a new owned descriptor, transferred exactly once.
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::other(
                "Track Git metadata lock must be a regular single-link file",
            ));
        }
        let deadline = Instant::now() + WAIT;
        loop {
            // SAFETY: file owns a valid fd; flock takes no pointers and retains no Rust data.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self { _file: file });
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                // Observe only an actual failed flock; a pre-syscall attempt proves nothing.
                on_contended();
            }
            if !matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK | libc::EINTR)) {
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Track Git metadata lock busy for 30s",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests;

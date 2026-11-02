//! Child-only descriptor isolation on kernels with and without close_range(CLOEXEC).
//! The fallback enumerates actual open descriptors, including ones above a lowered
//! RLIMIT_NOFILE. After fork there are no sibling threads changing this descriptor table.
use std::{io, mem::offset_of, os::fd::RawFd};

pub(super) fn mark_cloexec() -> io::Result<()> {
    // SAFETY: the caller is the forked child; its descriptor flags are private.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // The arguments are fixed and valid. EINVAL also covers kernels which
        // implement close_range but predate its CLOEXEC flag.
        Some(libc::ENOSYS | libc::EINVAL) => mark_from_proc(),
        _ => Err(error),
    }
}

fn mark_from_proc() -> io::Result<()> {
    // SAFETY: fixed Linux procfs interface; no allocation or directory-stream
    // library locks are used in the post-fork child.
    let directory = unsafe {
        libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if directory < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = mark_directory_entries(directory);
    // SAFETY: directory is owned here. O_CLOEXEC also prevents it surviving exec
    // if close is interrupted.
    unsafe { libc::close(directory) };
    result
}

fn mark_directory_entries(directory: RawFd) -> io::Result<()> {
    let mut buffer = [0u8; 8192];
    loop {
        // SAFETY: the buffer is writable for its stated length and the descriptor
        // refers to the open procfs directory.
        let count = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                directory,
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(());
        }
        let count = usize::try_from(count).map_err(|_| invalid_directory())?;
        if count > buffer.len() {
            return Err(invalid_directory());
        }
        let mut offset = 0;
        while offset < count {
            let entry = &buffer[offset..count];
            let name_offset = offset_of!(libc::dirent64, d_name);
            let length_offset = offset_of!(libc::dirent64, d_reclen);
            if entry.len() <= name_offset || length_offset + 2 > entry.len() {
                return Err(invalid_directory());
            }
            let length =
                u16::from_ne_bytes([entry[length_offset], entry[length_offset + 1]]) as usize;
            if length <= name_offset || length > entry.len() {
                return Err(invalid_directory());
            }
            let name = &entry[name_offset..length];
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(invalid_directory)?;
            let name = &name[..end];
            if name != b"." && name != b".." {
                let mut descriptor: RawFd = 0;
                if name.is_empty() {
                    return Err(invalid_directory());
                }
                for byte in name {
                    if !byte.is_ascii_digit() {
                        return Err(invalid_directory());
                    }
                    descriptor = descriptor
                        .checked_mul(10)
                        .and_then(|value| value.checked_add((byte - b'0') as RawFd))
                        .ok_or_else(invalid_directory)?;
                }
                if descriptor >= 3 {
                    // SAFETY: all entries are this child's open descriptors. We
                    // change flags, not ownership; bootstrap allowlisting happens
                    // only after the complete scan succeeds.
                    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
                    if flags < 0
                        || unsafe {
                            libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC)
                        } < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            offset += length;
        }
    }
}

fn invalid_directory() -> io::Error {
    io::Error::from_raw_os_error(libc::EIO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::process::CommandExt,
        },
        process::Command,
    };

    fn exec_does_not_inherit(mark: fn() -> io::Result<()>, lower_limit: bool) {
        let file = File::open("/dev/null").unwrap();
        // F_DUPFD deliberately creates an inheritable descriptor.
        let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 600) };
        assert!(descriptor >= 600);
        // SAFETY: F_DUPFD returned a new descriptor owned by this test.
        let inherited = unsafe { File::from_raw_fd(descriptor) };
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &format!("test ! -e /proc/self/fd/{descriptor}")]);
        // SAFETY: only rlimit and descriptor syscalls run after fork.
        unsafe {
            command.pre_exec(move || {
                if lower_limit {
                    let mut limit = std::mem::zeroed();
                    if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    limit.rlim_cur = 128;
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                mark()
            });
        }
        assert!(command.status().unwrap().success());
        // Child isolation must not change the parent's descriptor flags.
        assert_eq!(
            unsafe { libc::fcntl(inherited.as_raw_fd(), libc::F_GETFD) },
            0
        );
    }

    #[test]
    fn child_fd_allowlist_marks_inherited_descriptors_before_exec() {
        exec_does_not_inherit(mark_cloexec, false);
    }

    #[test]
    fn child_fd_allowlist_procfs_covers_descriptors_above_soft_limit() {
        exec_does_not_inherit(mark_from_proc, true);
    }
}

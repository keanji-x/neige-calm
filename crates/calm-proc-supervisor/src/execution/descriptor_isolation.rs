//! Linux exec descriptor isolation, including kernels without close_range(CLOEXEC).
use std::io;

pub(super) fn mark() -> io::Result<()> {
    mark_with(|| {
        // SAFETY: child-only operation on its descriptor table; no parent flags change.
        if unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

fn mark_with(close_range: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    match close_range() {
        Ok(()) => Ok(()),
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) => {
            proc_descriptors()
        }
        Err(error) => Err(error),
    }
}

fn malformed() -> io::Error {
    io::Error::from_raw_os_error(libc::EIO)
}

/// After fork: no allocation, locks, stdio, or descriptor ceiling inferred from RLIMIT.
/// Linux getdents64 returns d_reclen at offset 16 and a NUL-terminated d_name at 19.
fn proc_descriptors() -> io::Result<()> {
    // SAFETY: static NUL-terminated path. The Linux execution owner already requires procfs.
    let directory = unsafe {
        libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if directory < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = mark_directory(directory);
    // SAFETY: the enumeration descriptor is owned locally and never published.
    let closed = unsafe { libc::close(directory) };
    result?;
    if closed != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn mark_directory(directory: libc::c_int) -> io::Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs fills the provided structure on success.
    if unsafe { libc::fstatfs(directory, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: initialized by the successful fstatfs above.
    if unsafe { stat.assume_init() }.f_type != libc::PROC_SUPER_MAGIC {
        return Err(malformed());
    }
    let mut buffer = [0u8; 4096];
    loop {
        // SAFETY: kernel writes at most this stack buffer's length.
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
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(());
        }
        let count = usize::try_from(count).map_err(|_| malformed())?;
        if count > buffer.len() {
            return Err(malformed());
        }
        let mut offset = 0;
        while offset < count {
            let entry = &buffer[offset..count];
            if entry.len() < 20 {
                return Err(malformed());
            }
            let length = u16::from_ne_bytes([entry[16], entry[17]]) as usize;
            if length < 20 || length > entry.len() {
                return Err(malformed());
            }
            let name = &entry[19..length];
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(malformed)?;
            let name = &name[..end];
            if name != b"." && name != b".." {
                if name.is_empty() {
                    return Err(malformed());
                }
                let mut fd = 0i32;
                for digit in name {
                    if !digit.is_ascii_digit() {
                        return Err(malformed());
                    }
                    fd = fd
                        .checked_mul(10)
                        .and_then(|fd| fd.checked_add(i32::from(*digit - b'0')))
                        .ok_or_else(malformed)?;
                }
                if fd >= 3 {
                    // Only flags change, preserving membership while getdents advances and
                    // retaining std's exec-error pipe until the exec handshake completes.
                    loop {
                        // SAFETY: this is the fork child's stable, enumerated descriptor.
                        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                        if flags >= 0
                            && unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) }
                                == 0
                        {
                            break;
                        }
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
            }
            offset += length;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn unsupported_close_range_preserves_allowlist_above_soft_limit() {
        for errno in [libc::ENOSYS, libc::EINVAL] {
            let mut command = std::process::Command::new("/bin/sh");
            command.args([
                "-c",
                "test ! -e /proc/self/fd/128 && test -e /proc/self/fd/129",
            ]);
            // SAFETY: only libc and the production no-allocation isolation routine run in child.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(1, 128) < 0 || libc::dup2(1, 129) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
                    if libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let mut limit = limit.assume_init();
                    limit.rlim_cur = 64;
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    mark_with(|| Err(io::Error::from_raw_os_error(errno)))?;
                    // Same explicit reopening that the real helper command performs.
                    if libc::fcntl(129, libc::F_SETFD, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            assert!(command.status().unwrap().success());
        }
    }

    #[test]
    fn denied_close_range_does_not_start_an_unisolated_child() {
        let mut command = std::process::Command::new("/bin/true");
        // SAFETY: the child returns the production rejection through std's exec handshake.
        unsafe {
            command.pre_exec(|| mark_with(|| Err(io::Error::from_raw_os_error(libc::EPERM))));
        }
        assert_eq!(
            command.spawn().unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
    }
}

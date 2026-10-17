use super::{InputObservationError, InputObservationUnsupported};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::{fd::BorrowedFd, unix::ffi::OsStrExt},
};

pub(super) fn validate_component(name: &OsStr) -> Result<(), InputObservationError> {
    if name.is_empty() || name.as_bytes().contains(&b'/') || name.as_bytes().contains(&0) {
        return Err(InputObservationError::InvalidStep(
            "expected one nonempty path component",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn open_entry(parent: BorrowedFd<'_>, name: &OsStr) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = CString::new(name.as_bytes())?;
    // SAFETY: parent is borrowed for this call, name is terminated, and a
    // successful O_PATH descriptor is transferred exactly once to File.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: openat returned this new descriptor and ownership moves once.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn open_entry(_parent: BorrowedFd<'_>, _name: &OsStr) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(target_os = "linux")]
pub(super) fn effective_execute(
    file: BorrowedFd<'_>,
) -> Result<Result<(), i32>, InputObservationError> {
    use std::os::fd::AsRawFd;
    // SAFETY: the borrowed description and static empty path are live. The
    // kernel checks this actual object using effective credentials and ACLs.
    let result = unsafe {
        libc::syscall(
            libc::SYS_faccessat2,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::X_OK,
            libc::AT_EMPTY_PATH | libc::AT_EACCESS,
        )
    };
    if result == 0 {
        return Ok(Ok(()));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS) => Err(InputObservationError::Unsupported(
            InputObservationUnsupported::EffectiveAccessUnavailable,
        )),
        Some(errno) => Ok(Err(errno)),
        None => Err(error.into()),
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn effective_execute(
    _file: BorrowedFd<'_>,
) -> Result<Result<(), i32>, InputObservationError> {
    Err(InputObservationError::Unsupported(
        InputObservationUnsupported::EffectiveAccessUnavailable,
    ))
}

#[cfg(target_os = "linux")]
pub(super) fn read_symbolic_link(file: BorrowedFd<'_>) -> Result<OsString, InputObservationError> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
    // A single bounded read. A filesystem exceeding this observation budget
    // yields outer Unsupported, never an invented candidate-miss errno.
    let mut bytes = vec![0_u8; libc::PATH_MAX as usize + 1];
    // SAFETY: the O_PATH/NOFOLLOW descriptor holds the actual link. Empty-path
    // readlinkat reads that link, not a newly selected pathname; buffer is live.
    let count = unsafe {
        libc::readlinkat(
            file.as_raw_fd(),
            c"".as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if count as usize == bytes.len() {
        return Err(InputObservationError::Unsupported(
            InputObservationUnsupported::SymbolicLinkTargetTooLarge,
        ));
    }
    bytes.truncate(count as usize);
    Ok(OsString::from_vec(bytes))
}

#[cfg(not(target_os = "linux"))]
pub(super) fn read_symbolic_link(_file: BorrowedFd<'_>) -> Result<OsString, InputObservationError> {
    Err(io::ErrorKind::Unsupported.into())
}

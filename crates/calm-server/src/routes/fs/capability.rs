//! One admission for secure workspace file/directory operations, including the UI.
use super::{CalmError, Result};

pub(super) fn unavailable() -> CalmError {
    CalmError::SecureWorkspaceUnavailable(
        "Secure workspace files and image attachments require Linux with openat2 or \
         macOS 26+ (Darwin 25+) with enforced constrained openat. Upgrade the server host \
         to a supported OS and allow these filesystem operations."
            .into(),
    )
}

pub(crate) fn require_secure_workspace() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        super::macos::require_capability()
    }
    #[cfg(target_os = "linux")]
    {
        use nix::fcntl::{OFlag, OpenHow, openat2};
        use std::os::fd::{FromRawFd, OwnedFd};
        // RESOLVE_BENEATH rejects an absolute path before directory permissions matter.
        let probe = openat2(
            nix::libc::AT_FDCWD,
            "/",
            OpenHow::new()
                .flags(OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC)
                .resolve(super::workspace_resolve_flags(
                    super::WorkspaceSymlinks::Refused,
                )),
        )
        // SAFETY: close the fresh descriptor even if the kernel unexpectedly allowed the open.
        .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
        match probe {
            Err(nix::errno::Errno::EXDEV) => Ok(()),
            _ => Err(unavailable()),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(unavailable())
    }
}

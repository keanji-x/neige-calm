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
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let root = std::fs::File::open("/").map_err(|_| unavailable())?;
        let fd = openat2(
            root.as_raw_fd(),
            ".",
            OpenHow::new()
                .flags(OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC)
                .resolve(super::workspace_resolve_flags(
                    super::WorkspaceSymlinks::Refused,
                )),
        )
        .map_err(|_| unavailable())?;
        // SAFETY: openat2 returned a fresh fd with a single owning conversion.
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(unavailable())
    }
}

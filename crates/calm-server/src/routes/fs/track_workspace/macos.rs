//! Darwin's constrained openat contract, deliberately restricted to Track reads.

use super::super::{
    CalmError, OpenWorkspaceFile, Result, malformed_path, map_io_err, workspace_relative_path,
};
use nix::fcntl::{OFlag, openat};
use nix::libc;
use nix::sys::stat::Mode;
use std::ffi::CStr;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;

// Apple public headers, xnu-12377.121.6 bsd/sys/fcntl.h and bsd/sys/errno.h:
// https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/sys/fcntl.h
// nix 0.29 does not expose RESOLVE_BENEATH on Darwin. Keep the ABI values typed,
// pass every bit unchanged, and require runtime enforcement before any read.
const O_RESOLVE_BENEATH: OFlag = OFlag::from_bits_retain(0x0000_1000);
const O_NOFOLLOW_ANY: OFlag = OFlag::from_bits_retain(0x2000_0000);
const ENOTCAPABLE: libc::c_int = 107;

fn unavailable() -> CalmError {
    CalmError::Internal(
        "secure Track workspace reads require macOS 26+ (Darwin 25+) with enforced constrained openat support".into(),
    )
}

fn supported_release(release: &str) -> bool {
    let parts: Vec<_> = release.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
        && parts[0].parse::<u32>().is_ok_and(|major| major >= 25)
}

fn require_supported_kernel() -> Result<()> {
    let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname writes this correctly sized buffer; inspect it only on success.
    if unsafe { libc::uname(name.as_mut_ptr()) } != 0 {
        return Err(unavailable());
    }
    // SAFETY: successful uname initialized the struct and NUL-terminated release.
    let name = unsafe { name.assume_init() };
    let release = unsafe { CStr::from_ptr(name.release.as_ptr()) };
    if !release.to_str().is_ok_and(supported_release) {
        return Err(unavailable());
    }
    Ok(())
}

fn constrained_open(root: &File, relative: &Path) -> std::io::Result<File> {
    let fd = openat(
        Some(root.as_raw_fd()),
        relative,
        OFlag::O_RDONLY | O_NOFOLLOW_ANY | O_RESOLVE_BENEATH | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        // nix 0.29's Darwin errno enum predates ENOTCAPABLE. Preserve the raw
        // kernel error immediately, before any call that could overwrite errno.
        if error == nix::errno::Errno::UnknownErrno {
            std::io::Error::last_os_error()
        } else {
            error.into()
        }
    })?;
    // SAFETY: openat returned a fresh fd; transfer it to exactly one owning File.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn require_enforced_beneath(root: &File) -> Result<()> {
    // An absolute path must be rejected, even if it names the root directory.
    // Probe no contents and close any unexpected successful fd. This detects
    // ignored flags; it is not a proof of concurrent pathname resolution.
    match constrained_open(root, Path::new("/")) {
        Err(error) if error.raw_os_error() == Some(ENOTCAPABLE) => Ok(()),
        _ => Err(unavailable()),
    }
}

pub(super) async fn open(workspace_root: &Path, relative_path: &str) -> Result<OpenWorkspaceFile> {
    let relative = workspace_relative_path(relative_path)?;
    let workspace_root = workspace_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        require_supported_kernel()?;
        // The persisted root is trusted configuration and may itself use aliases.
        // O_DIRECTORY also rejects a replaced root FIFO without blocking on it.
        let fd = nix::fcntl::open(
            &workspace_root,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| {
            if error == nix::errno::Errno::ENOTDIR {
                CalmError::BadRequest(format!(
                    "track workspace {} is not a directory",
                    workspace_root.display()
                ))
            } else {
                map_io_err(&workspace_root, error.into())
            }
        })?;
        // SAFETY: open returned a fresh fd with a single owning conversion.
        let root = unsafe { File::from_raw_fd(fd) };
        require_enforced_beneath(&root)?;
        let requested = workspace_root.join(&relative);
        let file = constrained_open(&root, &relative)
            .map_err(|error| map_open_error(&requested, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| map_io_err(&requested, error))?;
        if !metadata.is_file() {
            return Err(CalmError::BadRequest(format!(
                "path {} is not a regular file",
                requested.display()
            )));
        }
        Ok(OpenWorkspaceFile {
            file: tokio::fs::File::from_std(file),
            display_path: requested,
            size: metadata.len(),
        })
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace open task failed: {error}")))?
}

fn map_open_error(requested: &Path, error: std::io::Error) -> CalmError {
    match error.raw_os_error() {
        Some(libc::ELOOP) => CalmError::BadRequest(format!(
            "macOS Track workspace reads do not allow symlinks in the file path: {}",
            requested.display()
        )),
        Some(ENOTCAPABLE) => CalmError::BadRequest(format!(
            "path {} resolves outside track workspace",
            requested.display()
        )),
        Some(libc::EINVAL | libc::ENAMETOOLONG) => malformed_path(requested),
        // Darwin returns EOPNOTSUPP (102) when opening a Unix socket as a file.
        Some(libc::ENXIO | libc::ENODEV | libc::EOPNOTSUPP) => CalmError::BadRequest(format!(
            "path {} is not a regular file",
            requested.display()
        )),
        Some(libc::EACCES | libc::EPERM) => {
            CalmError::Forbidden(format!("permission denied reading {}", requested.display()))
        }
        _ => map_io_err(requested, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_supported_darwin_releases_enable_reads() {
        for release in [
            "24.9.0",
            "",
            "unknown",
            "25",
            "25.1",
            "25.1.0.1",
            "25.x.0",
            "999999999999.0.0",
        ] {
            assert!(!supported_release(release), "{release}");
        }
        for release in ["25.0.0", "25.5.0", "26.0.0"] {
            assert!(supported_release(release), "{release}");
        }
    }

    #[test]
    fn kernel_enforces_beneath_even_for_absolute_root() {
        require_supported_kernel().unwrap();
        let root = File::open("/").unwrap();
        require_enforced_beneath(&root).unwrap();
        assert_eq!(
            constrained_open(&root, Path::new("/"))
                .unwrap_err()
                .raw_os_error(),
            Some(ENOTCAPABLE)
        );
    }
}

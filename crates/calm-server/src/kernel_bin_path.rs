//! The PATH of every Planner and Worker execution environment leads with the running kernel's
//! own bin dir, so `neige` there is the CLI installed beside this calm-server (#1784).

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// The running kernel's bin dir and the PATH built from it for one spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelLedPath {
    /// Parent directory of `current_exe()`; releases install `neige` there.
    pub bin_dir: PathBuf,
    /// `bin_dir`, then the kernel's own inherited PATH unchanged.
    pub path: OsString,
}

/// Build the Planner/Worker PATH from this process's executable and PATH.
pub fn kernel_led_path() -> io::Result<KernelLedPath> {
    let exe = std::env::current_exe()?;
    let bin_dir = exe
        .parent()
        .ok_or_else(|| {
            io::Error::other(format!(
                "kernel executable {} has no parent directory",
                exe.display()
            ))
        })?
        .to_path_buf();
    let path = path_leading_with(&bin_dir, std::env::var_os("PATH").as_deref())?;
    Ok(KernelLedPath { bin_dir, path })
}

/// An absent or empty `inherited` PATH adds no empty (current-directory) entry.
fn path_leading_with(bin_dir: &Path, inherited: Option<&OsStr>) -> io::Result<OsString> {
    let rest = inherited
        .filter(|path| !path.is_empty())
        .map(|path| std::env::split_paths(path).collect::<Vec<_>>())
        .unwrap_or_default();
    std::env::join_paths(std::iter::once(bin_dir.to_path_buf()).chain(rest))
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_dir_leads_and_the_inherited_path_follows_unchanged() {
        let path = path_leading_with(Path::new("/k/bin"), Some(OsStr::new("/usr/bin:/bin")));
        assert_eq!(path.unwrap(), OsString::from("/k/bin:/usr/bin:/bin"));
    }

    #[test]
    fn absent_or_empty_inherited_path_adds_no_current_directory_entry() {
        for inherited in [None, Some(OsStr::new(""))] {
            let path = path_leading_with(Path::new("/k/bin"), inherited).unwrap();
            assert_eq!(path, OsString::from("/k/bin"));
        }
    }

    #[test]
    fn a_bin_dir_that_cannot_join_a_path_is_refused() {
        assert!(path_leading_with(Path::new("/k:bin"), Some(OsStr::new("/bin"))).is_err());
    }

    #[test]
    fn kernel_bin_dir_is_the_parent_of_the_running_executable() {
        let led = kernel_led_path().unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(led.bin_dir, exe.parent().unwrap());
        assert_eq!(
            std::env::split_paths(&led.path).next().as_deref(),
            Some(led.bin_dir.as_path())
        );
    }
}

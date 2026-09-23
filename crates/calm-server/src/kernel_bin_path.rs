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

impl KernelLedPath {
    /// codex's exec environment and thread config are String-valued, so the PATH carried in
    /// `shell_environment_policy.set` must be UTF-8.
    pub fn path_utf8(&self) -> io::Result<&str> {
        self.path.to_str().ok_or_else(|| {
            io::Error::other(format!(
                "kernel-led PATH is not UTF-8 and cannot enter codex thread config: {:?}",
                self.path
            ))
        })
    }
}

/// Parent directory of the running calm-server executable.
fn kernel_bin_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| {
        io::Error::other(format!(
            "kernel executable {} has no parent directory",
            exe.display()
        ))
    })
}

/// Build the Planner/Worker PATH from this process's executable and PATH.
pub fn kernel_led_path() -> io::Result<KernelLedPath> {
    let bin_dir = kernel_bin_dir()?;
    let path = path_leading_with(&bin_dir, std::env::var_os("PATH").as_deref())?;
    Ok(KernelLedPath { bin_dir, path })
}

/// A POSIX shell statement putting the kernel bin dir in front of the PATH the shell itself
/// inherited, so a PTY child keeps the proc supervisor's PATH byte-for-byte (UTF-8 or not)
/// behind it. The supervisor's wire is String-typed, so only the bin dir must be UTF-8.
pub fn shell_path_prepend() -> io::Result<String> {
    let bin_dir = kernel_bin_dir()?;
    let dir = bin_dir
        .to_str()
        .ok_or_else(|| io::Error::other(format!("kernel bin dir is not UTF-8: {bin_dir:?}")))?;
    Ok(format!(
        "PATH='{}'\"${{PATH:+:$PATH}}\"; export PATH",
        dir.replace('\'', "'\\''")
    ))
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
    fn shell_prepend_leads_with_the_bin_dir_and_keeps_a_non_utf8_inherited_path() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap();
        let inherited = OsString::from_vec(b"/opt/\xffweird:/usr/bin".to_vec());
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "{}; printf %s \"$PATH\"",
                shell_path_prepend().unwrap()
            ))
            .env("PATH", &inherited)
            .output()
            .unwrap();
        let mut expected = dir.as_os_str().as_bytes().to_vec();
        expected.push(b':');
        expected.extend_from_slice(inherited.as_bytes());
        assert_eq!(out.stdout, expected);
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "{}; printf %s \"$PATH\"",
                shell_path_prepend().unwrap()
            ))
            .env("PATH", "")
            .output()
            .unwrap();
        assert_eq!(out.stdout, dir.as_os_str().as_bytes(), "no empty entry");
    }

    #[test]
    fn a_non_utf8_path_is_refused_for_codex_config() {
        use std::os::unix::ffi::OsStringExt;
        let led = KernelLedPath {
            bin_dir: PathBuf::from("/k/bin"),
            path: OsString::from_vec(b"/k/bin:/\xff".to_vec()),
        };
        assert!(led.path_utf8().is_err());
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

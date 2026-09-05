use crate::{Error, NetworkPolicy, ProcessIdentity, Result};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) struct PidFd(OwnedFd);
impl PidFd {
    pub fn open(pid: i32) -> Result<Self> {
        if pid <= 1 {
            return Err(Error::Evidence("refusing host PID <= 1".into()));
        }
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd as i32) }))
    }
    pub fn exited(&self) -> Result<bool> {
        let mut pfd = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut pfd, 1, 0) };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(pfd.revents & (libc::POLLIN | libc::POLLHUP) != 0)
    }
    pub fn kill(&self) -> Result<()> {
        self.signal(libc::SIGKILL)
    }
    pub fn signal(&self, signal: i32) -> Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }
}

pub(crate) fn boot_id() -> Result<String> {
    let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    if value.trim().is_empty() {
        return Err(Error::Evidence("missing boot identity".into()));
    }
    Ok(value.trim().to_owned())
}

pub(crate) fn identity(pid: i32, require_init: bool) -> Result<ProcessIdentity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(')')
        .ok_or_else(|| Error::Evidence("malformed process stat".into()))?
        .1;
    let start_time = fields
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::Evidence("missing process start time".into()))?;
    if require_init {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
        let inner_pid = status
            .lines()
            .find(|line| line.starts_with("NSpid:"))
            .and_then(|line| line.split_whitespace().last());
        if inner_pid != Some("1") {
            return Err(Error::Evidence("process is not namespace init".into()));
        }
    }
    let namespace_inode = std::fs::metadata(format!("/proc/{pid}/ns/pid"))?.ino();
    if require_init && namespace_inode == std::fs::metadata("/proc/self/ns/pid")?.ino() {
        return Err(Error::Evidence("process is in caller namespace".into()));
    }
    Ok(ProcessIdentity {
        pid,
        start_time,
        boot_id: boot_id()?,
        namespace_inode,
    })
}

pub(crate) enum Observation {
    Live(PidFd),
    Gone(&'static str),
    Unknown(String),
}

/// Pin before checking identity. Never signal a numeric PID after a separate check.
pub(crate) fn observe(expected: &ProcessIdentity) -> Observation {
    let check = || -> Result<Observation> {
        if boot_id()? != expected.boot_id {
            return Ok(Observation::Gone("prior_boot"));
        }
        let pidfd = match PidFd::open(expected.pid) {
            Ok(fd) => fd,
            Err(Error::Io(error)) if error.raw_os_error() == Some(libc::ESRCH) => {
                return Ok(Observation::Gone("init_absent"));
            }
            Err(error) => return Err(error),
        };
        let actual = match identity(expected.pid, false) {
            Ok(actual) => actual,
            Err(Error::Io(error))
                if error.kind() == io::ErrorKind::NotFound && pidfd.exited()? =>
            {
                // A zombie has no ns symlink; absence of stat means actual reaping.
                if !Path::new(&format!("/proc/{}/stat", expected.pid)).try_exists()? {
                    return Ok(Observation::Gone("init_reaped"));
                }
                return Ok(Observation::Live(pidfd));
            }
            Err(error) => return Err(error),
        };
        if actual.start_time != expected.start_time {
            return Ok(Observation::Gone("init_pid_reused"));
        }
        if actual.namespace_inode != expected.namespace_inode {
            return Err(Error::Evidence("PID namespace identity mismatch".into()));
        }
        Ok(Observation::Live(pidfd))
    };
    check().unwrap_or_else(|error| Observation::Unknown(error.to_string()))
}

pub(crate) fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

pub(crate) fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

pub(crate) fn socket_path(directory: &Path) -> Result<(File, PathBuf)> {
    let fd = File::open(directory)?;
    let path = PathBuf::from(format!("/proc/self/fd/{}/stdio.sock", fd.as_raw_fd()));
    Ok((fd, path))
}

pub(crate) fn base_command(bwrap: &Path, helper: &Path, network: NetworkPolicy) -> Command {
    let mut command = Command::new(bwrap);
    command
        .env_clear()
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--as-pid-1",
            "--die-with-parent",
            "--unshare-ipc",
            "--unshare-uts",
            "--new-session",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/bin",
            "/bin",
            "--symlink",
            "usr/sbin",
            "/sbin",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--clearenv",
            "--ro-bind",
        ])
        .arg(helper)
        .arg("/boundary-helper");
    if network == NetworkPolicy::Isolated {
        command.arg("--unshare-net");
    }
    command
}

/// Initial capture must prove this is the still-owned wrapper's direct child.
/// An old --info-fd numeric PID alone could now name an unrelated namespace init.
pub(crate) fn verify_capture_parent(pid: i32, wrapper_pid: u32) -> Result<()> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let parent = stat
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u32>().ok());
    if parent != Some(wrapper_pid) {
        return Err(Error::Evidence(
            "captured init is not this wrapper's child".into(),
        ));
    }
    Ok(())
}

pub(crate) fn verify_network(pid: i32, policy: NetworkPolicy) -> Result<()> {
    let parent = std::fs::metadata("/proc/self/ns/net")?.ino();
    let child = std::fs::metadata(format!("/proc/{pid}/ns/net"))?.ino();
    if (parent == child) != (policy == NetworkPolicy::Provider) {
        return Err(Error::Unsupported(
            "captured network namespace violates policy".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_identity_wrong_namespace_is_unknown() {
        let mut own = identity(std::process::id() as i32, false).unwrap();
        own.namespace_inode ^= 1;
        assert!(matches!(observe(&own), Observation::Unknown(_)));
    }

    #[test]
    fn boundary_identity_distinguishes_prior_boot_and_pid_reuse() {
        let own = identity(std::process::id() as i32, false).unwrap();
        let mut prior = own.clone();
        prior.boot_id = "earlier-boot".into();
        assert!(matches!(observe(&prior), Observation::Gone("prior_boot")));
        let mut earlier = own.clone();
        earlier.start_time = own.start_time.saturating_add(1);
        assert!(matches!(
            observe(&earlier),
            Observation::Gone("init_pid_reused")
        ));
        assert!(matches!(observe(&own), Observation::Live(_)));
    }
}

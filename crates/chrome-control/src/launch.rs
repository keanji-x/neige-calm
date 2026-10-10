//! The browser's command line, environment and CDP pipes.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tokio::net::unix::pipe;

use crate::process::ChildProcess;
use crate::{Error, Result};

/// The Wayland display the browser connects to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaylandEnv {
    /// `WAYLAND_DISPLAY`: a socket name inside `runtime_dir`, or an absolute socket path.
    pub display: PathBuf,
    /// `XDG_RUNTIME_DIR`.
    pub runtime_dir: PathBuf,
}

/// How to launch one browser. All fields are required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfig {
    /// The Chrome executable.
    pub binary: PathBuf,
    /// `--user-data-dir`: the persistent profile. Created if missing and set
    /// to 0700 on every launch.
    pub profile_dir: PathBuf,
    /// `HOME` of the browser, so its crash database and NSS state stay out of
    /// the caller's home. Created if missing and set to 0700 on every launch.
    pub home_dir: PathBuf,
    pub wayland: WaylandEnv,
    /// `--window-size` in pixels.
    pub size: (u32, u32),
}

/// Variables copied from the caller's environment when present. Everything
/// else is dropped, apart from the `LC_*` family.
const INHERITED: [&str; 4] = ["PATH", "LANG", "FONTCONFIG_FILE", "FONTCONFIG_PATH"];
const INHERITED_PREFIX: &str = "LC_";

/// Chrome reads CDP commands from fd 3 and writes responses to fd 4.
const CDP_COMMANDS_FD: RawFd = 3;
const CDP_RESPONSES_FD: RawFd = 4;
/// Child-side pipe ends are parked at or above this number until `pre_exec`
/// moves them onto 3 and 4, so the first `dup2` can never clobber the source
/// of the second.
const PARKED_FD_MIN: RawFd = 10;
/// Every descriptor from here up is close-on-exec in the browser.
const FIRST_UNSHARED_FD: libc::c_uint = 5;

fn arguments(config: &LaunchConfig) -> Vec<OsString> {
    let mut profile = OsString::from("--user-data-dir=");
    profile.push(&config.profile_dir);
    vec![
        "--ozone-platform=wayland".into(),
        profile,
        "--remote-debugging-pipe".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // `--remote-debugging-pipe` alone sets `navigator.webdriver`; this keeps
        // the owner's own logins from being flagged as automation.
        "--disable-blink-features=AutomationControlled".into(),
        format!("--window-size={},{}", config.size.0, config.size.1).into(),
    ]
}

/// The browser command without its fd set-up: binary, arguments, the
/// allowlisted environment, stdio and a process group of its own.
fn command(config: &LaunchConfig) -> Command {
    let mut command = Command::new(&config.binary);
    command
        .args(arguments(config))
        .env_clear()
        .envs(environment(std::env::vars_os(), config))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0);
    command
}

/// The complete child environment: the allowlisted part of `parent` plus the
/// display and `HOME` from `config`.
pub(crate) fn environment(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    config: &LaunchConfig,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = parent
        .into_iter()
        .filter(|(key, _)| inherited(key))
        .collect();
    env.push((
        "WAYLAND_DISPLAY".into(),
        config.wayland.display.clone().into(),
    ));
    env.push((
        "XDG_RUNTIME_DIR".into(),
        config.wayland.runtime_dir.clone().into(),
    ));
    env.push(("HOME".into(), config.home_dir.clone().into()));
    env
}

fn inherited(key: &OsStr) -> bool {
    key.to_str()
        .is_some_and(|key| INHERITED.contains(&key) || key.starts_with(INHERITED_PREFIX))
}

/// A started browser and the parent ends of its CDP pipes.
pub(crate) struct Spawned {
    pub(crate) process: ChildProcess,
    pub(crate) commands: pipe::Sender,
    pub(crate) responses: pipe::Receiver,
}

/// Starts the browser in its own process group with `PR_SET_PDEATHSIG(SIGKILL)`.
pub(crate) async fn spawn(config: &LaunchConfig) -> Result<Spawned> {
    let spawn_error = |source: io::Error| Error::Spawn {
        binary: config.binary.clone(),
        source,
    };
    // Private even when the directory already existed with a wider mode. A
    // directory we do not own cannot be chmodded, so launch refuses it.
    for dir in [&config.profile_dir, &config.home_dir] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .and_then(|()| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)))
            .map_err(spawn_error)?;
    }
    let (child_commands, commands) = cdp_pipe().map_err(spawn_error)?;
    let (responses, child_responses) = cdp_pipe().map_err(spawn_error)?;
    let child_commands = park(child_commands).map_err(spawn_error)?;
    let child_responses = park(child_responses).map_err(spawn_error)?;
    let commands = pipe::Sender::from_owned_fd(commands).map_err(spawn_error)?;
    let responses = pipe::Receiver::from_owned_fd(responses).map_err(spawn_error)?;

    let mut command = command(config);
    let parent = std::process::id() as libc::pid_t;
    let (commands_fd, responses_fd) = (child_commands.as_raw_fd(), child_responses.as_raw_fd());
    // SAFETY: the closure runs between fork and exec and only makes
    // async-signal-safe system calls; it allocates nothing.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The owner died between fork and prctl: the signal would never come.
            if libc::getppid() != parent {
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            // dup2 replaces whatever sits on 3 and 4. std's exec-error pipe lands
            // there only when the caller left those numbers free; an exec
            // failure then shows as an early exit instead of a spawn error.
            if libc::dup2(commands_fd, CDP_COMMANDS_FD) == -1
                || libc::dup2(responses_fd, CDP_RESPONSES_FD) == -1
            {
                return Err(io::Error::last_os_error());
            }
            close_above_on_exec(FIRST_UNSHARED_FD);
            Ok(())
        });
    }
    let process = ChildProcess::spawn(command).await.map_err(spawn_error)?;
    // Close our copies of the child's ends, so the child's exit is our EOF.
    drop((child_commands, child_responses));
    Ok(Spawned {
        process,
        commands,
        responses,
    })
}

/// Marks every descriptor from `first` up close-on-exec, so nothing the
/// caller leaked without that flag reaches the browser. Runs between fork and
/// exec: system calls only.
fn close_above_on_exec(first: libc::c_uint) {
    // SAFETY: close_range only changes descriptor flags.
    let done = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            first,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } == 0;
    if done {
        return;
    }
    // Kernels before 5.11 (ENOSYS, or EINVAL for the flag): one fcntl per number.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit fills the struct; fcntl on a closed number fails harmlessly.
    unsafe {
        let last = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            limit.rlim_cur.min(1 << 20) as libc::c_int
        } else {
            1 << 16
        };
        for fd in first as libc::c_int..last {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
}

/// A close-on-exec pipe as (read end, write end).
fn cdp_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: pipe2 writes two fds into the array on success.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both fds are freshly created and owned by nobody else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Moves a close-on-exec fd to a number at or above [`PARKED_FD_MIN`].
fn park(fd: OwnedFd) -> io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC returns a new fd that we then own.
    let parked = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, PARKED_FD_MIN) };
    if parked == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `parked` is a fresh fd owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(parked) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LaunchConfig {
        LaunchConfig {
            binary: "/opt/chrome/chrome".into(),
            profile_dir: "/data/profile".into(),
            home_dir: "/data/home".into(),
            wayland: WaylandEnv {
                display: "wayland-7".into(),
                runtime_dir: "/run/desktop".into(),
            },
            size: (1280, 800),
        }
    }

    #[test]
    fn the_launcher_argv_is_pinned_and_carries_no_automation_or_headless_flag() {
        let command = command(&config());
        assert_eq!(command.get_program(), "/opt/chrome/chrome");
        let args: Vec<&str> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "--ozone-platform=wayland",
                "--user-data-dir=/data/profile",
                "--remote-debugging-pipe",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-blink-features=AutomationControlled",
                "--window-size=1280,800",
            ]
        );
    }
}

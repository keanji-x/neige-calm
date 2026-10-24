//! Synchronous Git capture for blocking workspace provisioning. Unlike `output`,
//! the deadline covers descendants holding pipes open after Git exits.
use std::{
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::process::{CommandExt, ExitStatusExt},
    },
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

const CAP: usize = 1024 * 1024;
pub(crate) const GIT_BOUND: Duration = Duration::from_secs(30);

struct Group(Child);
impl Drop for Group {
    fn drop(&mut self) {
        #[cfg(all(test, target_os = "linux"))]
        tests::observe_before_kill(self.0.id());
        // This owner never reaps during capture: the live/unreaped leader pins the
        // numeric PGID until this final signal, including on pipe errors and deadlines.
        // Same ownership contract as shared_codex_appserver's owned-child termination.
        // SAFETY: process_group(0) creates a dedicated group with pgid == child pid.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}
// std::process::Child::try_wait consumes the zombie. Observe the owned child
// without reaping instead; only Group::drop may wait, after the final group signal.
fn observe_exit(child: &Child) -> io::Result<Option<ExitStatus>> {
    loop {
        // SAFETY: initialized output storage for our specific owned child. WNOWAIT
        // retains its identity even when the exit is observed repeatedly.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // SAFETY: waitid initialized the SIGCHLD union; pid zero means no exit yet.
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        let raw = match info.si_code {
            libc::CLD_EXITED => status << 8,
            libc::CLD_KILLED => status,
            libc::CLD_DUMPED => status | 0x80,
            _ => return Err(io::Error::other("unexpected Git waitid exit event")),
        };
        return Ok(Some(ExitStatus::from_raw(raw)));
    }
}
fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: this is an owned child pipe; fcntl retains no pointers.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<bool> {
    let mut buf = [0; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                if bytes.len() + n > CAP {
                    return Err(io::Error::other("Git output exceeds 1 MiB"));
                }
                bytes.extend_from_slice(&buf[..n]);
                // Return to the deadline check even for a continuously writing child.
                return Ok(false);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}
pub(crate) fn output(command: &mut Command, deadline: Instant) -> io::Result<Output> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Git deadline expired",
        ));
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = Group(command.spawn()?);
    let stdout = child.0.stdout.take().expect("stdout piped");
    let stderr = child.0.stderr.take().expect("stderr piped");
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    capture(child, stdout, stderr, deadline)
}

fn capture(
    child: Group,
    mut stdout: impl Read,
    mut stderr: impl Read,
    deadline: Instant,
) -> io::Result<Output> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut status = None;
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Git deadline expired",
            ));
        }
        let out_done = drain(&mut stdout, &mut out)?;
        let err_done = drain(&mut stderr, &mut err)?;
        if status.is_none() {
            status = observe_exit(&child.0)?;
        }
        if out_done
            && err_done
            && let Some(status) = status
        {
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(crate) trait BoundedOutput {
    fn bounded_output(&mut self, deadline: Instant) -> io::Result<Output>;
}
impl BoundedOutput for Command {
    fn bounded_output(&mut self, deadline: Instant) -> io::Result<Output> {
        output(self, deadline)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static PINNED_BEFORE_KILL: Cell<Option<bool>> = const { Cell::new(None) };
    }

    // Observe the real syscall boundary without consuming the zombie or panicking in Drop.
    pub(super) fn observe_before_kill(pid: u32) {
        PINNED_BEFORE_KILL.set(Some(exited_unreaped(pid)));
    }

    fn exited_unreaped(pid: u32) -> bool {
        // SAFETY: initialized output storage, and this thread owns the specific child.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        rc == 0 && unsafe { info.si_pid() } == pid as i32
    }

    fn run_case(kind: &str) {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant");
        let script = match kind {
            "success" => "sleep 30 >&- 2>&- & echo $! > \"$1\"; exit 7",
            "cap" => "yes x & echo $! > \"$1\"; exit 7",
            _ => "sleep 30 & echo $! > \"$1\"; exit 7",
        };
        let mut child = Group(
            Command::new("sh")
                .args(["-c", script, "fixture"])
                .arg(&pid_file)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        let startup = Instant::now() + Duration::from_secs(5);
        while !exited_unreaped(pid) {
            assert!(Instant::now() < startup, "fixture leader did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        let descendant: u32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let stdout = child.0.stdout.take().unwrap();
        let stderr = child.0.stderr.take().unwrap();
        nonblocking(&stdout).unwrap();
        nonblocking(&stderr).unwrap();
        PINNED_BEFORE_KILL.set(None);
        let deadline = Instant::now()
            + if kind == "timeout" {
                Duration::from_millis(150)
            } else {
                Duration::from_secs(5)
            };
        let result = if kind == "read_error" {
            struct Broken(bool);
            impl Read for Broken {
                fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                    if std::mem::replace(&mut self.0, false) {
                        Err(io::ErrorKind::WouldBlock.into())
                    } else {
                        Err(io::Error::other("injected pipe read failure"))
                    }
                }
            }
            capture(child, Broken(true), stderr, deadline)
        } else {
            capture(child, stdout, stderr, deadline)
        };
        match kind {
            "success" => assert_eq!(result.unwrap().status.code(), Some(7)),
            "timeout" => assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut),
            "cap" => assert_eq!(result.unwrap_err().to_string(), "Git output exceeds 1 MiB"),
            "read_error" => assert_eq!(
                result.unwrap_err().to_string(),
                "injected pipe read failure"
            ),
            _ => unreachable!(),
        }
        let cleanup = Instant::now() + Duration::from_secs(5);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{descendant}/stat"));
            if stat.is_err() || stat.unwrap().split_once(") ").unwrap().1.starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < cleanup,
                "descendant survived group termination"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !exited_unreaped(pid),
            "leader must be reaped after termination"
        );
        assert_eq!(
            PINNED_BEFORE_KILL.get(),
            Some(true),
            "leader must remain unreaped until the final group signal"
        );
    }

    #[test]
    fn bounded_git_success_pins_leader_until_group_kill() {
        run_case("success");
        let result = output(
            Command::new("sh").args(["-c", "kill -TERM $$"]),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(result.status.signal(), Some(libc::SIGTERM));
    }
    #[test]
    fn bounded_git_timeout_pins_leader_and_kills_pipe_holder() {
        run_case("timeout");
    }
    #[test]
    fn bounded_git_cap_pins_leader_and_kills_pipe_holder() {
        run_case("cap");
    }
    #[test]
    fn bounded_git_read_error_pins_leader_and_kills_pipe_holder() {
        run_case("read_error");
    }
}

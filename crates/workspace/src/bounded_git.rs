//! Synchronous Git capture for blocking workspace provisioning. Unlike `output`,
//! the deadline covers descendants holding pipes open after Git exits.
use std::{
    io::{self, Read},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const CAP: usize = 1024 * 1024;
pub(crate) const GIT_BOUND: Duration = Duration::from_secs(30);

struct Group(Child);
impl Drop for Group {
    fn drop(&mut self) {
        // SAFETY: process_group(0) creates a dedicated group with pgid == child pid.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
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
    let mut stdout = child.0.stdout.take().expect("stdout piped");
    let mut stderr = child.0.stderr.take().expect("stderr piped");
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
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
            status = child.0.try_wait()?;
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

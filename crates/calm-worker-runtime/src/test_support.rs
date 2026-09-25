//! Process fixtures for tests of `/proc` readers, here and in dependent crates (`test-support`).
use std::fs::File;
use std::os::fd::AsRawFd as _;
use std::process::{Child, Command, Stdio};

/// A spawned child that is SIGKILLed and reaped on drop, so a test that panics between spawn and
/// its own cleanup never orphans the process.
pub struct ChildGuard(Child);

impl ChildGuard {
    /// Spawn `command` with null stdio.
    pub fn spawn(command: &mut Command) -> Self {
        Self(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn test child"),
        )
    }

    /// A `sleep 300` that outlives any test unless killed.
    pub fn sleep() -> Self {
        Self::spawn(Command::new("sleep").arg("300"))
    }

    pub fn pid(&self) -> i32 {
        i32::try_from(self.0.id()).expect("pid fits i32")
    }

    /// SIGKILL and reap now.
    pub fn kill_and_reap(&mut self) {
        self.0.kill().expect("kill test child");
        self.0.wait().expect("reap test child");
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The `/proc/<pid>` directory of a child that is already reaped, held open. Any file read through
/// [`ReapedProcDir::path`] fails with `ESRCH`: the state a `/proc` scan meets when the process is
/// reaped between listing it and reading it, reproduced without timing.
pub struct ReapedProcDir {
    pub pid: i32,
    held: File,
}

impl ReapedProcDir {
    pub fn new() -> Self {
        let mut child = ChildGuard::sleep();
        let pid = child.pid();
        let held = File::open(format!("/proc/{pid}")).expect("hold /proc/<pid>");
        child.kill_and_reap();
        Self::from_held(pid, held)
    }

    /// Wrap a `/proc/<pid>` directory the caller opened before reaping `pid` itself.
    pub fn from_held(pid: i32, held: File) -> Self {
        Self { pid, held }
    }

    /// A path that resolves, through `/proc/self/fd`, to the held directory; symlink a fake proc
    /// root's entry (or one of its files, with a `/<name>` suffix) to it.
    pub fn path(&self) -> String {
        format!("/proc/self/fd/{}", self.held.as_raw_fd())
    }
}

impl Default for ReapedProcDir {
    fn default() -> Self {
        Self::new()
    }
}

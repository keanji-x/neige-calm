//! Child-process primitives shared by the connector runtimes: a capture bounded BEFORE it buffers, and a process-group kill that reaches descendants (`Child::kill` / `kill_on_drop` reach the direct child only).

use tokio::io::{AsyncRead, AsyncReadExt as _};

mod bounded;
mod env;
mod timed;
pub use bounded::{BoundedRunError, run_bounded};
pub use env::inherited_env;
pub use timed::{ChildFinishError, finish_within};

/// Keep the returned master alive for a child that needs terminal input but no rendered output.
/// Call `set_process_group_leader` first: the input becomes its controlling terminal after
/// setsid. If the owner crashes, closing the last master hangs up the foreground child group.
/// Spawning, process groups and normal cleanup remain owned by `GroupChild`.
#[cfg(unix)]
pub fn terminal_input(
    command: &mut tokio::process::Command,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use nix::fcntl::OFlag;
    #[cfg(test)]
    use std::os::fd::AsRawFd;
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    // Both descriptors are close-on-exec at creation. openpty + a later fcntl would expose
    // another thread's fork to the unprotected pair before the flags could be set.
    let master = nix::pty::posix_openpt(OFlag::O_RDWR | OFlag::O_NOCTTY | OFlag::O_CLOEXEC)
        .map_err(std::io::Error::from)?;
    nix::pty::grantpt(&master).map_err(std::io::Error::from)?;
    nix::pty::unlockpt(&master).map_err(std::io::Error::from)?;
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(terminal_slave_name(&master)?)?;
    // SAFETY: into_raw_fd transfers this one owning descriptor; no borrowed alias is wrapped.
    let master = unsafe { OwnedFd::from_raw_fd(master.into_raw_fd()) };
    #[cfg(test)]
    tests::observe_terminal_allocation(master.as_raw_fd(), slave.as_raw_fd());
    command.stdin(std::process::Stdio::from(slave));
    // SAFETY: ioctl is a direct syscall on the child's already-installed stdin, without
    // allocation or shared locks. The preceding setsid pre_exec established this session.
    unsafe {
        command.pre_exec(|| {
            use nix::sys::signal::{
                SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal, sigaction, sigprocmask,
            };
            // An owner's nohup/blocked HUP state must not disable terminal ownership in the child.
            let default = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
            sigaction(Signal::SIGHUP, &default).map_err(std::io::Error::from)?;
            let mut hangup = SigSet::empty();
            hangup.add(Signal::SIGHUP);
            sigprocmask(SigmaskHow::SIG_UNBLOCK, Some(&hangup), None)
                .map_err(std::io::Error::from)?;
            // libc's request constant and ioctl argument widths differ on Darwin.
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(master)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn terminal_slave_name(master: &nix::pty::PtyMaster) -> std::io::Result<std::path::PathBuf> {
    nix::pty::ptsname_r(master)
        .map(Into::into)
        .map_err(std::io::Error::from)
}

#[cfg(target_os = "macos")]
fn terminal_slave_name(master: &nix::pty::PtyMaster) -> std::io::Result<std::path::PathBuf> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    // Darwin's public ttycom.h defines TIOCPTYGNAME as an output of 128 bytes.
    // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/ttycom.h
    let mut name = [0_u8; 128];
    // SAFETY: this request writes exactly the ABI-sized, initialized buffer above.
    if unsafe {
        libc::ioctl(
            master.as_raw_fd(),
            libc::TIOCPTYGNAME as _,
            name.as_mut_ptr(),
        )
    } == -1
    {
        return Err(std::io::Error::last_os_error());
    }
    let name = std::ffi::CStr::from_bytes_until_nul(&name)
        .map_err(|_| std::io::Error::other("terminal name lacks a terminator"))?;
    Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
        name.to_bytes(),
    )))
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
fn terminal_slave_name(_: &nix::pty::PtyMaster) -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic terminal input is unsupported on this platform",
    ))
}
#[cfg(test)]
pub(crate) use timed::{TEST_DRAIN_STARTED, TEST_REAP_STARTED, TestPhaseObserver};

/// Read `reader` into `buf` with `cap` enforced **before** buffering, then drain and DISCARD the rest; `buf.len() > cap` is the truncation signal.
/// The tail is drained (without counting) because stopping at the cap would leave the pipe full and block the child on its next `write`.
pub async fn read_capped<R>(reader: &mut R, cap: usize, buf: &mut Vec<u8>) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    // `saturating_add`, not `+`: a `usize::MAX` cap wrapped to `take(0)` in release, returning an empty answer and skipping the drain.
    let bounded = cap.saturating_add(1) as u64;
    (&mut *reader).take(bounded).read_to_end(buf).await?;
    if buf.len() > cap {
        // Keep drain storage off the inline async state of nested command futures.
        let mut sink = vec![0u8; 8 * 1024];
        // A drain error is not the caller's problem: the answer is already capped, and the child is about to be reaped either way.
        while matches!(reader.read(&mut sink).await, Ok(n) if n > 0) {}
    }
    Ok(())
}

/// Make the spawned child a session/process-group leader (`setsid` in `pre_exec`, so pgid == pid), so one `kill(-pgid)` reaches every descendant.
#[cfg(unix)]
pub fn set_process_group_leader(cmd: &mut tokio::process::Command) {
    // SAFETY: `setsid(2)` is async-signal-safe and runs in the forked child
    // before exec; it touches no memory shared with the parent.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn set_process_group_leader(_cmd: &mut tokio::process::Command) {}

/// SIGKILL a whole process group. `pgid <= 1` is refused inside `signal_process_group`, so a corrupted or zero pgid can never become a broadcast.
#[cfg(unix)]
pub fn kill_process_group(pgid: i32) {
    signal_process_group(pgid, libc::SIGKILL);
}

#[cfg(not(unix))]
pub fn kill_process_group(_pgid: i32) {}

/// A spawned child that is its own process-group leader, and that sweeps that group on drop unless the group has been explicitly released.
/// `Drop` covers only the paths where the leader was never reaped; a caller that reaps via [`wait_and_release_group`](Self::wait_and_release_group) takes the pgid out of the guard and must sweep it itself. Sweeping after the reap leaves a pid-recycle window bounded by sequential pid allocation; sweeping before would cost exit-status fidelity.
pub struct GroupChild {
    child: tokio::process::Child,
    /// The group to sweep on drop. `None` once released or already swept.
    pgid: Option<i32>,
}

impl GroupChild {
    /// `pgid == pid` because the child is a session leader; `None` (the child is already gone) disarms rather than guessing.
    fn new(child: tokio::process::Child) -> Self {
        let pgid = child.id().map(|p| p as i32);
        Self { child, pgid }
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub fn stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.stdin.take()
    }

    /// Kill the owned group before reaping the leader; Drop uses the same cleanup.
    pub fn kill_group(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            kill_process_group(pgid);
        }
    }

    pub fn start_kill(&mut self) -> std::io::Result<()> {
        self.child.start_kill()
    }

    pub fn stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.stderr.take()
    }

    /// Reap the leader, then hand the caller the pgid it is now responsible for sweeping. The `take()` happens after the await, never before: a cancelled wait must leave the guard armed.
    pub async fn wait_and_release_group(
        &mut self,
    ) -> (std::io::Result<std::process::ExitStatus>, ReleasedGroup) {
        let status = self.child.wait().await;
        (status, ReleasedGroup(self.pgid.take()))
    }
}

/// The process group a reaped [`GroupChild`] handed back, which its new owner must sweep; `#[must_use]` so dropping it on the floor is a deliberate act.
#[must_use = "a released process group must be swept, or the call's descendants \
              outlive it — see GroupChild"]
pub struct ReleasedGroup(Option<i32>);

impl ReleasedGroup {
    /// SIGKILL the group. `None` (the child was already gone at spawn time) is a no-op rather than a guess.
    pub fn sweep(self) {
        if let Some(pgid) = self.0 {
            kill_process_group(pgid);
        }
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// The deadline passed to [`spawn_within`] elapsed before the child existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnTimedOut;

/// Start the same guarded spawn as `spawn_within`, retaining its join until the caller has
/// consumed and cleaned up a possibly late child. Owners with an admission fence must retain
/// this handle after cancellation/deadline; a detached spawn is not cleanup confirmation.
pub fn spawn_tracked(
    mut cmd: tokio::process::Command,
) -> tokio::task::JoinHandle<std::io::Result<GroupChild>> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _guard = handle.enter();
        cmd.spawn().map(GroupChild::new)
    })
}

/// Spawn `cmd` off the async path, bounded by `deadline`. With `pre_exec` set, `Command::spawn` is `fork` + `execve` and the parent blocks until the child's `execve` returns, so a timeout around an inline `spawn()` can never fire; the `Handle::enter()` guard is load-bearing because `PollEvented` registration panics without a runtime context.
/// The closure builds a [`GroupChild`], so the group is swept by that value's `Drop` wherever it ends up. Residuals: tokio can return `Err` after forking (pipe/reaper registration failure) and that process leaks; a `fork` wedged on a dead mount cannot be cancelled and occupies a blocking thread; a tool that daemonizes properly (own `fork` + `setsid`) leaves the group.
pub async fn spawn_within(
    cmd: tokio::process::Command,
    deadline: tokio::time::Instant,
) -> Result<std::io::Result<GroupChild>, SpawnTimedOut> {
    let mut join = spawn_tracked(cmd);

    match tokio::time::timeout_at(deadline, &mut join).await {
        Ok(Ok(res)) => Ok(res),
        Ok(Err(e)) => Ok(Err(std::io::Error::other(format!(
            "spawn task failed: {e}"
        )))),
        // Dropping the handle detaches the task; whatever `GroupChild` it eventually produces sweeps its own group when the unclaimed output is dropped.
        Err(_elapsed) => Err(SpawnTimedOut),
    }
}

#[cfg(unix)]
/// Send `signal` to the owned process **group** `pgid` (`kill(-pgid, signal)`). A non-positive `pgid` is refused; `ESRCH` is swallowed.
/// Returns `true` if the signal was delivered to at least one process.
pub fn signal_process_group(pgid: i32, signal: libc::c_int) -> bool {
    if pgid <= 1 {
        // kill(-1, …) would signal every process we can reach; kill(0, …) would hit our own group.
        tracing::warn!(
            pgid,
            "planner push: refusing to signal non-positive process group"
        );
        return false;
    }
    // SAFETY: `kill(2)` with a negative pid targets the process group
    // `pgid`. No memory is shared; the call is async-signal-safe.
    let rc = unsafe { libc::kill(-pgid, signal) };
    if rc == 0 {
        true
    } else {
        let err = std::io::Error::last_os_error();
        // ESRCH (no such process group) is the expected terminal state.
        tracing::debug!(pgid, signal, error = %err, "planner push: kill(-pgid) returned error (likely already gone)");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    type AllocationPause = (
        std::sync::mpsc::SyncSender<(i32, i32)>,
        std::sync::mpsc::Receiver<()>,
    );
    #[cfg(unix)]
    std::thread_local! {
        static ALLOCATION_PAUSE: std::cell::RefCell<Option<AllocationPause>> = const {std::cell::RefCell::new(None)};
    }
    #[cfg(unix)]
    pub(super) fn observe_terminal_allocation(master: i32, slave: i32) {
        ALLOCATION_PAUSE.with(|pause| {
            if let Some((ready, release)) = pause.borrow_mut().take() {
                ready.send((master, slave)).unwrap();
                release.recv().unwrap();
            }
        });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_creation_never_exposes_descriptors_to_a_concurrent_exec() {
        let (ready, allocated) = std::sync::mpsc::sync_channel(1);
        let (release, proceed) = std::sync::mpsc::sync_channel(1);
        let owner = std::thread::spawn(move || {
            ALLOCATION_PAUSE.with(|pause| *pause.borrow_mut() = Some((ready, proceed)));
            let mut command = tokio::process::Command::new("/bin/sleep");
            set_process_group_leader(&mut command);
            let terminal = terminal_input(&mut command).unwrap();
            drop(terminal);
        });
        let (master, slave) = allocated
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let mut sibling = tokio::process::Command::new("/usr/bin/python3");
        sibling
            .args([
                "-c",
                "import os,sys; print(sum(os.isatty(int(fd)) for fd in sys.argv[1:]))",
            ])
            .arg(master.to_string())
            .arg(slave.to_string())
            .kill_on_drop(true);
        set_process_group_leader(&mut sibling);
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(3), sibling.output()).await;
        release.send(()).unwrap();
        owner.join().unwrap();
        let output = output.unwrap().unwrap();
        assert!(output.status.success());
        assert_eq!(
            std::str::from_utf8(&output.stdout).unwrap().trim(),
            "0",
            "an unrelated exec must never inherit a query's terminal"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_input_is_a_tty_without_replacing_group_lifecycle() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "test -t 0"]);
        set_process_group_leader(&mut command);
        let terminal = terminal_input(&mut command).unwrap();
        use std::os::fd::AsRawFd;
        let flags = nix::fcntl::fcntl(terminal.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFD).unwrap();
        assert_ne!(
            flags & nix::fcntl::FdFlag::FD_CLOEXEC.bits(),
            0,
            "a terminal master must not leak to an unrelated exec"
        );
        let mut child = spawn_within(
            command,
            tokio::time::Instant::now() + std::time::Duration::from_secs(3),
        )
        .await
        .unwrap()
        .unwrap();
        let (status, group) = child.wait_and_release_group().await;
        group.sweep();
        assert!(status.unwrap().success());
    }

    #[test]
    fn read_capped_future_fits_its_byte_budget() {
        let mut reader = tokio::io::empty();
        let mut output = Vec::new();
        let future = read_capped(&mut reader, 16, &mut output);
        // This future is composed into stdout/stderr joins and gate recovery.
        // Keep per-reader state below 1 KiB rather than embedding drain storage
        // in every parent future, including commands whose output is not capped.
        let bytes = std::mem::size_of_val(&future);
        assert!(bytes < 1024, "bounded reader future uses {bytes} bytes");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn tracked_spawn_retains_a_child_blocked_past_the_deadline() {
        use std::os::fd::AsRawFd;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (parent, child) = std::os::unix::net::UnixStream::pair().unwrap();
        parent.set_nonblocking(true).unwrap();
        let mut parent = tokio::net::UnixStream::from_std(parent).unwrap();
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("300").kill_on_drop(true);
        set_process_group_leader(&mut command);
        // SAFETY: the child uses only async-signal-safe read/write on its pre-created socket.
        unsafe {
            command.pre_exec(move || {
                let mut byte = 1_u8;
                if libc::write(child.as_raw_fd(), (&byte as *const u8).cast(), 1) != 1
                    || libc::read(child.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) != 1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut pending = spawn_tracked(command);
        let mut byte = [0];
        parent.read_exact(&mut byte).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        assert!(
            !pending.is_finished(),
            "deadline does not confirm absence of the late child"
        );
        parent.write_all(&[1]).await.unwrap();
        let mut child = pending.await.unwrap().unwrap();
        child.kill_group();
        let (status, group) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            child.wait_and_release_group(),
        )
        .await
        .unwrap();
        group.sweep();
        assert!(!status.unwrap().success());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_master_closure_hangs_up_the_owned_child_group() {
        use std::os::unix::process::ExitStatusExt;
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("300").kill_on_drop(true);
        set_process_group_leader(&mut command);
        // SAFETY: mutate signal state only in the forked child, never the concurrent test host.
        unsafe {
            command.pre_exec(|| {
                use nix::sys::signal::{
                    SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal, sigaction,
                    sigprocmask,
                };
                let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
                sigaction(Signal::SIGHUP, &ignore).map_err(std::io::Error::from)?;
                let mut blocked = SigSet::empty();
                blocked.add(Signal::SIGHUP);
                sigprocmask(SigmaskHow::SIG_BLOCK, Some(&blocked), None)
                    .map_err(std::io::Error::from)?;
                Ok(())
            });
        }
        let terminal = terminal_input(&mut command).unwrap();
        let mut child = spawn_within(
            command,
            tokio::time::Instant::now() + std::time::Duration::from_secs(3),
        )
        .await
        .unwrap()
        .unwrap();
        drop(terminal);
        let finished = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            child.wait_and_release_group(),
        )
        .await;
        if finished.is_err() {
            child.kill_group();
            let (_, group) = child.wait_and_release_group().await;
            group.sweep();
            panic!("closing the owner's terminal must stop its child after an owner crash");
        }
        let (status, group) = finished.unwrap();
        group.sweep();
        assert_eq!(status.unwrap().signal(), Some(libc::SIGHUP));
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn terminal_hangup_reaches_foreground_descendants_without_a_group_sweep() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("leaf");
        let mut command = tokio::process::Command::new("/usr/bin/python3");
        command
            .args([
                "-c",
                concat!(
                    "import subprocess,sys,pathlib,time; ",
                    "p=subprocess.Popen(['/bin/sleep','300']); ",
                    "pathlib.Path(sys.argv[1]).write_text(str(p.pid)); ",
                    "time.sleep(300)"
                ),
            ])
            .arg(&leaf)
            .kill_on_drop(true);
        set_process_group_leader(&mut command);
        let terminal = terminal_input(&mut command).unwrap();
        let mut child = spawn_within(
            command,
            tokio::time::Instant::now() + std::time::Duration::from_secs(3),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !leaf.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid = std::fs::read_to_string(leaf).unwrap();
        drop(terminal);
        let observed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                if stat.is_err() || stat.unwrap().split_once(") ").unwrap().1.starts_with('Z') {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        child.kill_group();
        let (_, group) = child.wait_and_release_group().await;
        group.sweep();
        assert!(
            observed.is_ok(),
            "terminal ownership stops descendants even when its process owner cannot sweep"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_hangup_stops_late_exec_after_the_owner_master_closed() {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::ExitStatusExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (parent, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
        parent.set_nonblocking(true).unwrap();
        let mut parent = tokio::net::UnixStream::from_std(parent).unwrap();
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("300").kill_on_drop(true);
        set_process_group_leader(&mut command);
        let terminal = terminal_input(&mut command).unwrap();
        // SAFETY: only direct read/write/getpid syscalls, on an already-created child socket.
        unsafe {
            command.pre_exec(move || {
                let ready = libc::getpid().to_ne_bytes();
                let mut release = 0_u8;
                if libc::write(child_socket.as_raw_fd(), ready.as_ptr().cast(), ready.len())
                    != ready.len() as isize
                    || libc::read(
                        child_socket.as_raw_fd(),
                        (&mut release as *mut u8).cast(),
                        1,
                    ) != 1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let pending = spawn_tracked(command);
        let mut ready = [0; 4];
        parent.read_exact(&mut ready).await.unwrap();
        let pid = i32::from_ne_bytes(ready);
        drop(terminal);
        parent.write_all(&[1]).await.unwrap();
        match pending.await.unwrap() {
            Ok(mut child) => {
                let ended = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    child.wait_and_release_group(),
                )
                .await;
                if ended.is_err() {
                    child.kill_group();
                    let (_, group) = child.wait_and_release_group().await;
                    group.sweep();
                    panic!("a late exec must not retain the dead owner's terminal master");
                }
                let (status, group) = ended.unwrap();
                group.sweep();
                assert_eq!(status.unwrap().signal(), Some(libc::SIGHUP));
            }
            Err(_) => assert_eq!(
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
                Err(nix::errno::Errno::ESRCH)
            ),
        }
    }
}

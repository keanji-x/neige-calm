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

// This owner requires SIGCHLD to retain zombies and no other waiter to reap its
// children. The process-wide disposition must stay stable for this lifetime.
// Check it before spawn and again before final signalling.
// Normal cleanup kills pipe-holding descendants before releasing the leader pin.
// If identity is lost, abandon cleanup: descendants may survive, but a recycled
// numeric PID/PGID is never an acceptable cleanup target.
struct Group {
    child: Child,
    lost: bool,
    finished: bool,
}
impl Group {
    fn new(child: Child) -> Self {
        Self {
            child,
            lost: false,
            finished: false,
        }
    }

    fn observe_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.lost {
            return Err(pin_lost());
        }
        match observe_exit(&self.child) {
            Ok(status) => Ok(status),
            Err(error) => {
                // ECHILD explicitly means the identity pin is gone. Any other
                // probe failure also denies signal authority; never guess.
                self.lost = true;
                if error.raw_os_error() == Some(libc::ECHILD) {
                    Err(pin_lost())
                } else {
                    Err(io::Error::other(format!(
                        "Git leader pin probe failed: {error}"
                    )))
                }
            }
        }
    }

    // The only group-signal entry point. Even a previously observed zombie must
    // be proved waitable again here: capture may exit via success or any error.
    fn terminate(&mut self) -> io::Result<()> {
        let result = self.terminate_owned();
        // Mark completion only after successful reap or an explicit cleanup failure.
        self.finished = true;
        result
    }

    fn terminate_owned(&mut self) -> io::Result<()> {
        if let Err(error) = require_zombie_retention() {
            self.lost = true;
            return Err(error);
        }
        self.observe_exit()?;
        let result = self.signal_group();
        if result.is_ok() || matches!(self.observe_exit(), Ok(Some(_))) {
            // On a failed signal/snapshot, release an exited child only after
            // revalidating our exact pin. Never block waiting for a live denied child.
            self.child.wait()?;
        }
        result
    }

    fn signal_group(&mut self) -> io::Result<()> {
        #[cfg(test)]
        tests::observe_before_kill(self.child.id());
        // SAFETY: the preceding WNOWAIT probe proves this is still our owned
        // live/unreaped child; process_group(0) made its pid the group id.
        let result = signal_group(self.child.id());
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            #[cfg(target_os = "macos")]
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => {
                // Observe AFTER denial: even the last live member can exit between
                // any earlier snapshot and kill. Only a fresh complete observation
                // of this pinned original group may classify Darwin's zombie EPERM.
                self.observe_exit()?;
                #[cfg(test)]
                tests::snapshot_fault()?;
                let leader = crate::execution::macos::process_info(self.child.id())?;
                let members = crate::execution::macos::group_members(&leader)?;
                if members.iter().all(|p| p.pbi_status == libc::SZOMB) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn finish<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        self.terminate()?;
        result
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.terminate()
        {
            tracing::error!(%error, "Git group cleanup abandoned");
        }
    }
}
fn signal_group(pid: u32) -> io::Result<()> {
    #[cfg(test)]
    if let Some(errno) = tests::signal_error() {
        return Err(io::Error::from_raw_os_error(errno));
    }
    // SAFETY: caller retains and just validated the original child/group pin.
    if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn pin_lost() -> io::Error {
    io::Error::other("Git leader identity pin lost; refusing group signal and wait")
}
fn require_zombie_retention() -> io::Result<()> {
    // SAFETY: a null act only queries the current process-wide disposition.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(io::Error::other(
            "Git requires SIGCHLD zombie retention; SIG_IGN/SA_NOCLDWAIT is unsupported",
        ));
    }
    Ok(())
}
// std::process::Child::try_wait consumes the zombie. Observe the owned child
// without reaping instead; only Group::terminate may wait, after the final group signal.
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
    require_zombie_retention()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = Group::new(command.spawn()?);
    let stdout = child.child.stdout.take().expect("stdout piped");
    let stderr = child.child.stderr.take().expect("stderr piped");
    prepare_capture(child, stdout, stderr, deadline)
}

fn prepare_capture(
    mut child: Group,
    stdout: impl Read + AsRawFd,
    stderr: impl Read + AsRawFd,
    deadline: Instant,
) -> io::Result<Output> {
    let prepared = nonblocking(&stdout).and_then(|()| nonblocking(&stderr));
    if let Err(error) = prepared {
        return child.finish(Err(error));
    }
    capture(child, stdout, stderr, deadline)
}

fn capture(
    mut child: Group,
    stdout: impl Read,
    stderr: impl Read,
    deadline: Instant,
) -> io::Result<Output> {
    let result = capture_inner(&mut child, stdout, stderr, deadline);
    child.finish(result)
}

fn capture_inner(
    child: &mut Group,
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
            status = child.observe_exit()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn bounded_git_captures_zombie_only_git() {
        let result = Command::new("git")
            .arg("--version")
            .bounded_output(Instant::now() + Duration::from_secs(5))
            .unwrap();
        assert!(result.status.success());
        assert!(result.stdout.starts_with(b"git version "));
    }

    thread_local! {
        static PINNED_BEFORE_KILL: Cell<Option<bool>> = const { Cell::new(None) };
        static SIGNAL_ERROR: Cell<Option<i32>> = const { Cell::new(None) };
        #[cfg(target_os = "macos")]
        static SNAPSHOT_ERROR: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn signal_error() -> Option<i32> {
        SIGNAL_ERROR.take()
    }
    #[cfg(target_os = "macos")]
    pub(super) fn snapshot_fault() -> io::Result<()> {
        if SNAPSHOT_ERROR.replace(false) {
            Err(io::Error::from_raw_os_error(libc::EIO))
        } else {
            Ok(())
        }
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

    struct Invalid;
    impl AsRawFd for Invalid {
        fn as_raw_fd(&self) -> i32 {
            -1
        }
    }
    impl Read for Invalid {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            unreachable!()
        }
    }

    fn run_case(kind: &str) {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant");
        let script = match kind {
            "success" => "sleep 30 >&- 2>&- & echo $! > \"$1\"; exit 7",
            "cap" => {
                "sh -c 'trap \"\" PIPE HUP; while :; do printf \"%8192s\" x; done' 2>/dev/null & echo $! > \"$1\"; exit 7"
            }
            _ => "sleep 30 & echo $! > \"$1\"; exit 7",
        };
        let mut child = Group::new(
            Command::new("sh")
                .args(["-c", script, "fixture"])
                .arg(&pid_file)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let pid = child.child.id();
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
        let stdout = child.child.stdout.take().unwrap();
        let stderr = child.child.stderr.take().unwrap();
        nonblocking(&stdout).unwrap();
        nonblocking(&stderr).unwrap();
        PINNED_BEFORE_KILL.set(None);
        let deadline = Instant::now()
            + if kind == "timeout" {
                Duration::from_millis(150)
            } else {
                Duration::from_secs(5)
            };
        let result = if kind == "fcntl_error" {
            prepare_capture(child, Invalid, stderr, deadline)
        } else if kind == "read_error" {
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
            "fcntl_error" => assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EBADF)),
            "read_error" => assert_eq!(
                result.unwrap_err().to_string(),
                "injected pipe read failure"
            ),
            _ => unreachable!(),
        }
        let cleanup = Instant::now() + Duration::from_secs(5);
        loop {
            #[cfg(target_os = "linux")]
            let exited = std::fs::read_to_string(format!("/proc/{descendant}/stat"))
                .map(|stat| stat.split_once(") ").unwrap().1.starts_with('Z'))
                .unwrap_or(true);
            #[cfg(target_os = "macos")]
            let exited = crate::execution::macos::process_info(descendant)
                .map(|info| info.pbi_status == libc::SZOMB)
                .unwrap_or_else(|error| {
                    assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
                    true
                });
            if exited {
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

    struct Disposition(libc::sigaction);
    impl Disposition {
        fn set(handler: libc::sighandler_t, flags: libc::c_int) -> Self {
            // SAFETY: valid initialized sigaction storage; nextest isolates each test.
            unsafe {
                let mut old = std::mem::zeroed();
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler;
                action.sa_flags = flags;
                libc::sigemptyset(&mut action.sa_mask);
                assert_eq!(libc::sigaction(libc::SIGCHLD, &action, &mut old), 0);
                Self(old)
            }
        }
    }
    impl Drop for Disposition {
        fn drop(&mut self) {
            unsafe { libc::sigaction(libc::SIGCHLD, &self.0, std::ptr::null_mut()) };
        }
    }

    fn rejects_auto_reap(handler: libc::sighandler_t, flags: libc::c_int) {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("spawned");
        let disposition = Disposition::set(handler, flags);
        let result = output(
            Command::new("sh")
                .args(["-c", "touch \"$1\"", "fixture"])
                .arg(&marker),
            Instant::now() + Duration::from_secs(2),
        );
        drop(disposition);
        assert!(!marker.exists(), "must reject before spawning");
        assert!(result.unwrap_err().to_string().contains("SIGCHLD"));
    }

    #[test]
    fn bounded_git_rejects_inherited_sigchld_ignore() {
        rejects_auto_reap(libc::SIG_IGN, 0);
    }
    #[test]
    fn bounded_git_rejects_inherited_no_cld_wait() {
        rejects_auto_reap(libc::SIG_DFL, libc::SA_NOCLDWAIT);
    }

    fn lost_pin(kind: &str) {
        let mut child = Group::new(
            Command::new("sh")
                .args(["-c", "read release; exit 0"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let disposition = Disposition::set(libc::SIG_IGN, 0);
        // EOF releases the leader only after auto-reaping is enabled. Scheduling
        // cannot let it exit and become a retained zombie before the switch.
        drop(child.child.stdin.take());
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
                break;
            }
            assert!(Instant::now() < end, "fixture was not auto-reaped");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(disposition);
        let stdout = child.child.stdout.take().unwrap();
        let stderr = child.child.stderr.take().unwrap();
        PINNED_BEFORE_KILL.set(None);
        let result = match kind {
            "fcntl_error" => prepare_capture(child, Invalid, stderr, end),
            "read_error" => {
                struct Broken;
                impl Read for Broken {
                    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                        Err(io::Error::other("injected read failure"))
                    }
                }
                capture(child, Broken, stderr, end)
            }
            "timeout" => capture(child, stdout, stderr, Instant::now()),
            "success" => capture(child, stdout, stderr, end),
            _ => unreachable!(),
        };
        assert_eq!(PINNED_BEFORE_KILL.get(), None, "lost pin must never signal");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("identity pin lost")
        );
    }
    #[test]
    fn bounded_git_lost_pin_success() {
        lost_pin("success");
    }
    #[test]
    fn bounded_git_lost_pin_timeout() {
        lost_pin("timeout");
    }
    #[test]
    fn bounded_git_lost_pin_cap() {
        // Lose ownership only at the cap boundary, after capture cached a normal
        // exit. This exercises the final probe even when no further exit poll runs.
        let mut child = Group::new(
            Command::new("sh")
                .args(["-c", "exit 0"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let pid = child.child.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !exited_unreaped(pid) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        struct ReapAtCap {
            pid: u32,
            bytes: usize,
        }
        impl Read for ReapAtCap {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                buf.fill(b'x');
                self.bytes += buf.len();
                if self.bytes > CAP {
                    // SAFETY: deliberately consume our fixture's zombie to model
                    // another waiter stealing the pin after the cached observation.
                    assert_eq!(
                        unsafe { libc::waitpid(self.pid as i32, std::ptr::null_mut(), 0) },
                        self.pid as i32
                    );
                }
                Ok(buf.len())
            }
        }
        let stderr = child.child.stderr.take().unwrap();
        PINNED_BEFORE_KILL.set(None);
        let result = capture(child, ReapAtCap { pid, bytes: 0 }, stderr, deadline);
        assert_eq!(PINNED_BEFORE_KILL.get(), None, "lost pin must never signal");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("identity pin lost")
        );
    }
    #[test]
    fn bounded_git_lost_pin_read_error() {
        lost_pin("read_error");
    }
    #[test]
    fn bounded_git_lost_pin_fcntl_error() {
        lost_pin("fcntl_error");
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
    fn bounded_git_fcntl_error_pins_leader_and_kills_pipe_holder() {
        run_case("fcntl_error");
    }
    #[test]
    fn bounded_git_read_error_pins_leader_and_kills_pipe_holder() {
        run_case("read_error");
    }

    fn fixture(script: &str) -> Group {
        Group::new(
            Command::new("sh")
                .args(["-c", script])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        )
    }
    fn await_exit(child: &Group) {
        let end = Instant::now() + Duration::from_secs(5);
        while !exited_unreaped(child.child.id()) {
            assert!(Instant::now() < end, "fixture did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn bounded_git_cleanup_denial_reaps_owned_exited_leader() {
        let mut child = fixture("exit 0");
        await_exit(&child);
        let pid = child.child.id();
        SIGNAL_ERROR.set(Some(libc::EACCES));
        let result = child.terminate();
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EACCES));
        assert!(
            !exited_unreaped(pid),
            "cleanup error must still reap owned zombie"
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_git_eperm_with_live_member_is_denied() {
        // The exited leader alone cannot certify completion. A finite descendant
        // bounds this denial fixture without signalling a group after releasing its pin.
        let mut child = fixture("sleep 2 & exit 0");
        await_exit(&child);
        let leader = crate::execution::macos::process_info(child.child.id()).unwrap();
        assert!(
            crate::execution::macos::group_members(&leader)
                .unwrap()
                .iter()
                .any(|p| p.pbi_status != libc::SZOMB)
        );
        SIGNAL_ERROR.set(Some(libc::EPERM));
        let result = child.terminate();
        // Drain the finite descendant before asserting, including under mutation.
        child
            .child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut Vec::new())
            .unwrap();
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EPERM));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_git_eperm_with_incomplete_snapshot_is_denied_and_reaped() {
        let mut child = fixture("exit 0");
        await_exit(&child);
        let pid = child.child.id();
        SIGNAL_ERROR.set(Some(libc::EPERM));
        SNAPSHOT_ERROR.set(true);
        let result = child.terminate();
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
        assert!(
            !exited_unreaped(pid),
            "failed observation must release owned zombie"
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_git_live_snapshot_then_zombie_only_signal() {
        let mut child = fixture("read release; exit 19");
        let pid = child.child.id();
        let leader = crate::execution::macos::process_info(pid).unwrap();
        let members = crate::execution::macos::group_members(&leader).unwrap();
        assert!(members.iter().any(|p| p.pbi_status != libc::SZOMB));
        drop(child.child.stdin.take());
        await_exit(&child);
        // Real Darwin kill now returns EPERM, despite the earlier live snapshot.
        child.terminate().unwrap();
        assert!(!exited_unreaped(pid));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_git_group_snapshot_rejects_changed_birth_identity() {
        let mut child = fixture("read release");
        let mut leader = crate::execution::macos::process_info(child.child.id()).unwrap();
        leader.pbi_start_tvusec ^= 1;
        let result = crate::execution::macos::group_members(&leader);
        child.terminate().unwrap();
        assert!(result.is_err());
    }
}

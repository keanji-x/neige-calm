//! macOS process-exit observation for a listener identified over a live Unix socket.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) struct ExitWatcher {
    pid: i32,
    queue: Arc<OwnedFd>,
}

impl ExitWatcher {
    pub(super) fn new(pid: i32) -> io::Result<Self> {
        if pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "positive PID required",
            ));
        }
        // SAFETY: kqueue returns a new descriptor, transferred to OwnedFd.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let queue = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        let change = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT | libc::EV_RECEIPT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let mut receipt: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: both event pointers refer to initialized storage for one event.
        let count = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &change,
                1,
                &mut receipt,
                1,
                std::ptr::null(),
            )
        };
        let flags = receipt.flags;
        let data = receipt.data;
        if count != 1 || flags & libc::EV_ERROR == 0 || data != 0 {
            return Err(if data > 0 {
                io::Error::from_raw_os_error(data as i32)
            } else if count < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::other("kqueue process watch registration was not acknowledged")
            });
        }
        Ok(Self { pid, queue })
    }

    pub(super) fn pid(&self) -> i32 {
        self.pid
    }

    async fn wait(&self, timeout: Duration) -> io::Result<bool> {
        let queue = Arc::clone(&self.queue);
        tokio::task::spawn_blocking(move || wait_blocking(queue.as_raw_fd(), timeout))
            .await
            .map_err(|error| io::Error::other(format!("kqueue wait task failed: {error}")))?
    }

    /// `Ok(true)`: the exit event was already pending and has now been
    /// retrieved; nothing was signalled. `Ok(false)`: the signal was issued
    /// (or met `ESRCH`) and the exit still has to be observed by `wait`.
    async fn signal_unless_exited(&self, pgid: Option<i32>, signal: i32) -> io::Result<bool> {
        let queue = Arc::clone(&self.queue);
        let pid = self.pid;
        tokio::task::spawn_blocking(move || {
            poll_exit_then_signal(queue.as_raw_fd(), pid, pgid, signal)
        })
        .await
        .map_err(|error| io::Error::other(format!("kqueue signal task failed: {error}")))?
    }
}

/// Blocks until the one-shot `NOTE_EXIT` is retrieved (`Ok(true)`) or the deadline
/// passes (`Ok(false)`); `EINTR` is retried against the original deadline.
fn wait_blocking(queue: i32, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let seconds = remaining.as_secs().min(libc::time_t::MAX as u64) as libc::time_t;
        let timespec = libc::timespec {
            tv_sec: seconds,
            tv_nsec: remaining.subsec_nanos() as libc::c_long,
        };
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: the queue is held open by the caller; event and timespec are valid.
        let count = unsafe { libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, &timespec) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                if remaining > Duration::ZERO {
                    continue;
                }
                return Ok(false);
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(false);
        }
        let flags = event.flags;
        let data = event.data;
        if flags & libc::EV_ERROR != 0 && data != 0 {
            return Err(io::Error::from_raw_os_error(data as i32));
        }
        return Ok(true);
    }
}

/// Runs on ONE blocking thread with no scheduling gap between the poll and the `kill`:
/// an `EVFILT_PROC` watch does not reserve the pid/pgid, so a reaped listener could
/// otherwise be signalled at a recycled identifier. `ESRCH` alone proves nothing.
fn poll_exit_then_signal(queue: i32, pid: i32, pgid: Option<i32>, signal: i32) -> io::Result<bool> {
    if wait_blocking(queue, Duration::ZERO)? {
        return Ok(true);
    }
    let target = signal_target(pid, pgid)?;
    // SAFETY: the target is the socket-identified PID or its process group.
    if unsafe { libc::kill(target, signal) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(false);
        }
        return Err(error);
    }
    Ok(false)
}

fn signal_target(pid: i32, pgid: Option<i32>) -> io::Result<i32> {
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "positive PID required",
        ));
    }
    // The app-server spawn invariant makes it its process-group leader. Never
    // turn an arbitrary observed group (especially pgid 1) into kill(-pgid).
    Ok(match pgid {
        Some(group) if group > 1 && group == pid => -group,
        _ => pid,
    })
}

/// SIGTERM, grace, SIGKILL, 500 ms. `Ok(())` only on a `NOTE_EXIT` retrieved from the
/// kqueue registered for the socket-identified pid. A group member that ignores SIGTERM
/// and outlives the leader is left running (macOS has no `/proc` identity to sweep by).
pub(super) async fn terminate_listener(
    watcher: ExitWatcher,
    pgid: Option<i32>,
    grace: Duration,
) -> io::Result<()> {
    if watcher.signal_unless_exited(pgid, libc::SIGTERM).await? {
        return Ok(());
    }
    if watcher.wait(grace).await? {
        return Ok(());
    }
    tracing::warn!(
        target: "shared_codex_daemon::stop",
        pid = watcher.pid(),
        pgid,
        grace_ms = grace.as_millis() as u64,
        "stop-grace ceiling elapsed without a verified exit; escalating to SIGKILL"
    );
    if watcher.signal_unless_exited(pgid, libc::SIGKILL).await? {
        return Ok(());
    }
    if watcher.wait(Duration::from_millis(500)).await? {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "listener did not exit after SIGKILL",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::process::CommandExt, process::Command};

    struct KillGroupOnDrop(i32);
    impl Drop for KillGroupOnDrop {
        fn drop(&mut self) {
            unsafe { libc::kill(-self.0, libc::SIGKILL) };
        }
    }

    #[tokio::test]
    async fn terminate_listener_waits_for_grace_before_killing_term_ignoring_process() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done");
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let _cleanup = KillGroupOnDrop(pid);
        let watcher = ExitWatcher::new(pid).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let grace = Duration::from_millis(100);
        let started = Instant::now();
        terminate_listener(watcher, Some(pid), grace).await.unwrap();
        assert!(started.elapsed() >= grace);
        assert!(!child.wait().unwrap().success());
    }

    #[tokio::test]
    async fn terminate_listener_returns_once_a_term_cooperative_listener_exits() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("trap 'exit 0' TERM; while :; do sleep 1; done");
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let _cleanup = KillGroupOnDrop(pid);
        let watcher = ExitWatcher::new(pid).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        terminate_listener(watcher, Some(pid), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(child.wait().unwrap().success());
    }

    #[tokio::test]
    async fn terminate_listener_observes_an_exit_that_preceded_the_first_signal() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("while :; do sleep 1; done");
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let _cleanup = KillGroupOnDrop(pid);
        let watcher = ExitWatcher::new(pid).unwrap();
        // Reaped before the first signal: SIGTERM at `pid` would get ESRCH, and
        // the only exit proof left is the NOTE_EXIT already queued on the watch.
        // SAFETY: `pid` is this test's own unreaped child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        assert!(!child.wait().unwrap().success());
        let grace = Duration::from_secs(1);
        let started = Instant::now();
        terminate_listener(watcher, Some(pid), grace).await.unwrap();
        assert!(started.elapsed() < grace);
    }

    #[test]
    fn signal_target_uses_only_the_listeners_own_safe_process_group() {
        assert_eq!(signal_target(42, Some(42)).unwrap(), -42);
        assert_eq!(signal_target(42, Some(1)).unwrap(), 42);
        assert_eq!(signal_target(42, Some(41)).unwrap(), 42);
        assert!(signal_target(0, None).is_err());
    }
}

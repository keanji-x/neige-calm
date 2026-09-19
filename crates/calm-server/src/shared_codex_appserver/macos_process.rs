//! macOS process-exit observation for a listener identified over a live Unix socket.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::Arc,
    time::Duration,
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
}

fn wait_blocking(queue: i32, timeout: Duration) -> io::Result<bool> {
    let seconds = timeout.as_secs().min(libc::time_t::MAX as u64) as libc::time_t;
    let timeout = libc::timespec {
        tv_sec: seconds,
        tv_nsec: timeout.subsec_nanos() as libc::c_long,
    };
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    // SAFETY: the queue is held open by the caller; event and timeout are valid.
    let count = unsafe { libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, &timeout) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count == 0 {
        return Ok(false);
    }
    let flags = event.flags;
    let data = event.data;
    if flags & libc::EV_ERROR != 0 && data != 0 {
        return Err(io::Error::from_raw_os_error(data as i32));
    }
    Ok(true)
}

fn signal(pid: i32, pgid: Option<i32>, signal: i32) -> io::Result<()> {
    let target = signal_target(pid, pgid)?;
    // SAFETY: the target is the socket-identified PID or its process group.
    if unsafe { libc::kill(target, signal) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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

pub(super) async fn terminate_listener(
    watcher: ExitWatcher,
    pgid: Option<i32>,
    grace: Duration,
) -> io::Result<()> {
    signal(watcher.pid(), pgid, libc::SIGTERM)?;
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
    // The kqueue watch proves the original leader is still alive, pinning both
    // its PID and process group while this escalation signal is issued.
    signal(watcher.pid(), pgid, libc::SIGKILL)?;
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
    use std::{os::unix::process::CommandExt, process::Command, time::Instant};

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

    #[test]
    fn signal_target_uses_only_the_listeners_own_safe_process_group() {
        assert_eq!(signal_target(42, Some(42)).unwrap(), -42);
        assert_eq!(signal_target(42, Some(1)).unwrap(), 42);
        assert_eq!(signal_target(42, Some(41)).unwrap(), 42);
        assert!(signal_target(0, None).is_err());
    }
}

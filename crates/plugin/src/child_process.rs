//! Child-process primitives shared by the connector runtimes: a capture bounded BEFORE it buffers, and a process-group kill that reaches descendants (`Child::kill` / `kill_on_drop` reach the direct child only).

use tokio::io::{AsyncRead, AsyncReadExt as _};

mod bounded;
mod env;
mod timed;
pub use bounded::{BoundedRunError, run_bounded};
pub use env::inherited_env;
pub use timed::{ChildFinishError, finish_within};
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
        let mut sink = [0u8; 8 * 1024];
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

    pub fn stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub fn stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.stdin.take()
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
        if let Some(pgid) = self.pgid.take() {
            kill_process_group(pgid);
        }
    }
}

/// The deadline passed to [`spawn_within`] elapsed before the child existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnTimedOut;

/// Spawn `cmd` off the async path, bounded by `deadline`. With `pre_exec` set, `Command::spawn` is `fork` + `execve` and the parent blocks until the child's `execve` returns, so a timeout around an inline `spawn()` can never fire; the `Handle::enter()` guard is load-bearing because `PollEvented` registration panics without a runtime context.
/// The closure builds a [`GroupChild`], so the group is swept by that value's `Drop` wherever it ends up. Residuals: tokio can return `Err` after forking (pipe/reaper registration failure) and that process leaks; a `fork` wedged on a dead mount cannot be cancelled and occupies a blocking thread; a tool that daemonizes properly (own `fork` + `setsid`) leaves the group.
pub async fn spawn_within(
    mut cmd: tokio::process::Command,
    deadline: tokio::time::Instant,
) -> Result<std::io::Result<GroupChild>, SpawnTimedOut> {
    let handle = tokio::runtime::Handle::current();
    let mut join = tokio::task::spawn_blocking(move || {
        let _guard = handle.enter();
        cmd.spawn().map(GroupChild::new)
    });

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

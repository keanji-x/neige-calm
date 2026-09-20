//! Child-process primitives shared by the connector runtimes: a capture bounded BEFORE it buffers, and a process-group kill that reaches descendants (`Child::kill` / `kill_on_drop` reach the direct child only).

use tokio::io::{AsyncRead, AsyncReadExt as _};

mod timed;
pub(crate) use timed::{ChildFinishError, finish_within};
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
    crate::proc_identity::signal_process_group(pgid, libc::SIGKILL);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A small source cannot distinguish buffer-then-truncate from cap-then-buffer; here the source is 4 MiB and the assertion is on what was READ.
    #[tokio::test]
    async fn a_stream_far_over_the_cap_is_never_buffered_whole() {
        const CAP: usize = 64;
        let src = vec![b'a'; 4 * 1024 * 1024];
        let mut reader = std::io::Cursor::new(src);
        let mut buf = Vec::new();
        read_capped(&mut reader, CAP, &mut buf).await.unwrap();
        assert_eq!(
            buf.len(),
            CAP + 1,
            "at most cap+1 bytes may ever be materialised"
        );
        // …and the whole stream was still consumed, so a real child is never left blocked on a full pipe.
        assert_eq!(reader.position(), 4 * 1024 * 1024);
    }

    /// Every process whose `/proc/<pid>/cmdline` mentions `needle`; a pid file cannot witness the unclaimed-child test because the sweep can outrun the shell's second command.
    #[cfg(unix)]
    fn processes_matching(needle: &str) -> Vec<i32> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return found;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            if let Ok(raw) = std::fs::read(entry.path().join("cmdline"))
                && String::from_utf8_lossy(&raw).contains(needle)
            {
                found.push(pid);
            }
        }
        found
    }

    #[cfg(unix)]
    async fn assert_all_gone(needle: &str, what: &str) {
        for _ in 0..200 {
            if processes_matching(needle).is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stragglers = processes_matching(needle);
        for pid in &stragglers {
            // SAFETY: pids we just observed, killed only so a failing assertion
            // does not leave 30-second sleeps behind.
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
        panic!("{what}: {stragglers:?} still alive");
    }

    /// `kill_on_drop` is a parameter because it reaches the DIRECT child only: each test either observes a descendant (flag on) or turns the flag off so `GroupChild::drop` is the only mechanism left.
    #[cfg(unix)]
    fn group_leader_command(
        script: &std::path::Path,
        arg: &str,
        kill_on_drop: bool,
    ) -> tokio::process::Command {
        use std::process::Stdio;
        let mut cmd = tokio::process::Command::new(script);
        cmd.arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(kill_on_drop);
        set_process_group_leader(&mut cmd);
        cmd
    }

    /// A wrapper whose unique path lets `processes_matching` find the group leader; it backgrounds a long-lived grandchild and records that descendant's pid.
    #[cfg(unix)]
    fn wrapper_script(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("wrapper.sh");
        std::fs::write(
            &p,
            "#!/bin/sh\nsleep 30 >/dev/null 2>&1 &\necho $! > \"$1\"\nsleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// The single blocking thread is what makes this deterministic: with the default pool the queued spawn often completes before `timeout_at`'s first poll and the unclaimed path is never exercised.
    /// `kill_on_drop` is deliberately OFF: with it on, the leader dies from the direct-child fallback and this passes with the group sweep deleted.
    #[cfg(unix)]
    #[test]
    fn an_unclaimed_spawn_still_sweeps_its_process_group() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        rt.block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let script = wrapper_script(tmp.path());
            let needle = script.display().to_string();

            // Occupy the one blocking thread, so the spawn below is QUEUED and provably incomplete when the elapsed deadline is checked.
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            let hog = tokio::task::spawn_blocking(move || {
                // Bounded, so a failing assertion can never wedge the suite.
                let _ = blocked.recv_timeout(Duration::from_secs(10));
            });
            tokio::time::sleep(Duration::from_millis(50)).await;

            let cmd = group_leader_command(
                &script,
                &tmp.path().join("gc.pid").display().to_string(),
                false,
            );
            let past = tokio::time::Instant::now();
            assert_eq!(
                spawn_within(cmd, past).await.err(),
                Some(SpawnTimedOut),
                "the blocking pool is occupied, so the spawn cannot have completed"
            );

            // Let the queued spawn finally run. Nobody is holding its result.
            drop(release);
            let _ = hog.await;

            // `hog` finishing only frees the thread; this barrier is queued AFTER the spawn on a one-thread pool, so FIFO ordering means it cannot complete until the spawn task has.
            tokio::task::spawn_blocking(|| {})
                .await
                .expect("ordering barrier");

            assert_all_gone(&needle, "an unclaimed spawn leaked its process group").await;
        });
    }

    /// The wrapper is given time to fork its grandchild first, so this pins the descendant sweep and not merely `kill_on_drop` on the leader.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_a_claimed_child_sweeps_its_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let script = wrapper_script(tmp.path());
        let needle = script.display().to_string();
        let pidfile = tmp.path().join("gc.pid");

        // Flag ON, the production shape: the observable below is a DESCENDANT, which `kill_on_drop` cannot reach.
        let cmd = group_leader_command(&script, &pidfile.display().to_string(), true);
        let child = spawn_within(cmd, tokio::time::Instant::now() + Duration::from_secs(10))
            .await
            .expect("within the deadline")
            .expect("spawn");

        // Redirection creates the file before echo publishes the PID: wait for the complete line and keep the parsed value from that same read.
        let mut recorded_pid = None;
        for _ in 0..200 {
            if let Ok(raw) = std::fs::read_to_string(&pidfile)
                && raw.ends_with('\n')
                && let Ok(pid) = raw.trim().parse::<i32>()
                && pid > 0
            {
                recorded_pid = Some(pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let gc = recorded_pid.expect("the wrapper must publish a complete positive grandchild PID");
        let gc_start_time = crate::proc_identity::read_proc_start_time(gc);

        drop(child);

        assert_all_gone(&needle, "dropping a claimed child leaked its group").await;
        crate::test_support::assert_pid_dead(
            gc,
            gc_start_time,
            &format!("the recorded grandchild {gc} outlived the drop"),
        )
        .await;
    }

    #[tokio::test]
    async fn a_stream_under_the_cap_is_read_whole() {
        let mut reader = std::io::Cursor::new(b"hello".to_vec());
        let mut buf = Vec::new();
        read_capped(&mut reader, 64, &mut buf).await.unwrap();
        assert_eq!(buf, b"hello");
        assert!(buf.len() <= 64);
    }
}

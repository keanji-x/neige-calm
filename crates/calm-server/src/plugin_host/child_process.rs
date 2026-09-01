//! Child-process primitives shared by the connector runtimes: a capture that
//! is bounded BEFORE it buffers, and a process-group kill that reaches
//! descendants.
//!
//! Both exist because the obvious shapes are wrong in the same way — they look
//! like they enforce something and do not:
//!
//! * `read_to_end` + truncate bounds nothing. The allocation has already
//!   happened by the time anything is trimmed.
//! * `Child::kill` / `kill_on_drop` reach the DIRECT child only. A wrapper's
//!   `foo &` is orphaned onto pid 1, still holding whatever environment the
//!   caller thought it had just torn down.
//!
//! Split out of `cli_query` so it stays under the per-file size governance, and
//! because neither primitive is `cli-query`-specific.

use tokio::io::{AsyncRead, AsyncReadExt as _};

/// Read `reader` into `buf` with `cap` enforced **before** buffering, then
/// drain and DISCARD whatever else the child writes.
///
/// Same shape as `http_mcp`'s `read_capped` for response bodies:
/// `take(cap + 1)` makes "over the cap" observable without ever allocating the
/// whole stream, and `buf.len() > cap` is the truncation SIGNAL.
///
/// The tail is drained rather than simply left unread, and this is
/// load-bearing: stopping at the cap would leave the pipe buffer full, block
/// the child on its next `write`, and turn every over-cap answer into a
/// budget-expiry error instead of a truncated result. It is drained **without
/// counting**, so a caller genuinely does not know the true total — which is
/// why the truncation marker must not claim one.
pub async fn read_capped<R>(reader: &mut R, cap: usize, buf: &mut Vec<u8>) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    // `saturating_add`, not `+`: `cap` comes from `cli_query.max_output_bytes`,
    // and `usize::MAX` used to load. Debug built a panic into every such call;
    // release wrapped to `take(0)`, which returned an EMPTY answer with
    // `is_error: false` and skipped the drain — silent data loss plus a stalled
    // child (r2 G1). The manifest now has a ceiling; this is the backstop for
    // any cap that reaches here by another route.
    let bounded = cap.saturating_add(1) as u64;
    (&mut *reader).take(bounded).read_to_end(buf).await?;
    if buf.len() > cap {
        let mut sink = [0u8; 8 * 1024];
        // A drain error is not the caller's problem: the answer is already
        // capped, and the child is about to be reaped either way.
        while matches!(reader.read(&mut sink).await, Ok(n) if n > 0) {}
    }
    Ok(())
}

/// Make the spawned child a session/process-group leader, so one
/// `kill(-pgid)` reaches it AND every descendant it forked.
///
/// Same mechanism as [`crate::operation::task_verify_adapter`]'s gate wrapper:
/// `setsid` in `pre_exec`, so pgid == the child's pid.
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

/// SIGKILL a whole process group.
///
/// `pgid <= 1` is refused inside [`crate::proc_identity::signal_process_group`]:
/// `kill(-1, …)` would signal every process we can reach and `kill(0, …)` our
/// own group, so a corrupted or zero pgid can never become a broadcast.
#[cfg(unix)]
pub fn kill_process_group(pgid: i32) {
    crate::proc_identity::signal_process_group(pgid, libc::SIGKILL);
}

#[cfg(not(unix))]
pub fn kill_process_group(_pgid: i32) {}

/// A spawned child that is its own process-group leader, and that **sweeps
/// that group on drop unless the group has been explicitly released**.
///
/// # The two mechanisms, and why they are separate
///
/// `Drop` covers only the paths where the leader was never reaped: a cancelled
/// caller, a spawn whose receiver went away, an early `return`. There the pgid
/// is unambiguously still ours, because an unreaped pid cannot be recycled, and
/// the sweep provably precedes any reap.
///
/// The steady-state sweep is NOT `Drop`'s job. A caller that reaps the leader
/// via [`wait_and_release_group`](Self::wait_and_release_group) takes the pgid
/// out of the guard and must sweep it itself. That split is deliberate: while
/// `Drop` also swept the success path, deleting the explicit sweep left every
/// test green — `Drop` silently did the same work an instant later, so no
/// witness could distinguish the two and the explicit step was untested
/// (r3 H1).
///
/// # The recycle residual, stated honestly
///
/// Sweeping after the reap means the pgid could in principle name a recycled
/// process group. It is a real window, not an impossible one. It is accepted
/// because it is bounded by Linux's sequential pid allocation: between the
/// `wait()` returning and the `kill(-pgid)` microseconds later, the box would
/// have to allocate its way through the entire `pid_max` space AND land a new
/// session leader on exactly this pid. The alternative — sweeping before the
/// reap — costs exit-status fidelity on every call by a child that closes its
/// pipes and then exits normally, which is a certain, everyday defect rather
/// than a wraparound-shaped one (r3 H2).
pub struct GroupChild {
    child: tokio::process::Child,
    /// The group to sweep on drop. `None` once released or already swept.
    pgid: Option<i32>,
}

impl GroupChild {
    /// `pgid == pid` because [`set_process_group_leader`] made the child a
    /// session leader. `None` (the child is already gone) disarms rather than
    /// guessing.
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

    /// Reap the leader, then hand the caller the pgid it is now responsible for
    /// sweeping.
    ///
    /// The `take()` happens **after** the await, never before: a cancelled wait
    /// must leave the guard armed, or cancellation would silently become the
    /// one path with no teardown at all.
    ///
    /// Returning the pgid — rather than sweeping here — is what makes the
    /// caller's sweep a separately deletable, and therefore separately
    /// testable, step.
    pub async fn wait_and_release_group(
        &mut self,
    ) -> (std::io::Result<std::process::ExitStatus>, Option<i32>) {
        let status = self.child.wait().await;
        (status, self.pgid.take())
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

/// Spawn `cmd` **off the async path**, bounded by `deadline`.
///
/// # Why this is not just `cmd.spawn()`
///
/// `Command::spawn` looks non-blocking and is not. Setting `pre_exec` (which
/// [`set_process_group_leader`] does) forces std off `posix_spawn` and onto
/// `fork` + `execve`, where the PARENT blocks reading the exec-status pipe
/// until the child's `execve` returns or fails. Against a hung mount that is
/// unbounded — and `tokio::time::timeout` cancels only at await points, so a
/// timeout wrapped around an inline `spawn()` can never fire. That is the exact
/// defect the bring-up budget exists to prevent, so it must not live on the
/// bring-up path (r2 G8).
///
/// # Why `spawn_blocking` is sound here
///
/// Verified rather than assumed: tokio propagates the runtime context to
/// blocking-pool threads (`Handle::try_current()` succeeds inside
/// `spawn_blocking`), so registering the child's pipes and its reaper works
/// there. The `Handle::enter()` guard is kept anyway so the requirement is
/// stated in the code rather than relied on implicitly.
///
/// # Teardown is unconditional, by construction
///
/// The blocking closure builds a [`GroupChild`], so the process group is swept
/// by that value's `Drop` no matter WHERE it ends up (r3 H4). That matters
/// because there are three distinct ways the caller can go away and only one of
/// them used to be handled:
///
/// * the internal deadline fires — the `JoinHandle` is dropped, the closure
///   still completes, and the task's unclaimed output is dropped, sweeping the
///   group;
/// * `spawn_within`'s own future is cancelled (client hangup, an outer
///   timeout) — identical, because it is the same detached `JoinHandle`;
/// * the caller receives the child and then drops it — `GroupChild::drop`.
///
/// The previous shape adopted the child only on the FIRST of those, via a
/// detached task; a cancelled caller left the child with nothing but
/// `kill_on_drop`, which reaches the direct child alone.
///
/// # Residuals, stated accurately
///
/// A `fork`/`execve` wedged on a dead mount cannot be cancelled. The CALLER is
/// bounded by `deadline`; the blocking thread is not, and neither is runtime
/// shutdown, which waits on blocking tasks. Repeated calls against such a mount
/// will exhaust the blocking pool. This is the same accepted property
/// `connector::read_secrets` has, and it is not fixable without a
/// cancellable-exec mechanism the platform does not offer — so it is documented
/// rather than papered over.
///
/// If the runtime is torn down before the closure runs at all, the task is
/// dropped un-run and `cmd` is dropped without ever forking, so there is
/// nothing to leak.
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
        // Nothing to adopt explicitly: dropping the handle detaches the task,
        // and whatever `GroupChild` it eventually produces sweeps its own group
        // when that unclaimed output is dropped.
        Err(_elapsed) => Err(SpawnTimedOut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// #1164 P3 F2 — the cap must bound MEMORY, which means it is enforced
    /// before buffering. No matter how much the stream holds, at most `cap + 1`
    /// bytes are ever materialised.
    ///
    /// A 100-byte source against a 16-byte cap cannot make this point:
    /// buffer-then-truncate and cap-then-buffer are indistinguishable from the
    /// outside at that size. Here the source is 4 MiB and the assertion is on
    /// what was READ.
    ///
    /// Mutation witness: replace the body with `reader.read_to_end(buf).await?`
    /// and this goes red at 4194304 vs 65.
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
        // …and the whole stream was still consumed, so a real child is never
        // left blocked on a full pipe.
        assert_eq!(reader.position(), 4 * 1024 * 1024);
    }

    /// Every process whose `/proc/<pid>/cmdline` mentions `needle`.
    ///
    /// A pid file cannot witness these tests: the sweep is fast enough that a
    /// shell fixture never reaches its second command, so "the pid file is
    /// missing" is indistinguishable from "the script never ran". Scanning for
    /// a unique path is decisive in both directions — with teardown removed the
    /// `sleep 30` sits there for half a minute and the scan finds it.
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

    #[cfg(unix)]
    fn group_leader_command(script: &std::path::Path, arg: &str) -> tokio::process::Command {
        use std::process::Stdio;
        let mut cmd = tokio::process::Command::new(script);
        cmd.arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        set_process_group_leader(&mut cmd);
        cmd
    }

    /// A wrapper that backgrounds a long-lived grandchild, under a path unique
    /// to this test so `processes_matching` can find both.
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

    /// #1164 P3 r3 H4 — the spawn result must reach an owner that sweeps its
    /// process group **however the caller went away**, not only when
    /// `spawn_within`'s own internal deadline fired.
    ///
    /// `spawn_within` returns `SpawnTimedOut` and simply DROPS the
    /// `JoinHandle`. The blocking task still runs, still forks the wrapper, and
    /// the `GroupChild` it produces is never claimed by anyone — so the only
    /// thing that can reach that process group is the unclaimed value's `Drop`.
    ///
    /// Round 2 adopted the child with a detached task, which covered this path
    /// but NOT a cancelled `spawn_within` future. Making teardown a property of
    /// the VALUE covers both and needs no task at all.
    ///
    /// **The single blocking thread is what makes this deterministic**, not
    /// decoration. With the default pool the queued spawn often completes
    /// before `timeout_at`'s first poll, `spawn_within` returns `Ok`, and the
    /// unclaimed path is never exercised — the first version of this test
    /// passed alone and failed in the full parallel run for exactly that
    /// reason. Occupying the only blocking thread guarantees the task cannot
    /// have finished, so the deadline branch is forced.
    ///
    /// Mutation witness: empty `GroupChild`'s `Drop` body (`self.pgid = None;`).
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

            // Occupy the one blocking thread, so the spawn below is QUEUED and
            // provably incomplete when the elapsed deadline is checked.
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            let hog = tokio::task::spawn_blocking(move || {
                // Bounded, so a failing assertion can never wedge the suite.
                let _ = blocked.recv_timeout(Duration::from_secs(10));
            });
            tokio::time::sleep(Duration::from_millis(50)).await;

            let cmd =
                group_leader_command(&script, &tmp.path().join("gc.pid").display().to_string());
            let past = tokio::time::Instant::now();
            assert_eq!(
                spawn_within(cmd, past).await.err(),
                Some(SpawnTimedOut),
                "the blocking pool is occupied, so the spawn cannot have completed"
            );

            // Let the queued spawn finally run. Nobody is holding its result.
            drop(release);
            let _ = hog.await;

            assert_all_gone(&needle, "an unclaimed spawn leaked its process group").await;
        });
    }

    /// The other way a caller goes away: it RECEIVES the child and then drops
    /// it without ever waiting — a client hangup mid-call.
    ///
    /// Here the wrapper is given time to actually fork its grandchild first, so
    /// this pins the descendant sweep and not merely `kill_on_drop` on the
    /// leader.
    ///
    /// Mutation witness: empty `GroupChild`'s `Drop` body.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_a_claimed_child_sweeps_its_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let script = wrapper_script(tmp.path());
        let needle = script.display().to_string();
        let pidfile = tmp.path().join("gc.pid");

        let cmd = group_leader_command(&script, &pidfile.display().to_string());
        let child = spawn_within(cmd, tokio::time::Instant::now() + Duration::from_secs(10))
            .await
            .expect("within the deadline")
            .expect("spawn");

        // Let the wrapper get as far as forking and recording its grandchild,
        // so the drop below has a real descendant to reach.
        for _ in 0..200 {
            if pidfile.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let gc: i32 = std::fs::read_to_string(&pidfile)
            .expect("the wrapper must have recorded its grandchild")
            .trim()
            .parse()
            .unwrap();

        drop(child);

        assert_all_gone(&needle, "dropping a claimed child leaked its group").await;
        // SAFETY: existence probe only, no signal delivered.
        assert!(
            unsafe { libc::kill(gc, 0) } != 0,
            "the recorded grandchild {gc} outlived the drop"
        );
    }

    /// An under-cap stream is read whole, and `len > cap` — the truncation
    /// signal — stays false.
    #[tokio::test]
    async fn a_stream_under_the_cap_is_read_whole() {
        let mut reader = std::io::Cursor::new(b"hello".to_vec());
        let mut buf = Vec::new();
        read_capped(&mut reader, 64, &mut buf).await.unwrap();
        assert_eq!(buf, b"hello");
        assert!(buf.len() <= 64);
    }
}

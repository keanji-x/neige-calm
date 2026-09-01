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
    (&mut *reader).take(cap as u64 + 1).read_to_end(buf).await?;
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

/// SIGKILLs a process group unless disarmed.
///
/// Armed for exactly the window in which the group leader is still ours (not
/// yet reaped), which is what makes the pgid unambiguous: a reaped pid can be
/// recycled, and `kill(-pgid)` on a recycled group would hit strangers. Every
/// path that reaps the leader must [`disarm`](Self::disarm) first.
pub struct KillGroupOnDrop(Option<i32>);

impl KillGroupOnDrop {
    /// `pgid` is the leader's pid — `Child::id()` after
    /// [`set_process_group_leader`]. `None` (the child already exited) is a
    /// no-op rather than a guess.
    pub fn arm(pgid: Option<i32>) -> Self {
        Self(pgid)
    }

    /// Signal now and disarm, so the drop does not signal a second time.
    pub fn kill_now(&mut self) {
        if let Some(pgid) = self.0.take() {
            kill_process_group(pgid);
        }
    }

    pub fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for KillGroupOnDrop {
    /// Covers the cancellation path a direct-child `kill_on_drop` cannot: a
    /// dropped future leaves the leader unreaped, so the group is still safely
    /// addressable.
    fn drop(&mut self) {
        self.kill_now();
    }
}

#[cfg(unix)]
fn kill_process_group(pgid: i32) {
    crate::proc_identity::signal_process_group(pgid, libc::SIGKILL);
}

#[cfg(not(unix))]
fn kill_process_group(_pgid: i32) {}

#[cfg(test)]
mod tests {
    use super::*;

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

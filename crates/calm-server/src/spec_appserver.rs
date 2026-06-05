//! Process and socket utilities retained for shared codex app-server
//! supervision and boot recovery.

use std::path::Path;
use std::time::Duration;

use crate::codex_appserver::{ClientInfo, CodexAppServer};

/// #318 INV-5 (R3-B1) — read the `starttime` field (clock-ticks since
/// boot) for `pid` from `/proc/<pid>/stat`. Returns `None` if the entry
/// doesn't exist (the process is gone), the file can't be parsed, or
/// we are running on a non-Linux target.
///
/// **Why it matters.** `(pid, start_time, boot_id)` is the canonical
/// Linux identity token for a live process across reboots:
/// `start_time` is jiffies-since-boot for the creation of THAT pid
/// (invariant within a boot), and `boot_id` (a per-boot UUID from
/// `/proc/sys/kernel/random/boot_id`) distinguishes "same boot, pid
/// recycled" from "different boot entirely". After a reboot ALL
/// `start_time` values restart from 0, so the captured stamp alone
/// could in principle coincide with a fresh post-reboot pid's stamp
/// (probability is small but nonzero, especially right after boot
/// when starttime is small). The `boot_id` companion check makes the
/// triple race-free across reboots — a different boot ⇒ skip the
/// kill regardless of pid/start_time. The triple is read at spawn,
/// persisted alongside the pgid, and verified before signaling on
/// boot recovery — see [`verify_owned_pid`].
///
/// `/proc/<pid>/stat` layout (proc(5)): space-separated fields after the
/// `comm` blob (which can contain spaces/parens and is always wrapped in
/// `(…)` — split on the **last** `)` to skip it safely). `starttime` is
/// field 22 (1-indexed); after the comm-wrap split, that's index 19 of
/// the remaining tokens (we drop the first three fields `state ppid
/// pgrp` … `state` is index 0 of the post-comm split). Concretely: pid,
/// `(comm)`, state, ppid, pgrp, session, tty_nr, tpgid, flags, minflt,
/// cminflt, majflt, cmajflt, utime, stime, cutime, cstime, priority,
/// nice, num_threads, itrealvalue, **starttime** — that's index 19 in
/// the post-comm split.
#[cfg(target_os = "linux")]
pub fn read_proc_start_time(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_starttime_from_stat(&stat)
}

/// Pure parser for `/proc/<pid>/stat` field 22 (`starttime`).
///
/// Split out from [`read_proc_start_time`] so the load-bearing
/// `rsplit_once(')')` — needed because `comm` can contain `)` (e.g.
/// `(name with paren)`, `(weird)name)`, etc.) — is exercised by unit
/// tests using synthetic stat content. Production callers go through
/// [`read_proc_start_time`] which reads the file + delegates here;
/// tests can feed arbitrary strings without spawning processes whose
/// `comm` they don't control.
///
/// The cross-platform stub above this in non-Linux builds doesn't need
/// this helper (it returns `None` unconditionally), but the parser is
/// cfg-gate-free so unit tests run on every host.
pub fn parse_starttime_from_stat(content: &str) -> Option<u64> {
    // `comm` may contain `)` — strip everything up to and including the
    // LAST `)`. The remainder starts with the `state` field.
    let after = content.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    // Skip state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    // flags(6) minflt(7) cminflt(8) majflt(9) cmajflt(10) utime(11)
    // stime(12) cutime(13) cstime(14) priority(15) nice(16)
    // num_threads(17) itrealvalue(18) → starttime is index 19.
    let starttime = fields.nth(19)?;
    starttime.parse::<u64>().ok()
}

/// Non-Linux stub. Identity verification via `/proc` is Linux-specific;
/// on macOS / BSD the file does not exist. The kernel only spawns
/// `codex app-server` on Linux production hosts (the boot-recovery path
/// is Linux-only by design), but cross-platform builds still need this
/// to compile.
#[cfg(not(target_os = "linux"))]
pub fn read_proc_start_time(_pid: i32) -> Option<u64> {
    None
}

/// #318 INV-5 (R3-B1) — read the kernel's per-boot UUID
/// (`/proc/sys/kernel/random/boot_id`). The kernel generates this once
/// at boot and it survives in `/proc` for the lifetime of the running
/// kernel; every reboot rerolls it. Returns `None` on a non-Linux
/// target or a read failure (treated by [`verify_owned_pid`] as
/// "can't prove identity → skip the kill").
///
/// The value is a 36-char canonical UUID + trailing newline; we strip
/// the newline and store the canonical form on the spec card payload.
/// Equality is byte-for-byte (no UUID parsing required — both writer
/// and reader are this same fn, and the kernel never changes the
/// format mid-boot).
#[cfg(target_os = "linux")]
pub fn read_boot_id() -> Option<String> {
    let raw = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Non-Linux stub for [`read_boot_id`].
#[cfg(not(target_os = "linux"))]
pub fn read_boot_id() -> Option<String> {
    None
}

/// #318 INV-5 (R3-B1) — verify that the live process at `pid` is the
/// SAME process whose `(start_time, boot_id)` triple we captured at
/// spawn.
///
/// Returns `true` iff ALL of:
///   * the current `/proc/sys/kernel/random/boot_id` matches
///     `expected_boot_id` (i.e. no reboot since spawn — without this,
///     a coincidentally-equal cross-boot `start_time` would slip
///     through),
///   * `/proc/<pid>/stat` exists,
///   * its `starttime` (field 22) matches `expected_start_time`.
///
/// Returns `false` otherwise. The cross-reboot case is short-circuited
/// before the `/proc/<pid>/stat` read — a `boot_id` mismatch means
/// every pid in the prior boot is gone, regardless of stamp.
///
/// **Why we need this on top of [`socket_owned_by_appserver`].** The
/// socket probe (`UnixStream::connect` succeeds → trust the pgid) is a
/// good cheap proxy but suffers a TOCTOU window between the probe and
/// the subsequent `signal_process_group(pgid, …)`. Between those two
/// syscalls the kernel can reap the listener, recycle its pid/pgid to
/// an unrelated user process, and our SIGTERM/SIGKILL then lands on
/// that innocent process. `(pid, start_time, boot_id)` is race-free
/// identity:
///
///   * Cross-reboot pid recycle: `boot_id` mismatch ⇒ reject.
///   * Same-boot pid recycle: the recycled process has a strictly
///     later `start_time` (it started AFTER our stamp), so the
///     stamp comparison rejects.
///   * Liveness-only mismatch (we crashed before persisting →
///     `/proc/<pid>` is gone): the `read_proc_start_time` ENOENT
///     short-circuits to `None` ⇒ reject.
///
/// On a non-Linux target (no `/proc`) this returns `false`
/// unconditionally — the caller's fallback (skip the kill, cleanup the
/// stale socket, let the respawn rebind) is correct in that environment.
pub fn verify_owned_pid(pid: i32, expected_start_time: u64, expected_boot_id: &str) -> bool {
    // Reboot check FIRST — cheapest, and short-circuits the post-reboot
    // case (the entire prior boot's pid namespace is dead, regardless
    // of any individual pid's stamp).
    let Some(live_boot) = read_boot_id() else {
        return false;
    };
    if live_boot != expected_boot_id {
        return false;
    }
    let Some(live) = read_proc_start_time(pid) else {
        return false;
    };
    live == expected_start_time
}

/// Send `signal` to the process **group** `pgid` (`kill(-pgid, signal)`).
///
/// This is the load-bearing reap for the spec-push child. The `node`
/// launcher and the native `codex app-server` it forks share `pgid` (the
/// launcher is spawned as a group leader via `process_group(0)`), so one
/// group signal reaps both. Best-effort: a non-positive `pgid` (never
/// expected — the child is always a real positive pid) is refused so we
/// can't accidentally signal our own group or every process; `ESRCH`
/// (group already gone) is swallowed.
///
/// Returns `true` if the signal was delivered to at least one process,
/// `false` on `ESRCH`/refused (for the escalation logic in
/// [`crate::terminal_sweeper::reap_spec_push`]).
pub fn signal_process_group(pgid: i32, signal: libc::c_int) -> bool {
    if pgid <= 1 {
        // Guard against persistence corruption / a 0 pgid: kill(-1, …)
        // would signal every process we can reach, kill(0, …)/kill(-0, …)
        // would hit our own group. Never legitimate for a spawned child.
        tracing::warn!(
            pgid,
            "spec push: refusing to signal non-positive process group"
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
        tracing::debug!(pgid, signal, error = %err, "spec push: kill(-pgid) returned error (likely already gone)");
        false
    }
}

#[derive(Debug)]
pub enum SockDirCleanupOutcome {
    Removed,
    NotPresent,
    Error(std::io::Error),
}

/// Remove the listen socket and its now-empty per-card dir
/// (`<data_dir>/appserver/<card_id>/`). Best-effort: a missing socket /
/// non-empty dir is fine. Mirrors the PTY `remove_file(sock)` cleanup in
/// [`crate::terminal_sweeper::reap_terminal_artifacts`].
pub fn cleanup_sock_dir(sock: &Path) -> SockDirCleanupOutcome {
    let outcome = match std::fs::remove_file(sock) {
        Ok(()) => SockDirCleanupOutcome::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SockDirCleanupOutcome::NotPresent,
        Err(e) => SockDirCleanupOutcome::Error(e),
    };
    if let Some(dir) = sock.parent() {
        // `remove_dir` only succeeds when empty — exactly what we want
        // (don't nuke a dir that unexpectedly holds other files).
        let _ = std::fs::remove_dir(dir);
    }
    outcome
}

/// #313 problem #1 round-3 (B1) + #335 PR2 — verify that the shared codex
/// app-server socket at `sock` has a live listener BEFORE the caller signals
/// the process group.
///
/// **Why this exists.** After a host reboot a stale process group id could
/// belong to an unrelated process (PIDs/PGIDs are recycled), so a
/// `kill(-pgid, SIGTERM/SIGKILL)` could target arbitrary user processes.
/// Connect alone is not enough: a different listener on a stale path could
/// otherwise authorize a kill. We require both WebSocket connect and a JSON-RPC
/// `initialize` round-trip.
///
/// Returns `true` when the kill is **safe** (initialize succeeded — caller
/// should proceed with `signal_process_group`), `false` when the caller
/// should **skip** the kill (socket missing/refused, non-WS listener,
/// initialize failure/timeout — caller should still `cleanup_sock_dir` to
/// wipe the stale path before respawn).
///
/// Any probe failure is conservative-skip. A false-negative (we skip a kill
/// we could have done) is harmless because boot recovery's `cleanup_sock_dir`
/// plus respawn still works; a false-positive (we kill the wrong process) is
/// the bug we're guarding against.
pub async fn socket_owned_by_appserver(sock: &Path) -> bool {
    match tokio::time::timeout(Duration::from_secs(3), CodexAppServer::connect(sock)).await {
        Err(_) => {
            tracing::warn!(
                sock = %sock.display(),
                "takeover ownership probe: websocket connect timed out — skipping kill"
            );
            false
        }
        Ok(Ok((client, _notifs))) => {
            // Connect + WebSocket upgrade succeeded. Finish the ownership
            // probe with a JSON-RPC initialize round-trip so a random
            // non-codex listener on the same stale path cannot authorize a
            // process-group kill.
            let client = client.with_request_timeout(Duration::from_secs(2));
            match tokio::time::timeout(
                Duration::from_secs(3),
                client.initialize(ClientInfo {
                    name: "neige-calm-takeover-probe".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                }),
            )
            .await
            {
                Ok(Ok(_)) => {
                    tracing::debug!(
                        sock = %sock.display(),
                        "takeover ownership probe: initialize OK — socket is a codex app-server"
                    );
                    true
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        sock = %sock.display(),
                        error = %e,
                        "takeover ownership probe: initialize failed — skipping kill"
                    );
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        sock = %sock.display(),
                        "takeover ownership probe: initialize timed out — skipping kill"
                    );
                    false
                }
            }
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            if msg.contains("No such file")
                || msg.contains("os error 2")
                || msg.contains("Connection refused")
                || msg.contains("os error 111")
            {
                // ENOENT — socket file gone (graceful teardown / host
                // wipe) → no listener exists, nothing to kill.
                // ECONNREFUSED — socket path exists, no listener bound
                // (stale dirent from a crashed process) → likewise
                // nothing of ours to kill.
                tracing::info!(
                    sock = %sock.display(),
                    error = %e,
                    "takeover ownership probe: socket has no live listener — \
                     skipping kill of persisted pgid (post-reboot PID may be unrelated); \
                     caller should still cleanup_sock_dir before respawn"
                );
                false
            } else {
                // Any other error (EACCES, EAGAIN, WS handshake failure,
                // non-JSON-RPC listener, …): we can't prove ownership.
                // Default to skipping the kill — safety over reaping a
                // leaked group (the respawn path can retry, but reviving a
                // SIGKILLed user process can't).
                //
                // #315 round-4 (N3) — the conservative-skip-kill on
                // unrecognized errors trades a worst-case "stale socket
                // file leaks forever" for the worst-case "we SIGTERM/
                // SIGKILL an unrelated process group whose pid was
                // recycled into our persisted pgid slot post-reboot".
                tracing::warn!(
                    sock = %sock.display(),
                    error = %e,
                    "takeover ownership probe: app-server probe failed — skipping kill \
                     to avoid signaling unrelated process group"
                );
                false
            }
        }
    }
}

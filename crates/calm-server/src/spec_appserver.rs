//! Two-process-per-spec-card model + supervisor (issue #293 PR3a).
//!
//! ## What this is
//!
//! The push migration (#293) moved spec agents off the old 30 s long-poll
//! (pull) model entirely onto a codex app-server *push* channel — the pull
//! machinery (`calm.wait_for_events` + the Stop-hook fallback) was deleted in
//! the cutover. The architecture splits one spec card across **two processes
//! that share a single codex thread**:
//!
//!   1. a **`codex app-server --listen unix://<sock>`** child — the
//!      long-lived agent the *kernel* drives programmatically (via PR2's
//!      [`CodexAppServer`] client), and
//!   2. the browser-facing TUI, spawned under the existing PTY terminal
//!      renderer so the WS render path (`RenderPlane`/`RenderPatch`) is
//!      byte-identical to today. Non-empty goals use **`codex resume
//!      <thread_id> --remote unix://<sock>`** to rejoin the kernel-created
//!      thread. Empty goals use **`codex --remote unix://<sock>`**; the TUI
//!      fresh-starts the thread and the kernel observes the first
//!      `turn/started` before activating push delivery.
//!
//! The spike (`docs/spikes/293-appserver-thread-sharing.md`) verified the
//! `--remote` TUI and a programmatic client can drive/observe the *same*
//! thread against the real binary.
//!
//! ## What this PR (3a) does and does NOT do
//!
//! This module gives the kernel the ability to **own** the `app-server`
//! child and optionally drive turn #1 on the create-wave hot path. Push is
//! the only path (#293 cutover — no flag, no pull coexistence). The
//! [`SpecPushHandle`] is parked in a [`SpecPushRegistry`] keyed by
//! [`WaveId`] (one spec card per wave) so the dispatcher can resolve a
//! wave's app-server client and push observations onto it.
//!
//! ## PR3b — dispatcher push delivery (added on top of 3a)
//!
//! [`SpecPushHandle::push_observation`] is the delivery primitive the
//! dispatcher ([`crate::dispatcher`]) calls when a wave event (task
//! completed/failed, user-authored report edit) lands. The decision of
//! *how* to deliver is the pure [`decide`] fn over the consumer-tracked
//! [`SpecPushPhase`]:
//!
//!   * **Idle / TurnCompleted** (between turns) → the caller atomically
//!     claims the right to issue (flips phase to **Issuing**) and fires one
//!     coalesced `turn/start` (its observation + anything already queued).
//!   * **Issuing / TurnRunning** → enqueue; the single in-cycle issuer (or
//!     the [`NotificationStream`] consumer task on the next `turn/completed`)
//!     flushes the queue as one coalesced `turn/start`.
//!
//! This enqueue+flush design is **required, not optional**: verified
//! against the real codex binary (PR3b probe), a `turn/start` issued while
//! a turn is active returns an OK ack but its work is **silently dropped**
//! (no `turn/started`/`turn/completed` ever fires for it). `turn/steer` is
//! avoided because its `expectedTurnId` races the live stream.
//!
//! ## B1 — single-winner turn issuance (flush-vs-push race)
//!
//! Two paths can issue a `turn/start`: a dispatcher `push_observation` and
//! the consumer's `flush_push_queue` (on `turn/completed`). If both saw
//! "between turns" and each issued, codex would silently drop one turn's
//! work. The fix is the **Issuing** phase: the decision to issue and the
//! claim of the right to issue are atomic under ONE status-lock
//! acquisition, so exactly one caller wins per cycle; the loser (and any
//! concurrent push) only enqueues. The winner drains the whole queue (plus
//! its own observation, for the push case) into a single `turn/start`. No
//! observation is lost: anything enqueued before the winner drains rides
//! that turn; anything after waits for the next cycle.
//!
//! ## DECISION A — create-wave blocking sequence
//!
//! In the create-wave path (under the flag) the kernel awaits, **before
//! returning the 201 and spawning the `--remote` TUI**:
//!
//!   boot `app-server` → poll socket ready → [`CodexAppServer::connect`] →
//!   [`initialize`](CodexAppServer::initialize) →
//!   when `goal.trim()` is non-empty, [`thread_start`](CodexAppServer::thread_start) →
//!   [`turn_start`](CodexAppServer::turn_start)`([text(goal.trim())])` →
//!   **await the initial `turn/started` or `turn/completed` notification**.
//!   Empty goals skip `thread/start` and park the handle in
//!   [`SpecPushPhase::PendingThreadStart`] until the remote TUI fresh-starts
//!   the thread.
//!
//! For non-empty goals, awaiting the first lifecycle notification
//! guarantees a *rollout exists on disk* (the spike's hard constraint) so
//! the `--remote` TUI's `thread/resume` can rejoin the same thread.
//! `turn/started` is the normal signal; `turn/completed` is also accepted
//! if it arrives first. Empty-goal boots intentionally create no rollout;
//! they rely on the remote TUI to create the thread, while the parked kernel
//! handle buffers pushes until that first lifecycle notification supplies
//! the thread id.
//! There is no per-notification lifecycle budget: EOF/reader exit,
//! JSON-RPC errors, and child exit are the deterministic failure signals.
//! The generous overall boot backstop only catches an alive child that
//! silently wedges during layer-3 init/boot.
//!
//! ## Supervision (2a + B1 process-group fix)
//!
//! The **kernel** spawns and owns the child via
//! [`tokio::process::Command`] — required because the kernel must talk to
//! the app-server (the steps above) *before* the PTY daemon (and therefore
//! the `--remote` TUI) exists. The child lives inside the
//! [`SpecPushHandle`] held by the registry.
//!
//! ### Why `kill_on_drop` alone is NOT enough (B1, empirically proven)
//!
//! Against real codex-cli 0.133.0 the spawned PID is a **`node` launcher**
//! (`codex.js`) that forks a **native `codex app-server` child** which
//! holds the listening socket FDs. `kill_on_drop(true)` SIGKILLs only the
//! node launcher; the native child **survives**, is reparented
//! (PPID→1, under `systemd --user`), and keeps serving the socket. So a
//! pid-only kill (or `kill_on_drop`) leaks the real server on the *normal*
//! teardown path.
//!
//! The load-bearing fix is to spawn the launcher in its **own process
//! group** ([`Command::process_group`]`(0)` → the launcher becomes a group
//! leader so `pgid == launcher pid`, and the native child it forks
//! inherits that pgid). Teardown then signals the **whole group** with
//! `kill(-pgid, …)` (SIGTERM, short grace, SIGKILL), which reaps both the
//! launcher and the native child. We keep `kill_on_drop(true)` as a
//! belt-and-suspenders for the launcher; the group kill is what actually
//! reaps the server.
//!
//! [`SpecPushHandle::drop`] does a synchronous best-effort
//! `kill(-pgid, SIGTERM)` (a bare libc call, fine in `Drop`) so a dropped
//! handle on *any* error path never leaks the native child. The consumer
//! task is aborted on drop too (no orphan task after the connection ends).
//! The pgid is also persisted on the spec-card payload so a kernel
//! hard-crash can reap the orphaned group on the next boot.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::codex_appserver::{
    ClientInfo, CodexAppServer, InputItem, Notification, NotificationStream,
};
use crate::error::{CalmError, Result};
use crate::spec_push::*;

/// How long to poll for the app-server's listen socket to appear + accept
/// a connection before giving up. The server creates the socket after
/// binding; we reuse the same `UnixStream::connect` poll cadence the PTY
/// daemon spawn uses (`routes::terminal::spawn_terminal_with_parts` —
/// 75 × 40 ms). 20 s is generous for a local `app-server` boot (a model
/// turn is NOT required for the socket to come up) while still bounding a
/// binary that never binds (e.g. missing auth → exit during boot).
const SOCKET_READY_POLL: Duration = Duration::from_millis(150);
/// Total wall-clock budget for the socket-ready poll.
const SOCKET_READY_BUDGET: Duration = Duration::from_secs(20);

/// S1 — layer-3 init/boot wedge backstop for the whole post-spawn boot
/// sequence: socket connect + WebSocket upgrade/handshake, initialize,
/// thread start/resume, turn start, and the initial lifecycle wait. The
/// primary readiness/failure signals remain event-driven (`turn/started`,
/// `turn/completed`, reader EOF / JSON-RPC error, and child exit); this
/// generous budget only catches the pathological "process stayed alive but
/// boot silently stopped making progress" case so rollback can reap the
/// app-server process group. PR3 will refine this into interrupt /
/// daemon-restart handling. Healthy slow starts are not expected to trip
/// this because codex emits `turn/started` inline as soon as the turn is
/// accepted.
const OVERALL_BOOT_BUDGET: Duration = Duration::from_secs(45);

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
/// spawn (and persisted alongside `appserver_pgid` on the spec card
/// payload).
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

/// Boot a `codex app-server` for a spec card, optionally drive turn #1, and
/// return the live handle — DECISION A's blocking sequence.
///
/// Steps (each `?`-propagates a [`CalmError::CodexAppServer`] /
/// [`CalmError::Internal`] on failure so the create-wave route can map it
/// to a 5xx, same contract as `seed_and_spawn_spec_daemon`):
///
///   1. spawn `codex app-server --listen unix://<sock>` with the SAME env
///      the codex PTY daemon gets (`env_map` — `CODEX_HOME`, proxy, MCP
///      vars; built by [`crate::spec_card::build_codex_env_map`] +
///      post-commit augmentation in the route),
///   2. poll the socket until it accepts a connection,
///   3. [`connect`](CodexAppServer::connect) +
///      [`initialize`](CodexAppServer::initialize) +
///      [`thread_start`](CodexAppServer::thread_start),
///   4. when the trimmed goal is non-empty,
///      [`turn_start`](CodexAppServer::turn_start) with that goal text,
///   5. for a non-empty goal, **await `turn/started` or `turn/completed`**
///      on the notification stream (rollout now on disk),
///   6. spawn the status-tracking consumer task over the rest of the
///      stream and return everything as [`SpecPushHandle`].
///
/// Empty goals still run through initialize + `thread/start`, then park an
/// idle handle without issuing `turn/start`.
///
/// `codex_bin` is the resolved `codex` CLI path (`CodexClient::codex_bin`).
/// `env_map` is the `serde_json` object map of env vars (string values
/// only are applied — non-string values are ignored, matching
/// `spawn_terminal_with_parts`).
pub async fn spawn_spec_appserver(
    codex_bin: &str,
    env_map: &Value,
    goal_text: &str,
    sock: &Path,
) -> Result<SpecPushHandle> {
    spawn_spec_appserver_with_watchdog_config(
        codex_bin,
        env_map,
        goal_text,
        sock,
        TurnWatchdogConfig::default(),
    )
    .await
}

/// Test/fixture variant of [`spawn_spec_appserver`] that lets callers shorten
/// runtime watchdog budgets without changing production constants.
pub async fn spawn_spec_appserver_with_watchdog_config(
    codex_bin: &str,
    env_map: &Value,
    goal_text: &str,
    sock: &Path,
    watchdog: TurnWatchdogConfig,
) -> Result<SpecPushHandle> {
    spawn_spec_appserver_with_watchdog_config_and_recovery(
        codex_bin, env_map, goal_text, sock,
        // Test-only: production callers go through the `_recovery` variant which threads the spec prompt.
        None, watchdog, None,
    )
    .await
}

/// Production variant of [`spawn_spec_appserver_with_watchdog_config`] that
/// wires the notification consumer to the runtime process-level recovery
/// supervisor for this wave.
pub async fn spawn_spec_appserver_with_watchdog_config_and_recovery(
    codex_bin: &str,
    env_map: &Value,
    goal_text: &str,
    sock: &Path,
    developer_instructions: Option<&str>,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
) -> Result<SpecPushHandle> {
    spawn_spec_appserver_with_watchdog_config_and_recovery_for_wave(
        codex_bin,
        env_map,
        goal_text,
        sock,
        developer_instructions,
        watchdog,
        recovery_signal,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn spawn_spec_appserver_with_watchdog_config_and_recovery_for_wave(
    codex_bin: &str,
    env_map: &Value,
    goal_text: &str,
    sock: &Path,
    developer_instructions: Option<&str>,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
    wave_id: Option<&crate::ids::WaveId>,
) -> Result<SpecPushHandle> {
    // The server `chmod 0700`s the socket's PARENT dir, so the parent must
    // be a user-owned dir (not bare sticky /tmp). The caller creates the
    // per-card subdir under the user-owned data dir; we only ensure it
    // exists + clear a stale socket file so `bind()` doesn't EADDRINUSE.
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CalmError::Internal(format!(
                "mkdir appserver sock parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    if sock.exists() {
        let _ = std::fs::remove_file(sock);
    }
    let listen = format!("unix://{}", sock.display());

    // 1. Spawn the app-server child with the codex daemon's env. We start
    //    from a clean env-application loop (same shape as
    //    `spawn_terminal_with_parts`): apply each string-valued entry. This
    //    carries CODEX_HOME + proxy (so model turns work) + MCP vars.
    let mut cmd = Command::new(codex_bin);
    cmd.arg("app-server")
        .arg("--listen")
        .arg(&listen)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        // B1: spawn in its OWN process group so `pgid == launcher pid`.
        // The `node` launcher forks the native `codex app-server` child,
        // which inherits this pgid; teardown `kill(-pgid, …)` then reaps
        // BOTH (the native child is what holds the socket and survives a
        // launcher-only kill). `kill_on_drop` stays as a belt-and-
        // suspenders reaper of the launcher.
        .process_group(0)
        .kill_on_drop(true);
    if let Some(map) = env_map.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                cmd.env(k, val);
            }
        }
    }
    let child = cmd
        .spawn()
        .map_err(|e| CalmError::CodexAppServer(format!("spawn codex app-server: {e}")))?;
    // `process_group(0)` makes the child a group leader, so its pgid equals
    // its pid. `child.id()` is `Some` until the child is reaped (we never
    // `wait()` it here — it lives in the handle), so the unwrap path only
    // fails if the child already exited, which `poll_connect` catches next.
    let pgid: i32 = match child.id() {
        Some(pid) => i32::try_from(pid).map_err(|_| {
            CalmError::CodexAppServer(format!("app-server pid {pid} out of i32 range"))
        })?,
        None => {
            // Child already exited before we read its pid — surface a clear
            // spawn error (kill_on_drop reaps whatever is left on return).
            return Err(CalmError::CodexAppServer(
                "codex app-server exited immediately after spawn (no pid)".to_string(),
            ));
        }
    };
    // #318 INV-5 (R3-B1) — capture the launcher's `(starttime, boot_id)`
    // IMMEDIATELY after spawn (before the child has any chance to exit).
    // Together with the pgid this is the boot-recovery identity token:
    //   * mid-boot pid recycle → recycled process has a later starttime
    //     → `verify_owned_pid` rejects.
    //   * post-reboot pid recycle → `boot_id` differs → `verify_owned_pid`
    //     rejects regardless of starttime.
    // `None` for either (test fixtures on a non-Linux target, transient
    // ENOENT race) is persisted as missing and the recovery path
    // conservatively skips the kill — same posture as a mismatch.
    let start_time = read_proc_start_time(pgid);
    let boot_id = read_boot_id();
    tracing::info!(
        pid = pgid, pgid, start_time, ?boot_id, sock = %sock.display(),
        "spec push: spawned codex app-server (own process group)",
    );

    // From here on, any early return drops `child` (→ kill_on_drop on the
    // launcher) but we must also reap the native child's GROUP. Wrap the
    // remaining fallible sequence so every `?` triggers a group SIGTERM +
    // socket-dir cleanup before propagating (S2 rollback). A `Drop`-based
    // guard keeps this DRY across the half-dozen `?` sites below.
    let mut rollback = SpawnRollback::new(pgid, sock);
    // Layer-3 init/boot wedge backstop: the whole post-spawn sequence is
    // normally decided by concrete events (socket readiness, WS/JSON-RPC
    // responses, `turn/started`/`turn/completed`, EOF, child exit). This
    // generous outer timeout exists only for the "child is alive, but the
    // boot handshake/lifecycle silently stops making progress" class; if
    // it fires, the armed rollback below reaps the process group and clears
    // the socket dir. PR3 will refine this into interrupt / daemon-restart.
    let handle = match tokio::time::timeout(
        OVERALL_BOOT_BUDGET,
        build_handle_after_spawn(
            child,
            pgid,
            start_time,
            boot_id,
            wave_id.cloned(),
            goal_text,
            sock,
            developer_instructions,
            watchdog,
            recovery_signal,
        ),
    )
    .await
    {
        Ok(res) => res?,
        Err(_) => {
            return Err(CalmError::CodexAppServer(format!(
                "codex app-server boot did not complete within {}s overall \
                 (layer-3 init/boot wedge backstop fired)",
                OVERALL_BOOT_BUDGET.as_secs()
            )));
        }
    };
    rollback.disarm();
    Ok(handle)
}

/// #313 problem #1 — boot-time variant of [`spawn_spec_appserver`] for
/// **takeover after a kernel restart**: respawn a fresh `codex app-server`
/// for a spec card whose `codex_thread_id` is already persisted, then
/// `initialize` + `thread/resume(<thread_id>)` against it (no `turn/start`).
///
/// Same shape as [`spawn_spec_appserver`]: spawn the child in its OWN
/// process group ([`Command::process_group`]`(0)` → `pgid == launcher pid`),
/// arm a [`SpawnRollback`] guard so any `?`/backstop timeout reaps the
/// launcher's whole group. The only differences from the create-wave happy
/// path are:
///   * the post-spawn sequence calls
///     [`build_handle_after_spawn_resume`] instead of
///     [`build_handle_after_spawn`] — `thread/resume(thread_id)` in place of
///     `thread/start` + `turn/start` + initial lifecycle wait, and
///   * a `-32600 "no rollout found"` from `thread/resume` (the wave never
///     ran turn #1 last boot — the "inert wave" case) surfaces here as a
///     [`CalmError::CodexAppServer`]; the caller treats it as non-fatal
///     and leaves the wave inert (matches the create-wave error posture).
///
/// Resume only — does NOT issue `turn/start`. The dispatcher's catch-up
/// replay will push any `id > push_watermark` events through the normal
/// push path right after this returns; the first such push issues the
/// first new turn (just like in steady state). If there are no catch-up
/// events the wave sits idle until the next live event lands.
pub async fn resume_spec_appserver(
    codex_bin: &str,
    env_map: &Value,
    thread_id: &str,
    sock: &Path,
) -> Result<SpecPushHandle> {
    resume_spec_appserver_with_watchdog_config(
        codex_bin,
        env_map,
        thread_id,
        sock,
        TurnWatchdogConfig::default(),
    )
    .await
}

/// Test/fixture variant of [`resume_spec_appserver`] that lets callers
/// shorten runtime watchdog budgets without changing production constants.
pub async fn resume_spec_appserver_with_watchdog_config(
    codex_bin: &str,
    env_map: &Value,
    thread_id: &str,
    sock: &Path,
    watchdog: TurnWatchdogConfig,
) -> Result<SpecPushHandle> {
    resume_spec_appserver_with_watchdog_config_and_recovery(
        codex_bin, env_map, thread_id, sock, watchdog, None,
    )
    .await
}

/// Production variant of [`resume_spec_appserver_with_watchdog_config`] that
/// wires the notification consumer to the runtime process-level recovery
/// supervisor for this wave.
pub async fn resume_spec_appserver_with_watchdog_config_and_recovery(
    codex_bin: &str,
    env_map: &Value,
    thread_id: &str,
    sock: &Path,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
) -> Result<SpecPushHandle> {
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CalmError::Internal(format!(
                "mkdir appserver sock parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    if sock.exists() {
        let _ = std::fs::remove_file(sock);
    }
    let listen = format!("unix://{}", sock.display());

    let mut cmd = Command::new(codex_bin);
    cmd.arg("app-server")
        .arg("--listen")
        .arg(&listen)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0)
        .kill_on_drop(true);
    if let Some(map) = env_map.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                cmd.env(k, val);
            }
        }
    }
    let child = cmd
        .spawn()
        .map_err(|e| CalmError::CodexAppServer(format!("spawn codex app-server (resume): {e}")))?;
    let pgid: i32 = match child.id() {
        Some(pid) => i32::try_from(pid).map_err(|_| {
            CalmError::CodexAppServer(format!("app-server pid {pid} out of i32 range"))
        })?,
        None => {
            return Err(CalmError::CodexAppServer(
                "codex app-server exited immediately after spawn (no pid) on resume".to_string(),
            ));
        }
    };
    // #318 INV-5 (R3-B1) — same identity-stamp capture as the spawn path
    // (see `spawn_spec_appserver`). The fresh respawn after takeover gets
    // its own (start_time, boot_id); we persist them into the spec card
    // payload below via `spec_card_set_appserver_after_takeover` so the
    // NEXT boot-recovery cycle has a current identity token.
    let start_time = read_proc_start_time(pgid);
    let boot_id = read_boot_id();
    tracing::info!(
        pid = pgid, pgid, start_time, ?boot_id, sock = %sock.display(), %thread_id,
        "spec push (resume): spawned codex app-server (own process group)",
    );

    let mut rollback = SpawnRollback::new(pgid, sock);
    // Same layer-3 init/boot wedge backstop as the create path: resume is
    // expected to complete via socket/WS/JSON-RPC progress or fail via
    // child exit / transport errors. The timeout is only for an alive child
    // that wedges silently during boot; rollback remains armed until the
    // handle is fully parked.
    let handle = match tokio::time::timeout(
        OVERALL_BOOT_BUDGET,
        build_handle_after_spawn_resume(
            child,
            pgid,
            start_time,
            boot_id,
            thread_id,
            sock,
            watchdog,
            recovery_signal,
        ),
    )
    .await
    {
        Ok(res) => res?,
        Err(_) => {
            return Err(CalmError::CodexAppServer(format!(
                "codex app-server resume did not complete within {}s overall \
                 (layer-3 init/boot wedge backstop fired)",
                OVERALL_BOOT_BUDGET.as_secs()
            )));
        }
    };
    rollback.disarm();
    Ok(handle)
}

// #313 PR4-round2 (B2): `adopt_live_appserver` removed.
//
// Earlier rounds tried to *adopt* a persisted-still-alive app-server (the
// rare case where the kernel `SIGKILL`ed but the native `codex
// app-server` child was reparented under `systemd --user` and stayed
// bound to the socket). Safe adoption required either probing the
// server's live turn-phase (no `thread/status` query exists on the
// codex JSON-RPC) or seeding the handle pessimistically as
// `TurnRunning` + a fallback timer — both add complexity for a marginal
// optimization. Worse, the round-1 implementation left the adopted
// handle's phase as the default `Idle`; boot catch-up then fired a
// `turn/start` against a possibly-mid-turn server and codex silently
// dropped it (the very bug #293 PR3b's queue exists to prevent).
//
// The cleanest correctness fix is to ALWAYS respawn on takeover:
// kill the persisted process group (best-effort SIGTERM + SIGKILL),
// clean the stale socket, and call [`resume_spec_appserver`]. The
// `thread/resume(<thread_id>)` rejoins the rollout on disk; the fresh
// server's notification stream reconciles the consumer-tracked phase
// (`Idle` until a `turn/started`/`turn/completed` arrives), so a push
// landing immediately after takeover either starts a new turn (idle —
// safe) or rides the next flush (mid-turn — buffered correctly).
// The downside (one extra spawn per restart in the rare adopt-eligible
// case) is well worth the simpler, race-free code path.

/// Best-effort rollback guard for the post-spawn fallible sequence
/// (S2/B1). While *armed*, dropping it sends `SIGTERM` to the child's
/// process group and clears the per-card socket dir, so any `?` early
/// return after the child is spawned can't leak the native `codex
/// app-server`. [`disarm`](Self::disarm) is called once the
/// [`SpecPushHandle`] (which then owns teardown) is built successfully.
struct SpawnRollback {
    pgid: i32,
    sock: PathBuf,
    armed: bool,
}

impl SpawnRollback {
    fn new(pgid: i32, sock: &Path) -> Self {
        Self {
            pgid,
            sock: sock.to_path_buf(),
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SpawnRollback {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        tracing::warn!(
            pgid = self.pgid,
            sock = %self.sock.display(),
            "spec push: spawn sequence failed after child spawn — killing process group + cleaning socket dir"
        );
        signal_process_group(self.pgid, libc::SIGTERM);
        cleanup_sock_dir(&self.sock);
    }
}

/// Remove the listen socket and its now-empty per-card dir
/// (`<data_dir>/appserver/<card_id>/`). Best-effort: a missing socket /
/// non-empty dir is fine. Mirrors the PTY `remove_file(sock)` cleanup in
/// [`crate::terminal_sweeper::reap_terminal_artifacts`].
pub fn cleanup_sock_dir(sock: &Path) {
    let _ = std::fs::remove_file(sock);
    if let Some(dir) = sock.parent() {
        // `remove_dir` only succeeds when empty — exactly what we want
        // (don't nuke a dir that unexpectedly holds other files).
        let _ = std::fs::remove_dir(dir);
    }
}

/// #313 problem #1 round-3 (B1) + #335 PR2 — verify that the per-card
/// socket at `sock` has a live codex app-server listener BEFORE the caller
/// signals the persisted `pgid`.
///
/// **Why this exists.** After a host reboot the persisted `appserver_pgid`
/// almost certainly belongs to an unrelated process (PIDs/PGIDs are
/// recycled), so a `kill(-pgid, SIGTERM/SIGKILL)` could nuke arbitrary
/// user processes. The per-card socket path is UUID-scoped
/// (`<data_dir>/appserver/<card_id>/sock`), but connect alone is not enough:
/// a different listener on a stale path could otherwise authorize a kill.
/// We require both WebSocket connect and a JSON-RPC `initialize` round-trip.
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

/// The fallible post-spawn sequence (connect → initialize → thread/start →
/// optional turn/start → optional initial lifecycle wait → spawn consumer).
/// Split out so the [`SpawnRollback`] guard in [`spawn_spec_appserver`]
/// wraps every `?`.
#[allow(clippy::too_many_arguments)]
async fn build_handle_after_spawn(
    mut child: Child,
    pgid: i32,
    start_time: Option<u64>,
    boot_id: Option<String>,
    wave_id: Option<crate::ids::WaveId>,
    goal_text: &str,
    sock: &Path,
    developer_instructions: Option<&str>,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
) -> Result<SpecPushHandle> {
    // 2. Poll the socket for readiness, bailing early if the child dies
    //    during boot (the common no-auth / bad-env failure mode).
    let connected = poll_connect(&mut child, sock).await?;
    let (client, notifs) = connected;

    build_handle_after_connect(
        child,
        pgid,
        start_time,
        boot_id,
        client,
        notifs,
        wave_id,
        goal_text,
        sock,
        developer_instructions,
        watchdog,
        recovery_signal,
    )
    .await
}

/// The fallible post-connect boot sequence. Kept separate from
/// [`build_handle_after_spawn`] so tests can drive a fake JSON-RPC peer and
/// assert the exact `turn/start` behavior without spawning a real codex
/// binary.
#[allow(clippy::too_many_arguments)]
async fn build_handle_after_connect(
    mut child: Child,
    pgid: i32,
    start_time: Option<u64>,
    boot_id: Option<String>,
    client: CodexAppServer,
    mut notifs: NotificationStream,
    wave_id: Option<crate::ids::WaveId>,
    goal_text: &str,
    sock: &Path,
    developer_instructions: Option<&str>,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
) -> Result<SpecPushHandle> {
    let client = Arc::new(client);

    // 3. initialize + optional thread/start + optional turn/start ack. The caller wraps this
    //    whole build future in `OVERALL_BOOT_BUDGET`; individual progress is
    //    still determined by JSON-RPC success/error and child/stream liveness.
    client
        .initialize(ClientInfo {
            name: "neige-calm-spec-push".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await?;
    let goal_text = goal_text.trim();
    if goal_text.is_empty() {
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            phase: SpecPushPhase::PendingThreadStart,
            last_thread_id: None,
            last_turn_id: None,
        }));
        tracing::info!(
            wave_id = wave_id.as_ref().map(|id| id.as_str()),
            "spec push: empty goal — initialized app-server without thread/start; waiting for TUI fresh-start"
        );
        return Ok(park_handle(
            child,
            pgid,
            start_time,
            boot_id,
            client,
            None,
            sock,
            notifs,
            status,
            watchdog,
            recovery_signal,
        ));
    }
    let thread = client.thread_start(developer_instructions).await?;
    let thread_id = thread
        .thread_id()
        .ok_or_else(|| {
            CalmError::CodexAppServer("thread/start result missing thread.id".to_string())
        })?
        .to_string();
    tracing::info!(thread_id = %thread_id, "spec push: thread started");

    let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
        last_thread_id: Some(thread_id.clone()),
        ..Default::default()
    }));

    let turn = client
        .turn_start(&thread_id, vec![InputItem::text(goal_text)])
        .await?;
    tracing::info!(thread_id = %thread_id, turn_id = ?turn.turn_id(), "spec push: turn #1 started (ack)");

    // 5. Await `turn/started` (or `turn/completed`, which also proves the
    //    rollout exists) before the `--remote` TUI tries to resume.
    //    This waits on deterministic lifecycle/EOF/child-exit signals, not
    //    a "started within N seconds" budget. The caller's overall boot
    //    backstop only handles a fully silent boot wedge.
    //    `await_initial_turn_lifecycle` reads notifications off the SAME
    //    `notifs` receiver the consumer task takes over below; it records
    //    the matched lifecycle signal into `status`.
    //    Nothing is buffered or replayed — we simply hand the still-open
    //    receiver to the consumer task afterwards, so no notification is
    //    lost (anything not yet consumed is still queued on the mpsc).
    await_initial_turn_lifecycle(&mut child, &mut notifs, &thread_id, &status).await?;

    // 6. Spawn the consumer task and park the live handle (see
    //    [`park_handle`] for the shared tail).
    Ok(park_handle(
        child,
        pgid,
        start_time,
        boot_id,
        client,
        Some(thread_id),
        sock,
        notifs,
        status,
        watchdog,
        recovery_signal,
    ))
}

/// #313 problem #1 — boot-time takeover variant of [`build_handle_after_spawn`].
///
/// Same shape (poll-connect → initialize → spawn consumer → park-handle)
/// but **swaps `thread/start` + `turn/start` + initial lifecycle wait** for a
/// single `thread/resume(thread_id)`. No turn is issued on resume: the wave
/// may be mid-turn from the prior boot, or simply between turns; either way
/// the kernel's role here is to **re-attach** so the dispatcher can push
/// catch-up events onto the live thread again, not to drive a fresh turn.
///
/// A `thread/resume` failure (`-32600 "no rollout found"` on a thread that
/// never ran turn #1 in the prior boot, or any transport error) propagates
/// as [`CalmError::CodexAppServer`]; the caller treats it as non-fatal and
/// leaves the wave inert (matches the create-wave error posture).
#[allow(clippy::too_many_arguments)]
async fn build_handle_after_spawn_resume(
    mut child: Child,
    pgid: i32,
    start_time: Option<u64>,
    boot_id: Option<String>,
    thread_id: &str,
    sock: &Path,
    watchdog: TurnWatchdogConfig,
    recovery_signal: Option<SpecRecoverySignal>,
) -> Result<SpecPushHandle> {
    // 2. Poll the socket for readiness (same as the spawn path).
    let (client, notifs) = poll_connect(&mut child, sock).await?;
    let client = Arc::new(client);

    // 3. initialize + thread/resume. No `turn/start`, no initial lifecycle wait
    //    — resume rejoins the persisted thread by id; if a turn is mid-flight
    //    we'll observe its `turn/completed` on the notification stream and
    //    reconcile the consumer-tracked phase.
    let resumed = {
        client
            .initialize(ClientInfo {
                name: "neige-calm-spec-push".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            })
            .await?;
        // Codex persists developer_instructions in the rollout; resume must NOT re-supply or codex rejects with "override ignored while running".
        client.thread_resume(thread_id).await?
    };
    // Defensive: the server should echo back the same id on a successful
    // resume; if it doesn't, log and prefer the persisted id (we keyed
    // everything off it).
    if let Some(echoed) = resumed.thread_id()
        && echoed != thread_id
    {
        tracing::warn!(
            persisted = %thread_id,
            echoed = %echoed,
            "spec push (resume): server echoed a different thread id than persisted; using persisted",
        );
    }
    tracing::info!(thread_id = %thread_id, "spec push (resume): thread resumed");

    // #318 INV-4 (R2-B2) — seed the consumer-tracked phase as `Resumed`,
    // NOT `Idle`. `thread/resume` does not prove the server is between
    // turns: a prior-boot turn could still be running on the resumed
    // thread. `decide(Resumed) == Enqueue` so a catch-up push buffers
    // instead of firing a (silently-dropped) `turn/start` mid-turn; the
    // server's first `turn/started` / `turn/completed` then reconciles
    // the phase (via `record()`) before any issue actually runs.
    let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
        phase: SpecPushPhase::Resumed,
        last_thread_id: Some(thread_id.to_string()),
        last_turn_id: None,
    }));

    let mut handle = park_handle(
        child,
        pgid,
        start_time,
        boot_id,
        client.clone(),
        Some(thread_id.to_string()),
        sock,
        notifs,
        status.clone(),
        watchdog,
        recovery_signal,
    );

    // #318 INV-4 (codex P1) — spawn the idle-resume reconcile timer. See
    // [`RESUMED_RECONCILE_BUDGET`] for the full rationale. The timer
    // CAS-promotes `Resumed → TurnCompleted` only if a real lifecycle
    // notification has NOT already moved the phase (via `record()`); if it
    // has, the timer no-ops. After a successful promotion it invokes
    // [`flush_push_queue`] once so any catch-up observations the
    // dispatcher enqueued under `decide(Resumed) == Enqueue` get issued
    // as a coalesced `turn/start` instead of being stranded forever on a
    // truly-idle resumed thread (the case where neither `turn/started`
    // nor `turn/completed` will EVER arrive).
    let reconciler = tokio::spawn(resume_reconcile_task(
        RESUMED_RECONCILE_BUDGET,
        thread_id.to_string(),
        status,
        SpecPusherSource::Legacy { client },
        handle.queue.clone(),
        handle.watermark_sink.clone(),
        handle.queue_persist.clone(),
    ));
    handle.resume_reconciler = Some(reconciler);

    Ok(handle)
}

/// Poll `UnixStream::connect(sock)` until it succeeds (the app-server has
/// bound), bailing out as a skip-able error if the child exits during
/// boot. Mirrors the readiness loop in `spawn_terminal_with_parts`.
///
/// The socket budgets are wedge backstops for "process stayed alive but
/// never bound"; readiness itself is still connect-ok, and failure is still
/// child-exit or exhausted wedge backstop.
async fn poll_connect(
    child: &mut Child,
    sock: &Path,
) -> Result<(CodexAppServer, NotificationStream)> {
    let deadline = tokio::time::Instant::now() + SOCKET_READY_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if sock.exists()
            && let Ok(pair) = CodexAppServer::connect(sock).await
        {
            return Ok(pair);
        }
        // Child died during boot — surface a clear error instead of
        // spinning until the deadline.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(CalmError::CodexAppServer(format!(
                "codex app-server exited during boot (status {status})"
            )));
        }
        tokio::time::sleep(SOCKET_READY_POLL).await;
    }
    Err(CalmError::CodexAppServer(format!(
        "codex app-server did not accept a connection within {}s",
        SOCKET_READY_BUDGET.as_secs()
    )))
}

/// Drain the stream until an initial lifecycle signal for `thread_id`
/// arrives. `turn/started` is the normal proof that a rollout exists;
/// `turn/completed` is accepted too because it is an even stronger proof
/// that the accepted turn ran to a lifecycle boundary.
///
/// This is event-driven: success is the matching notification, failure is
/// deterministic stream closure/reader exit or child exit. There is no
/// per-notification "started within N seconds" failure path here; the
/// caller's overall boot backstop only handles a fully silent boot wedge.
async fn await_initial_turn_lifecycle(
    child: &mut Child,
    notifs: &mut NotificationStream,
    thread_id: &str,
    status: &SharedStatus,
) -> Result<()> {
    tokio::select! {
        notification = notifs.await_notification(|n| {
            matches!(
                n,
                Notification::TurnStarted { thread_id: t, .. }
                    | Notification::TurnCompleted { thread_id: t, .. }
                    if t == thread_id
            )
        }) => {
            let notification = notification?;
            record(status, &notification).await;
            match &notification {
                Notification::TurnStarted { turn, .. } => {
                    let turn_id = turn.get("id").and_then(Value::as_str).map(str::to_string);
                    tracing::debug!(thread_id, ?turn_id, "spec push: observed initial turn/started");
                }
                Notification::TurnCompleted { turn, .. } => {
                    let turn_id = turn.get("id").and_then(Value::as_str).map(str::to_string);
                    tracing::debug!(thread_id, ?turn_id, "spec push: observed initial turn/completed");
                }
                _ => {}
            }
            Ok(())
        }
        child_status = child.wait() => {
            match child_status {
                Ok(exit) => Err(CalmError::CodexAppServer(format!(
                    "codex app-server exited before initial turn lifecycle notification (status {exit})"
                ))),
                Err(e) => Err(CalmError::CodexAppServer(format!(
                    "wait for codex app-server while awaiting initial turn lifecycle notification: {e}"
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::ids::WaveId;
    use tokio::time::Instant as TokioInstant;

    /// Build a real [`SpecPushHandle`] over an in-process WS pair + a
    /// long-running dummy child (so `kill_on_drop` has something to reap),
    /// for registry-mechanics tests without a `codex` binary. Returns the
    /// handle plus the server WS end the handle's client is talking to —
    /// the caller keeps the server end alive for the handle's lifetime.
    async fn fake_handle() -> (
        SpecPushHandle,
        tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    ) {
        let (client, notifs, server) = CodexAppServer::connect_pair_for_test().await;
        // A real child so `kill_on_drop(true)` reaps something on drop.
        // `sleep` exists on every unix CI image; the test never waits on it.
        // Spawn it in its OWN process group (as production does) so the
        // handle's pgid is meaningful and the group-kill Drop path is
        // exercised.
        let child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn dummy child");
        let pgid = i32::try_from(child.id().expect("dummy child pid")).expect("pid fits i32");
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus::default()));
        let queue: PushQueue = Arc::new(Mutex::new(VecDeque::new()));
        let watermark_sink: WatermarkSinkSlot = Arc::new(Mutex::new(None));
        let queue_persist: QueuePersistSlot = Arc::new(Mutex::new(None));
        let thread_id_slot: ThreadIdSlot = Arc::new(Mutex::new(Some("thread-test".to_string())));
        let initial_prompt_ready_sink: InitialPromptReadySinkSlot = Arc::new(Mutex::new(None));
        let client = Arc::new(client);
        let consumer = tokio::spawn(consume_notifications(
            notifs,
            thread_id_slot.clone(),
            status.clone(),
            client.clone(),
            queue.clone(),
            watermark_sink.clone(),
            queue_persist.clone(),
            initial_prompt_ready_sink.clone(),
            TurnWatchdogConfig::default(),
            None,
        ));
        let handle = SpecPushHandle {
            source: SpecPushSource::Legacy {
                child: Box::new(child),
                pgid,
                start_time: read_proc_start_time(pgid),
                boot_id: read_boot_id(),
                client,
                sock: PathBuf::from("/tmp/test/app.sock"),
            },
            thread_id: Some("thread-test".into()),
            thread_id_slot,
            consumer,
            resume_reconciler: None,
            status,
            queue,
            watermark_sink,
            queue_persist,
            initial_prompt_ready_sink,
        };
        (handle, server)
    }

    fn fake_child() -> Child {
        Command::new("sleep")
            .arg("60")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn dummy child")
    }

    #[tokio::test]
    async fn empty_goal_boot_parks_handle_without_turn_start() {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio_tungstenite::tungstenite::Message;

        let (client, notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        let saw_thread_start = Arc::new(AtomicBool::new(false));
        let saw_turn_start = Arc::new(AtomicBool::new(false));
        let saw_thread_start_for_task = Arc::clone(&saw_thread_start);
        let saw_turn_start_for_task = Arc::clone(&saw_turn_start);
        let (sut_returned_tx, mut sut_returned_rx) = tokio::sync::oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            let watchdog = tokio::time::sleep(Duration::from_secs(1));
            tokio::pin!(watchdog);
            loop {
                let frame = tokio::select! {
                    _ = &mut sut_returned_rx => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        break;
                    }
                    _ = &mut watchdog => panic!("SUT did not return after empty-goal initialize"),
                    next = server.next() => match next {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(_)) => continue,
                        None | Some(Err(_)) => break,
                    },
                };
                let req: Value = serde_json::from_str(&frame).expect("json-rpc request");
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                match req.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        server
                            .send(Message::Text(
                                serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "userAgent": "fake-codex-app-server/empty-goal",
                                        "codexHome": "",
                                        "platformFamily": "unix",
                                        "platformOs": "linux"
                                    }
                                })
                                .to_string(),
                            ))
                            .await
                            .expect("send initialize response");
                    }
                    Some("thread/start") => {
                        saw_thread_start_for_task.store(true, Ordering::SeqCst);
                        break;
                    }
                    Some("turn/start") => {
                        saw_turn_start_for_task.store(true, Ordering::SeqCst);
                        break;
                    }
                    other => panic!("unexpected method during empty-goal boot: {other:?}"),
                }
            }
            server
        });

        let child = fake_child();
        let pgid = i32::try_from(child.id().expect("dummy child pid")).expect("pid fits i32");
        let handle = build_handle_after_connect(
            child,
            pgid,
            read_proc_start_time(pgid),
            read_boot_id(),
            client,
            notifs,
            None,
            " \n\t ",
            Path::new("/tmp/test-empty-goal/app.sock"),
            None,
            TurnWatchdogConfig::default(),
            None,
        )
        .await
        .expect("empty goal boot should park a handle");
        sut_returned_tx.send(()).expect("server task still waiting");

        assert_eq!(handle.thread_id, None);
        let status = handle.status().await;
        assert_eq!(status.phase, SpecPushPhase::PendingThreadStart);
        assert_eq!(status.last_thread_id, None);
        assert_eq!(status.last_turn_id, None);

        let _server = server_task.await.expect("server task");
        assert!(
            !saw_thread_start.load(Ordering::SeqCst),
            "empty trimmed goal must not issue thread/start"
        );
        assert!(
            !saw_turn_start.load(Ordering::SeqCst),
            "empty trimmed goal must not issue turn/start"
        );
        drop(handle);
    }

    #[tokio::test]
    async fn initial_turn_lifecycle_accepts_started_without_budget() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        let mut child = fake_child();
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            last_thread_id: Some("thread-test".into()),
            ..Default::default()
        }));

        server
            .send(Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "turn/started",
                    "params": { "threadId": "thread-test", "turn": { "id": "turn-1" } }
                })
                .to_string(),
            ))
            .await
            .expect("send turn/started");

        await_initial_turn_lifecycle(&mut child, &mut notifs, "thread-test", &status)
            .await
            .expect("started lifecycle");
        let g = status.lock().await;
        assert_eq!(g.phase, SpecPushPhase::TurnRunning);
        assert_eq!(g.last_turn_id.as_deref(), Some("turn-1"));
        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn initial_turn_lifecycle_accepts_completed_without_started() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        let mut child = fake_child();
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            last_thread_id: Some("thread-test".into()),
            ..Default::default()
        }));

        server
            .send(Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "turn/completed",
                    "params": { "threadId": "thread-test", "turn": { "id": "turn-1" } }
                })
                .to_string(),
            ))
            .await
            .expect("send turn/completed");

        await_initial_turn_lifecycle(&mut child, &mut notifs, "thread-test", &status)
            .await
            .expect("completed lifecycle");
        let g = status.lock().await;
        assert_eq!(g.phase, SpecPushPhase::TurnCompleted);
        assert_eq!(g.last_turn_id.as_deref(), Some("turn-1"));
        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn consumer_turn_lifecycle_clears_initial_prompt_marker() {
        use crate::card_role_cache::CardRoleCache;
        use crate::db::prelude::*;
        use crate::db::sqlite::SqlxRepo;
        use crate::model::{CardRole, NewCard, NewCove, NewWave};

        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory repo"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "lifecycle-clear".into(),
                color: "#abcdef".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let cache = CardRoleCache::new();
        let mut tx = repo.pool().begin().await.expect("begin tx");
        let spec = crate::db::sqlite::card_create_with_id_tx(
            &mut tx,
            crate::model::new_id(),
            NewCard {
                wave_id: wave.id,
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({
                    "codex_thread_id": "thread-test",
                    "appserver_needs_initial_prompt": true,
                    "push_watermark": 0,
                }),
            },
            CardRole::Spec,
            false,
            &cache,
        )
        .await
        .expect("create spec card");
        tx.commit().await.expect("commit");

        let (client, notifs, _server) = CodexAppServer::connect_pair_for_test().await;
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            last_thread_id: Some("thread-test".into()),
            ..Default::default()
        }));
        let initial_prompt_ready_sink: InitialPromptReadySinkSlot = Arc::new(Mutex::new(None));
        {
            let repo = Arc::clone(&repo);
            let card_id = spec.id.clone();
            *initial_prompt_ready_sink.lock().await = Some(Arc::new(move |_thread_id: String| {
                let repo = Arc::clone(&repo);
                let card_id = card_id.clone();
                Box::pin(async move {
                    repo.spec_card_clear_needs_initial_prompt(card_id.as_str())
                        .await
                        .expect("clear initial-prompt marker");
                })
            }));
        }
        let mut consumer = NotificationConsumer {
            notifs,
            thread_id_slot: Arc::new(Mutex::new(Some("thread-test".to_string()))),
            status: status.clone(),
            source: SpecPusherSource::Legacy {
                client: Arc::new(client),
            },
            queue: Arc::new(Mutex::new(VecDeque::new())),
            watermark_sink: Arc::new(Mutex::new(None)),
            queue_persist: Arc::new(Mutex::new(None)),
            initial_prompt_ready_sink,
            watchdog: TurnWatchdogConfig::default(),
            recovery_signal: None,
            active_turn: None,
            initial_prompt_ready_attempted: false,
        };

        consumer
            .process_notification(Notification::TurnStarted {
                thread_id: "thread-test".to_string(),
                turn: serde_json::json!({ "id": "turn-1" }),
            })
            .await;

        let g = status.lock().await;
        assert_eq!(g.last_turn_id.as_deref(), Some("turn-1"));
        drop(g);
        let got = repo
            .card_get(spec.id.as_str())
            .await
            .unwrap()
            .expect("spec card");
        assert!(
            got.payload.get("appserver_needs_initial_prompt").is_none(),
            "first observed turn lifecycle must clear the bootstrap marker"
        );
    }

    #[tokio::test]
    async fn pending_thread_start_buffers_until_tui_thread_lifecycle_flushes() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (client, notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        let client = Arc::new(client);
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            phase: SpecPushPhase::PendingThreadStart,
            last_thread_id: None,
            last_turn_id: None,
        }));
        let queue: PushQueue = Arc::new(Mutex::new(VecDeque::new()));
        let thread_id_slot: ThreadIdSlot = Arc::new(Mutex::new(None));
        let enqueued: Arc<Mutex<Vec<(i64, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let dequeued: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let watermarks: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let enqueue_rows = Arc::clone(&enqueued);
        let dequeue_rows = Arc::clone(&dequeued);
        let persist = QueuePersist {
            enqueue: Arc::new(move |envelope_id, text| {
                let rows = Arc::clone(&enqueue_rows);
                Box::pin(async move {
                    let mut rows = rows.lock().await;
                    rows.push((envelope_id, text));
                    Some(i64::try_from(rows.len()).expect("row id fits i64"))
                })
            }),
            dequeue: Arc::new(move |ids| {
                let rows = Arc::clone(&dequeue_rows);
                Box::pin(async move {
                    rows.lock().await.extend(ids);
                })
            }),
            list: Arc::new(|| Box::pin(async { Vec::new() })),
        };
        let queue_persist: QueuePersistSlot = Arc::new(Mutex::new(Some(Arc::new(persist))));
        let watermark_sink: WatermarkSinkSlot = Arc::new(Mutex::new(Some({
            let watermarks = Arc::clone(&watermarks);
            Arc::new(move |watermark| {
                let watermarks = Arc::clone(&watermarks);
                Box::pin(async move {
                    watermarks.lock().await.push(watermark);
                }) as futures_util::future::BoxFuture<'static, ()>
            })
        })));
        let ready_threads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let initial_prompt_ready_sink: InitialPromptReadySinkSlot = Arc::new(Mutex::new(Some({
            let ready_threads = Arc::clone(&ready_threads);
            Arc::new(move |thread_id| {
                let ready_threads = Arc::clone(&ready_threads);
                Box::pin(async move {
                    ready_threads.lock().await.push(thread_id);
                }) as futures_util::future::BoxFuture<'static, ()>
            })
        })));

        let pusher = SpecPusher {
            source: SpecPusherSource::Legacy {
                client: Arc::clone(&client),
            },
            thread_id_slot: Arc::clone(&thread_id_slot),
            status: Arc::clone(&status),
            queue: Arc::clone(&queue),
            watermark_sink: Arc::clone(&watermark_sink),
            queue_persist: Arc::clone(&queue_persist),
        };

        let outcome = pusher
            .push_observation(42, "buffer me")
            .await
            .expect("pending push should enqueue");
        assert_eq!(outcome, PushOutcome::Enqueued);
        assert_eq!(
            enqueued.lock().await.as_slice(),
            &[(42, "buffer me".into())]
        );
        assert_eq!(queue.lock().await.len(), 1);
        assert_eq!(*watermarks.lock().await, Vec::<i64>::new());

        let server_task = tokio::spawn(async move {
            let frame = server
                .next()
                .await
                .expect("turn/start frame")
                .expect("turn/start frame ok");
            let Message::Text(text) = frame else {
                panic!("expected text frame");
            };
            let req: Value = serde_json::from_str(&text).expect("json-rpc request");
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("turn/start")
            );
            assert_eq!(
                req.pointer("/params/threadId").and_then(Value::as_str),
                Some("thread-empty-goal-test")
            );
            assert_eq!(
                req.pointer("/params/input/0/text").and_then(Value::as_str),
                Some("buffer me")
            );
            server
                .send(Message::Text(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req.get("id").cloned().unwrap_or(Value::Null),
                        "result": { "turn": { "id": "turn-flush" } }
                    })
                    .to_string(),
                ))
                .await
                .expect("send turn/start response");
        });

        let mut consumer = NotificationConsumer {
            notifs,
            thread_id_slot: Arc::clone(&thread_id_slot),
            status: Arc::clone(&status),
            source: SpecPusherSource::Legacy { client },
            queue: Arc::clone(&queue),
            watermark_sink,
            queue_persist,
            initial_prompt_ready_sink,
            watchdog: TurnWatchdogConfig::default(),
            recovery_signal: None,
            active_turn: None,
            initial_prompt_ready_attempted: false,
        };
        consumer
            .process_notification(Notification::TurnStarted {
                thread_id: "thread-empty-goal-test".to_string(),
                turn: serde_json::json!({ "id": "turn-user-1" }),
            })
            .await;
        assert_eq!(
            thread_id_slot.lock().await.as_deref(),
            Some("thread-empty-goal-test")
        );
        assert_eq!(
            ready_threads.lock().await.as_slice(),
            &["thread-empty-goal-test".to_string()]
        );
        assert_eq!(queue.lock().await.len(), 1);

        consumer
            .process_notification(Notification::TurnCompleted {
                thread_id: "thread-empty-goal-test".to_string(),
                turn: serde_json::json!({ "id": "turn-user-1" }),
            })
            .await;
        server_task.await.expect("server task");
        assert!(queue.lock().await.is_empty());
        assert_eq!(*watermarks.lock().await, vec![42]);
        assert_eq!(*dequeued.lock().await, vec![1]);
    }

    #[tokio::test]
    async fn initial_turn_lifecycle_fails_on_child_exit_without_budget() {
        let (_client, mut notifs, _server) = CodexAppServer::connect_pair_for_test().await;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn exiting child");
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            last_thread_id: Some("thread-test".into()),
            ..Default::default()
        }));

        let err = tokio::time::timeout(
            Duration::from_secs(1),
            await_initial_turn_lifecycle(&mut child, &mut notifs, "thread-test", &status),
        )
        .await
        .expect("child exit should win promptly")
        .expect_err("child exit is a lifecycle failure");
        assert!(
            err.to_string()
                .contains("exited before initial turn lifecycle"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn registry_insert_get_remove() {
        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-1");

        // Empty-state contract.
        assert!(reg.is_empty());
        assert!(!reg.contains(&wave));
        assert!(reg.remove(&wave).is_none());

        // Insert a live handle, then observe it via `contains` / `len`.
        let (handle, _server) = fake_handle().await;
        let thread_id = handle.thread_id.clone();
        assert!(reg.insert(wave.clone(), handle).is_none());
        assert!(reg.contains(&wave));
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 1);

        // A different wave is independent.
        let other = WaveId::from("wave-2");
        assert!(!reg.contains(&other));

        // Remove returns the handle (same thread id) and empties the map.
        let removed = reg
            .remove(&wave)
            .expect("remove returns the inserted handle");
        assert_eq!(removed.thread_id, thread_id);
        drop(removed); // child reaped via kill_on_drop here.
        assert!(!reg.contains(&wave));
        assert!(reg.is_empty());
        // Removing again is a clean None.
        assert!(reg.remove(&wave).is_none());
    }

    #[tokio::test]
    async fn registry_insert_replaces_and_returns_prior_handle() {
        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-replace");
        let (h1, _s1) = fake_handle().await;
        let (h2, _s2) = fake_handle().await;
        assert!(reg.insert(wave.clone(), h1).is_none());
        // Second insert on the same key returns the prior handle.
        let prior = reg
            .insert(wave.clone(), h2)
            .expect("prior handle returned on replace");
        drop(prior);
        assert_eq!(reg.len(), 1);
        let _ = reg.remove(&wave);
    }

    /// #322 — `park` is the production registration path; it runs the
    /// aspect framework's `BeforeHandleParkInRegistry` checks before
    /// inserting. With the production aspect set (INV-6's
    /// `WatermarkSinkInstalledAspect` is registered by
    /// `state::build_aspect_registry`), parking a handle that's missing
    /// its watermark sink MUST panic — the `fake_handle` helper builds
    /// exactly this shape (no sink installed) so this test pins the
    /// "park without sink panics" contract on the production aspect set.
    ///
    /// Without this enforcement, a future refactor that splits the
    /// `install_watermark_sink` call from the park site would silently
    /// drop queued-then-flushed envelopes from the durable watermark —
    /// the original #313 bug class, now caught at park time in release
    /// builds too (not just under `debug_assert!`).
    #[tokio::test]
    #[should_panic(expected = "watermark-sink-installed")]
    async fn park_panics_when_aspect_fails() {
        use crate::aspect::{AspectRegistry, WatermarkSinkInstalledAspect};

        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-no-sink");
        let (handle, _server) = fake_handle().await;
        // `fake_handle` builds a handle with `watermark_sink = None`;
        // INV-6's aspect must trip.
        let mut aspects = AspectRegistry::new();
        aspects.register_before_handle_park(Arc::new(WatermarkSinkInstalledAspect));
        // Panics inside the aspect dispatcher; the `expected` substring
        // pins the aspect name in the panic message so a rename is a
        // visible diff.
        let _ = reg.park(wave, handle, &aspects).await;
    }

    /// Sibling positive case: with the watermark sink installed, the
    /// aspect passes and `park` behaves like a bare `insert`. Pins the
    /// "park is a transparent wrapper around insert on the happy path"
    /// contract so a future aspect refactor can't break the production
    /// register sequence.
    #[tokio::test]
    async fn park_succeeds_when_invariant_holds() {
        use crate::aspect::{AspectRegistry, WatermarkSinkInstalledAspect};

        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-with-sink");
        let (handle, _server) = fake_handle().await;

        // Install a no-op sink so INV-6 passes. The sink itself is
        // never invoked here — the aspect only probes presence via
        // `has_watermark_sink`.
        let sink: WatermarkSink = Arc::new(|_id| Box::pin(async move {}));
        handle.install_watermark_sink(sink).await;

        let mut aspects = AspectRegistry::new();
        aspects.register_before_handle_park(Arc::new(WatermarkSinkInstalledAspect));

        // First park: no prior handle.
        assert!(reg.park(wave.clone(), handle, &aspects).await.is_none());
        assert!(reg.contains(&wave));
        let _ = reg.remove(&wave);
    }

    /// `park` with an empty aspect registry is a noop dispatcher around
    /// `insert` — proves the aspect framework's zero-aspect case stays
    /// cheap (and exists so tests that don't care about aspects can
    /// still exercise the production code path without registering
    /// every aspect every time).
    #[tokio::test]
    async fn park_with_empty_aspect_registry_just_inserts() {
        use crate::aspect::AspectRegistry;

        let reg = SpecPushRegistry::new();
        let wave = WaveId::from("wave-empty-aspects");
        let (handle, _server) = fake_handle().await;
        let aspects = AspectRegistry::new();
        assert!(reg.park(wave.clone(), handle, &aspects).await.is_none());
        assert!(reg.contains(&wave));
        let _ = reg.remove(&wave);
    }

    #[tokio::test]
    async fn handle_status_snapshot_round_trips() {
        let (handle, _server) = fake_handle().await;
        // Fresh handle: default phase.
        let st = handle.status().await;
        assert_eq!(st.phase, SpecPushPhase::Idle);
    }

    #[test]
    fn spec_push_phase_default_is_idle() {
        assert_eq!(SpecPushPhase::default(), SpecPushPhase::Idle);
        let st = SpecPushStatus::default();
        assert_eq!(st.phase, SpecPushPhase::Idle);
        assert!(st.last_thread_id.is_none());
        assert!(st.last_turn_id.is_none());
    }

    /// #328 P2 (parser comm-paren test) — load-bearing `rsplit_once(')')`
    /// in [`parse_starttime_from_stat`] must survive `comm` blobs that
    /// contain literal `)` characters (the kernel allows arbitrary bytes
    /// inside the parens). Before the extraction this branch was never
    /// covered: production callers fed `sleep` / `true` with paren-free
    /// `comm`, and a regression that switched to `split_once(')')` would
    /// silently misalign field 22 against the wrong token.
    ///
    /// Synthesized stat content follows proc(5) layout: pid `(comm)` state
    /// ppid pgrp session tty_nr tpgid flags minflt cminflt majflt cmajflt
    /// utime stime cutime cstime priority nice num_threads itrealvalue
    /// **starttime** …
    #[test]
    fn parse_starttime_handles_normal_comm() {
        // pid=1234, comm="bash", starttime=98765 at field 22.
        // Indices after the LAST ')': state(0) ppid(1) pgrp(2) session(3)
        // tty(4) tpgid(5) flags(6) minflt(7) cminflt(8) majflt(9)
        // cmajflt(10) utime(11) stime(12) cutime(13) cstime(14)
        // priority(15) nice(16) num_threads(17) itrealvalue(18)
        // starttime(19).
        let stat = "1234 (bash) S 1 1234 1234 0 -1 4194304 100 0 0 0 5 3 0 0 20 0 1 0 98765 5242880 100 18446744073709551615";
        assert_eq!(parse_starttime_from_stat(stat), Some(98765));
    }

    #[test]
    fn parse_starttime_handles_comm_with_paren() {
        // comm = "name with paren)" — note the LITERAL ')' inside the comm
        // blob. A naive `split_once(')')` would terminate at the inner
        // paren and read `with` (field offset 0 of the wrong tail) as
        // state, misaligning every subsequent index and parsing the wrong
        // u64. `rsplit_once(')')` finds the closing paren of the comm
        // wrap and the parse is correct.
        let stat = "9999 (name) with paren)) S 1 9999 9999 0 -1 4194304 100 0 0 0 5 3 0 0 20 0 1 0 424242 5242880 100 0";
        assert_eq!(parse_starttime_from_stat(stat), Some(424242));
    }

    #[test]
    fn parse_starttime_handles_comm_with_spaces_and_parens() {
        // comm = "weird (name)" — embedded space + paren. Same defense.
        let stat = "42 (weird (name)) S 1 42 42 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 7777 0 0 0";
        assert_eq!(parse_starttime_from_stat(stat), Some(7777));
    }

    #[test]
    fn parse_starttime_returns_none_on_malformed() {
        // No closing paren at all → rsplit fails.
        assert_eq!(parse_starttime_from_stat("1 bash S 1 1"), None);
        // Closing paren but not enough fields after it for index 19.
        assert_eq!(parse_starttime_from_stat("1 (bash) S 1 1 1"), None);
        // Field 22 is non-numeric.
        let bad = "1 (bash) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 NOT_A_NUMBER 0";
        assert_eq!(parse_starttime_from_stat(bad), None);
    }

    /// PR3b delivery-decision table. The whole policy lives in the pure
    /// [`decide`] fn so it's testable without an app-server. Idle /
    /// TurnCompleted are "between turns" → start a turn now; TurnRunning AND
    /// Issuing → enqueue (a `turn/start` issued mid-turn is silently dropped
    /// by codex, verified against the real binary in the PR3b probe;
    /// `Issuing` means another caller already won the right to issue — B1).
    #[test]
    fn decide_push_action_table() {
        assert_eq!(decide(SpecPushPhase::Idle), PushAction::StartTurnNow);
        assert_eq!(
            decide(SpecPushPhase::TurnCompleted),
            PushAction::StartTurnNow
        );
        assert_eq!(
            decide(SpecPushPhase::PendingThreadStart),
            PushAction::Enqueue
        );
        assert_eq!(decide(SpecPushPhase::TurnRunning), PushAction::Enqueue);
        // B1: a caller observing `Issuing` must enqueue, never issue a second
        // (silently-dropped) turn/start.
        assert_eq!(decide(SpecPushPhase::Issuing), PushAction::Enqueue);
        // #318 INV-4 (R2-B2): a caller observing `Resumed` (boot-takeover
        // path right after `thread/resume`, before any lifecycle
        // notification has reconciled the phase) must enqueue — the
        // server may still be running the prior boot's turn and a
        // `turn/start` would be silently dropped.
        assert_eq!(decide(SpecPushPhase::Resumed), PushAction::Enqueue);
        // #347 layer B: a watchdog-bailed handle must not keep accepting
        // observations into a queue whose consumer no longer has a reliable
        // process behind it. Runtime recovery replays from durable events.
        assert_eq!(decide(SpecPushPhase::Wedged), PushAction::RejectWedged);
    }

    #[tokio::test]
    async fn push_observation_rejects_wedged_without_queueing() {
        let (handle, _server) = fake_handle().await;
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::Wedged;
        }

        let err = handle
            .push_observation(123, "must not enter dead queue")
            .await
            .expect_err("wedged handle rejects pushes");
        assert!(
            err.to_string().contains("wedged"),
            "error should explain wedged phase: {err}"
        );
        assert!(handle.queue.lock().await.is_empty());
    }

    /// `push_observation` on an idle handle issues a `turn/start` the fake
    /// server observes, and claims the issue right by flipping the tracked
    /// phase to `Issuing` (B1). The phase only advances to `TurnRunning`
    /// once the server's `turn/started` lands (not sent in this test).
    #[tokio::test]
    async fn push_observation_starts_turn_when_idle() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        // Idle by default → push should fire a turn/start.
        let push = tokio::spawn(async move {
            let outcome = handle.push_observation(1, "an observation").await.unwrap();
            // Single observation, no queue drain → max id is this push's id.
            assert_eq!(outcome, PushOutcome::Issued { max_envelope_id: 1 });
            // Phase claimed as `Issuing` by the winning push_observation (B1);
            // a real `turn/started` would later reconcile it to TurnRunning.
            assert_eq!(handle.status().await.phase, SpecPushPhase::Issuing);
            handle
        });

        // Server side: read the request frame, confirm it's a turn/start
        // carrying our text, and answer its id so the client's request
        // resolves.
        let req = loop {
            match server.next().await.expect("frame").expect("ws ok") {
                Message::Text(t) => break serde_json::from_str::<Value>(&t).unwrap(),
                _ => continue,
            }
        };
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("turn/start")
        );
        let input_text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(input_text, "an observation");
        let id = req.get("id").cloned().unwrap();
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "turn": { "id": "t-push-1" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        let _handle = push.await.unwrap();
        // Keep the server end alive until here.
        let _server = server;
    }

    /// `push_observation` while a turn is running enqueues (no turn/start
    /// frame is sent); the queued text is then flushed by the consumer on
    /// the next `turn/completed`.
    #[tokio::test]
    async fn push_observation_enqueues_during_turn_then_flushes_on_completed() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        // Force the tracked phase to TurnRunning.
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnRunning;
        }
        // Two pushes while "running" — both enqueue, no frame on the wire.
        assert_eq!(
            handle.push_observation(10, "obs one").await.unwrap(),
            PushOutcome::Enqueued
        );
        assert_eq!(
            handle.push_observation(11, "obs two").await.unwrap(),
            PushOutcome::Enqueued
        );
        assert_eq!(handle.queue.lock().await.len(), 2);

        // Drive a turn/completed THROUGH the real notification channel so
        // the consumer task's flush path runs. The consumer is reading the
        // client's NotificationStream, which the fake server feeds via WS
        // notification frames.
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "method": "turn/completed",
                    "params": { "threadId": "thread-test", "turn": { "id": "t-done" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        // The consumer should now flush a single coalesced turn/start with
        // both observations joined by a newline.
        let req = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match server.next().await.expect("frame").expect("ws ok") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v.get("method").and_then(Value::as_str) == Some("turn/start") {
                            break v;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect("flush turn/start must arrive after turn/completed");

        let input_text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            input_text, "obs one\nobs two",
            "flush must coalesce queued observations into one turn"
        );
        // Queue drained.
        assert!(handle.queue.lock().await.is_empty());
        let _server = server;
    }

    /// #325 regression — `rehydrate_queue_from_persist` returns the
    /// envelope_ids it just re-loaded, in FIFO order. Boot-takeover's
    /// `register_and_catch_up` feeds these into a skip-set so the
    /// subsequent `events_since(watermark)` catch-up replay doesn't
    /// re-deliver the same envelope that is already sitting in the
    /// rehydrated queue.
    ///
    /// Without the returned ids, the dedup invariant would be impossible
    /// to enforce — the catch-up loop has no other way to know which
    /// rows came from disk vs. which arrived live.
    #[tokio::test]
    async fn rehydrate_queue_from_persist_returns_envelope_ids_for_dedup() {
        use std::sync::Mutex as StdMutex;

        let (handle, _server) = fake_handle().await;

        // Stub QueuePersist: `list` returns three pre-seeded rows with
        // known envelope_ids; `enqueue` / `dequeue` aren't exercised here.
        let stored: Arc<StdMutex<Vec<(i64, i64, String)>>> = Arc::new(StdMutex::new(vec![
            (101, 5, "obs-five".to_string()),
            (102, 7, "obs-seven".to_string()),
            (103, 9, "obs-nine".to_string()),
        ]));
        let stored_for_list = Arc::clone(&stored);
        let persist = QueuePersist {
            enqueue: Arc::new(|_, _| Box::pin(async { None })),
            dequeue: Arc::new(|_| Box::pin(async {})),
            list: Arc::new(move || {
                let snapshot = stored_for_list.lock().unwrap().clone();
                Box::pin(async move { snapshot })
            }),
        };
        handle.install_queue_persist(persist).await;

        // watermark=0 means "nothing already delivered" — all three rows
        // are live and should rehydrate (this test predates the round-2
        // watermark-filter; see `rehydrate_filters_stale_rows_against_watermark`
        // for the filter coverage).
        let ids = handle.rehydrate_queue_from_persist(0).await;
        assert_eq!(
            ids,
            vec![5, 7, 9],
            "#325: rehydrate must return the envelope_ids in FIFO order so \
             catch-up can dedup against them"
        );

        // Queue restored.
        let q = handle.queue.lock().await;
        assert_eq!(q.len(), 3);
        assert_eq!(q[0].envelope_id, 5);
        assert_eq!(q[0].db_id, Some(101));
        assert_eq!(q[1].envelope_id, 7);
        assert_eq!(q[2].envelope_id, 9);
    }

    /// #325 regression — full Enqueue-persist → crash → rehydrate +
    /// catch-up flow, asserting NO duplicate `turn/start` payload.
    ///
    /// The scenario codex's P1 review flagged:
    ///
    /// 1. Pre-crash: a `task.completed` envelope (id=10) arrives mid-turn
    ///    → `Inner::push_to_spec` returns `Ok(Enqueued)` → the Enqueue
    ///    arm of `push_observation` persists a `spec_push_queue` row +
    ///    pushes onto the in-memory `VecDeque`. The dispatcher
    ///    deliberately does NOT advance the durable `push_watermark` on
    ///    `Enqueued` (PR #315 PR4 B1).
    /// 2. Kernel crashes between persist and the consumer task's flush.
    /// 3. Boot-takeover resumes the spec thread (phase = Idle).
    ///    `rehydrate_queue_from_persist` re-loads row id=10 into the
    ///    in-memory queue. `events_since(watermark)` ALSO returns id=10
    ///    (watermark < 10 because step 1 deliberately didn't advance it).
    /// 4. Without dedup: catch-up replay calls
    ///    `catch_up_push_under_lock(10)` → `push_observation` on the Idle
    ///    handle → `StartTurnNow` → drains rehydrated row + appends the
    ///    catch-up envelope → ONE `turn/start` with TWO copies of the
    ///    SAME observation. (And a SECOND `spec_push_queue` row is
    ///    persisted for the appended item if there's contention; here we
    ///    assert on the wire because the wire is the codex-facing
    ///    contract that matters.)
    /// 5. With dedup (this PR's fix): catch-up skips id=10 because it's
    ///    in the rehydrated skip-set. The explicit `flush_pending` then
    ///    drives ONE `turn/start` with ONE copy of the observation.
    ///
    /// The test stays under the spec_appserver layer (no dispatcher
    /// wiring) — we directly model what `register_and_catch_up` does
    /// after rehydrate: build the skip-set from rehydrate's return value
    /// and only deliver catch-up envelopes whose id isn't in it.
    #[tokio::test]
    async fn rehydrate_then_catch_up_does_not_double_deliver_same_envelope() {
        use futures_util::{SinkExt, StreamExt};
        use std::collections::HashSet;
        use std::sync::Mutex as StdMutex;
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        let handle = Arc::new(handle);

        // Stub persist: pre-seeded with one "surviving from prior process"
        // row at envelope_id = 10. Track dequeue ids to verify the flush
        // wires up correctly post-recovery.
        let stored: Arc<StdMutex<Vec<(i64, i64, String)>>> = Arc::new(StdMutex::new(vec![(
            999,
            10,
            "task.completed-for-id-10".to_string(),
        )]));
        let dequeued: Arc<StdMutex<Vec<i64>>> = Arc::new(StdMutex::new(Vec::new()));
        let stored_for_list = Arc::clone(&stored);
        let stored_for_enq = Arc::clone(&stored);
        let dequeued_for_close = Arc::clone(&dequeued);
        let persist = QueuePersist {
            enqueue: Arc::new(move |envelope_id, text| {
                // Mimic the SQL INSERT: append + return a fresh row id.
                let stored = Arc::clone(&stored_for_enq);
                Box::pin(async move {
                    let mut g = stored.lock().unwrap();
                    let new_id = g.iter().map(|(id, _, _)| *id).max().unwrap_or(0) + 1;
                    g.push((new_id, envelope_id, text));
                    Some(new_id)
                })
            }),
            dequeue: Arc::new(move |ids| {
                let dequeued = Arc::clone(&dequeued_for_close);
                Box::pin(async move {
                    dequeued.lock().unwrap().extend(ids);
                })
            }),
            list: Arc::new(move || {
                let snap = stored_for_list.lock().unwrap().clone();
                Box::pin(async move { snap })
            }),
        };
        handle.install_queue_persist(persist).await;

        // Step 3: rehydrate. The skip-set is built from the returned ids
        // — same as `register_and_catch_up` does in production.
        // watermark=9: the prior process's dispatcher cooperatively
        // withheld push_watermark on Enqueued (PR #315 PR4 B1), so the
        // durable watermark is BELOW envelope_id=10 and the rehydrate
        // filter keeps the row live.
        let rehydrated_ids = handle.rehydrate_queue_from_persist(9).await;
        assert_eq!(
            rehydrated_ids,
            vec![10],
            "rehydrate must return the envelope_ids of pending rows"
        );
        let skip: HashSet<i64> = rehydrated_ids.iter().copied().collect();

        // Step 4: simulate catch-up replay. `events_since(watermark)`
        // would return id=10 (since the watermark is below it — see
        // step 1 in the docstring). We model the catch-up loop's
        // dedup-then-deliver: if id is in skip-set, do NOT call
        // push_observation; otherwise call it.
        //
        // With dedup ON (this PR's fix): id=10 is in skip → loop does
        // nothing → no StartTurnNow fires.
        let catch_up_ids = [10i64];
        let mut replayed = 0usize;
        for id in catch_up_ids {
            if skip.contains(&id) {
                continue;
            }
            // We don't actually exercise the dispatcher here — the
            // production catch_up_push_under_lock would call
            // push_observation. The skip path means this branch is dead
            // in this test; asserting via `replayed` below.
            let _ = handle
                .push_observation(id, "would-be-duplicate-payload")
                .await;
            replayed += 1;
        }
        assert_eq!(
            replayed, 0,
            "#325 dedup: id=10 must be skipped because the same envelope \
             already sits in the rehydrated queue"
        );

        // Step 5: explicit flush_pending — the no-other-catch-up edge
        // case `register_and_catch_up` now drives. With our fix, this
        // issues ONE `turn/start` with ONE copy of the rehydrated
        // observation.
        let flush_handle = Arc::clone(&handle);
        let flush = tokio::spawn(async move {
            flush_handle.flush_pending().await;
        });

        // Read frames from the server side; capture every `turn/start`
        // payload and ack so the client's request resolves.
        let observed_turns: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let observed_for_task = Arc::clone(&observed_turns);
        let server_task = tokio::spawn(async move {
            // Bounded — one turn/start expected, plus generous slack.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while tokio::time::Instant::now() < deadline {
                let f = match tokio::time::timeout(Duration::from_millis(250), server.next()).await
                {
                    Ok(Some(Ok(Message::Text(t)))) => t,
                    Ok(Some(Ok(_))) => continue,
                    Ok(None) | Ok(Some(Err(_))) => break,
                    Err(_) => continue,
                };
                let v: Value = match serde_json::from_str(&f) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("method").and_then(Value::as_str) != Some("turn/start") {
                    continue;
                }
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                let text = v
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                observed_for_task.lock().unwrap().push(text);
                // Ack so the client's `turn_start` future resolves.
                let _ = server
                    .send(Message::Text(
                        serde_json::to_string(&serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "turn": { "id": "t-recovery" } }
                        }))
                        .unwrap(),
                    ))
                    .await;
            }
            server
        });

        // Wait for the flush to finish (client side resolves).
        flush.await.expect("flush_pending joins");
        // Give the server task time to observe the turn/start frame.
        let _ = tokio::time::timeout(Duration::from_millis(500), server_task).await;

        let turns = observed_turns.lock().unwrap().clone();
        assert_eq!(
            turns.len(),
            1,
            "#325: exactly one `turn/start` must be issued after rehydrate + \
             catch-up — not two (duplicate-payload bug) and not zero \
             (rehydrated items left undelivered). got: {turns:?}"
        );
        assert_eq!(
            turns[0], "task.completed-for-id-10",
            "#325: the single `turn/start` must carry the rehydrated \
             observation's text exactly once (no double-line coalescing)"
        );

        // Sanity: the rehydrated row was dequeued after delivery, so a
        // hypothetical *third* boot wouldn't see it again. This mirrors
        // the persist↔in-memory 1:1 invariant `flush_push_queue` upholds.
        let drained = dequeued.lock().unwrap().clone();
        assert_eq!(
            drained,
            vec![999],
            "#325: the rehydrated row's db_id must be dequeued after the \
             explicit flush_pending — otherwise a third boot would replay it again"
        );
    }

    /// #325 round-2 P1 — `Enqueue`-arm persist-await race close.
    ///
    /// The race codex's round-2 review flagged:
    /// 1. Phase observed as `TurnRunning` → action fixed to `Enqueue`.
    /// 2. `persist_one` awaits the DB insert (real-world: tens of ms).
    /// 3. During (2), a `turn/completed` arrives; consumer task's
    ///    `flush_push_queue` runs, finds the queue empty (our row not yet
    ///    appended), walks phase Issuing→TurnCompleted with nothing to
    ///    flush — and no further `turn/completed` is expected.
    /// 4. `persist_one` returns; we append to the in-memory queue and
    ///    return `Ok(Enqueued)`. Without the round-2 fix, the row stays
    ///    stranded until an unrelated live event or restart.
    ///
    /// The fix: after the append, re-acquire status and — if phase is no
    /// longer in `{Issuing, TurnRunning}` — drive an idempotent
    /// `flush_pending`. This test:
    ///   * installs a slow `persist.enqueue` that blocks on a oneshot,
    ///     simulating the DB-insert window;
    ///   * starts the push (it blocks inside `persist_one`);
    ///   * walks the phase to `TurnCompleted` from outside (simulating
    ///     the consumer's flush-of-empty path);
    ///   * unblocks `persist_one`;
    ///   * asserts the row is FLUSHED via a `turn/start` on the wire —
    ///     not stranded in the queue.
    #[tokio::test]
    async fn enqueue_arm_flushes_when_persist_await_races_past_turn_completed() {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::Mutex as StdMutex;
        use tokio::sync::oneshot;
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        let handle = Arc::new(handle);
        // Force phase to `TurnRunning` so `push_observation`'s `decide` picks
        // `Enqueue` (the racy arm).
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnRunning;
        }

        // Slow persist: block on a oneshot so we control exactly when
        // `persist_one` returns. The same closure must also serve
        // dequeues (flush success path).
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        let next_db_id = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let next_db_id_e = Arc::clone(&next_db_id);
        let release_rx_e = Arc::clone(&release_rx);
        let persist = QueuePersist {
            enqueue: Arc::new(move |_envelope_id, _text| {
                let next_db_id = Arc::clone(&next_db_id_e);
                let release_rx = Arc::clone(&release_rx_e);
                Box::pin(async move {
                    // Take the receiver (single-shot); if absent, no wait.
                    let rx_opt = release_rx.lock().unwrap().take();
                    if let Some(rx) = rx_opt {
                        let _ = rx.await;
                    }
                    Some(next_db_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
                })
            }),
            dequeue: Arc::new(|_| Box::pin(async {})),
            list: Arc::new(|| Box::pin(async { Vec::new() })),
        };
        handle.install_queue_persist(persist).await;

        // Step 1+2: start the push; it blocks inside `persist_one`.
        let push_handle = Arc::clone(&handle);
        let push =
            tokio::spawn(async move { push_handle.push_observation(42, "racy-observation").await });

        // Wait for `push_observation` to enter the persist await. We can't
        // observe the precise moment, so yield + brief sleep is sufficient
        // — `push_observation`'s status-lock + decide work is pure-CPU.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Step 3: simulate the consumer's `flush_push_queue` on an empty
        // queue — it walks phase Issuing→TurnCompleted but finds nothing
        // to flush. We just set the phase directly: the bug is "after this
        // walk, no `turn/completed` is expected to come back", and our
        // post-append recheck must detect that and force a flush.
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnCompleted;
        }

        // Step 4: release `persist_one` so `push_observation` continues.
        // The Enqueue arm appends to the queue, re-acquires status, sees
        // `TurnCompleted` (NOT in {Issuing, TurnRunning}), and drives
        // `flush_pending` → `flush_push_queue` → coalesced `turn/start`.
        let _ = release_tx.send(());

        // Server side: expect ONE `turn/start` with our observation text.
        // Ack it so the client's request resolves and `flush_pending`
        // returns.
        let req = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match server.next().await.expect("frame").expect("ws ok") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v.get("method").and_then(Value::as_str) == Some("turn/start") {
                            break v;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect(
            "#325 round-2 P1: the stranded row must be flushed via flush_pending — \
             no `turn/start` arrived within 3s, meaning the row is still sitting \
             in the queue undelivered",
        );

        let text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            text, "racy-observation",
            "#325 round-2 P1: the forced flush must carry the just-appended observation"
        );

        // Ack so the push future resolves.
        let id = req.get("id").cloned().unwrap();
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "turn": { "id": "t-race-close" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        let outcome = push.await.unwrap().unwrap();
        assert_eq!(
            outcome,
            PushOutcome::Enqueued,
            "#325 round-2 P1: outcome is still Enqueued (the contract holds — the \
             caller never sees the internal force-flush); the flush_pending is a \
             best-effort safety net on top, not a new outcome variant"
        );

        // Queue must be empty post-flush (the row was flushed, then dequeued).
        assert!(
            handle.queue.lock().await.is_empty(),
            "#325 round-2 P1: queue must drain after the forced flush"
        );

        let _server = server;
    }

    /// #327 regression — same shape as the #325 round-2 P1 test above, but
    /// the racing promoter is `resume_reconcile_task` walking
    /// `Resumed → TurnCompleted` instead of the consumer task's
    /// `turn/completed`-driven `Issuing → TurnCompleted` walk. PR #323
    /// introduced this second promoter (a 5s budget timer that recovers the
    /// idle-resume case where `thread/resume` lands on a server with no
    /// in-flight turn), and #327 flagged that it opens a structurally
    /// identical race window against `push_observation`'s status/queue lock
    /// gap:
    ///
    /// 1. `push_observation` locks status, sees `Resumed`, picks `Enqueue`,
    ///    releases status (no claim flip — `Resumed` is a "wait" phase).
    /// 2. `persist_one` awaits the DB insert.
    /// 3. The reconciler timer fires: locks status, CAS-promotes
    ///    `Resumed → TurnCompleted`, releases, calls `flush_push_queue`
    ///    against an empty queue (our row isn't appended yet), no-ops.
    /// 4. `persist_one` returns; we `push_back` the row into the queue —
    ///    but the only future flush trigger was the reconciler that just
    ///    walked past, and no `turn/completed` is coming on an idle thread.
    ///    Pre-#325-post-enqueue-recheck, the row would sit stranded.
    ///
    /// The #325 P1 fix (`!matches!(phase, Issuing | TurnRunning)` recheck +
    /// idempotent `flush_pending`) closes this branch incidentally because
    /// `TurnCompleted ∉ {Issuing, TurnRunning}` — the same recheck that
    /// catches the consumer-task race also catches the reconciler race.
    /// This test pins that coverage so a future refactor of the recheck
    /// predicate (e.g. narrowing it to "only on `TurnCompleted`" or
    /// excluding `Resumed` itself in some way) cannot silently re-open the
    /// #327 window.
    ///
    /// Difference from the #325 P1 test: initial phase is `Resumed`
    /// (boot-takeover plant from PR #323), and the racing promoter is the
    /// reconciler walking `Resumed → TurnCompleted` rather than a
    /// `turn/completed`-driven flush walking `Issuing → TurnCompleted`.
    /// Both end at `TurnCompleted`, both leave the queue empty at the time
    /// of the walk, and both rely on the post-enqueue recheck to catch the
    /// just-appended row.
    #[tokio::test]
    async fn enqueue_arm_flushes_when_resume_reconciler_races_past_persist_await() {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::Mutex as StdMutex;
        use tokio::sync::oneshot;
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        let handle = Arc::new(handle);
        // Boot-takeover post-`thread/resume` posture: phase = Resumed. A
        // push observed in this phase routes to `Enqueue` (see
        // `decide(Resumed)`), so this exercises the racy arm.
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::Resumed;
        }

        // Slow persist: block on a oneshot so we control exactly when
        // `persist_one` returns, simulating the DB-insert window during
        // which the reconciler can walk past us.
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        let next_db_id = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let next_db_id_e = Arc::clone(&next_db_id);
        let release_rx_e = Arc::clone(&release_rx);
        let persist = QueuePersist {
            enqueue: Arc::new(move |_envelope_id, _text| {
                let next_db_id = Arc::clone(&next_db_id_e);
                let release_rx = Arc::clone(&release_rx_e);
                Box::pin(async move {
                    let rx_opt = release_rx.lock().unwrap().take();
                    if let Some(rx) = rx_opt {
                        let _ = rx.await;
                    }
                    Some(next_db_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
                })
            }),
            dequeue: Arc::new(|_| Box::pin(async {})),
            list: Arc::new(|| Box::pin(async { Vec::new() })),
        };
        handle.install_queue_persist(persist).await;

        // Step 1+2: start the push; it blocks inside `persist_one`.
        let push_handle = Arc::clone(&handle);
        let push = tokio::spawn(async move {
            push_handle
                .push_observation(77, "resume-race-observation")
                .await
        });

        // Give `push_observation` time to: lock status, observe `Resumed`,
        // pick `Enqueue`, release status, and enter `persist_one`'s await.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            handle.status.lock().await.phase,
            SpecPushPhase::Resumed,
            "preconditions: phase must still be Resumed (push is stuck in persist_one's await)"
        );
        assert!(
            handle.queue.lock().await.is_empty(),
            "preconditions: queue must still be empty (the push hasn't appended yet)"
        );

        // Step 3: simulate the reconciler timer firing. It locks status,
        // CAS-promotes `Resumed → TurnCompleted`, releases, and calls
        // `flush_push_queue` against the (still-empty) queue — which
        // claims `Issuing`, finds nothing to drain, and walks back to
        // `TurnCompleted`. We invoke `flush_push_queue` directly with the
        // handle's slots so the test is deterministic (no need to wait
        // out the 5s budget under `start_paused`); this matches the
        // reconciler's effective behavior on the status/queue from the
        // push's perspective.
        {
            let mut g = handle.status.lock().await;
            assert_eq!(g.phase, SpecPushPhase::Resumed);
            g.phase = SpecPushPhase::TurnCompleted;
        }
        flush_push_queue(
            handle.thread_id.as_deref().expect("fake handle thread id"),
            &handle.status,
            &handle.pusher().source,
            &handle.queue,
            &handle.watermark_sink,
            &handle.queue_persist,
        )
        .await;
        // After the reconciler's flush-against-empty, phase walked back to
        // `TurnCompleted` (the no-drain release path).
        assert_eq!(
            handle.status.lock().await.phase,
            SpecPushPhase::TurnCompleted
        );

        // Step 4: release `persist_one`. `push_observation` now appends
        // to the queue, re-acquires status, sees `TurnCompleted` (NOT in
        // {Issuing, TurnRunning}), and drives `flush_pending` →
        // `flush_push_queue` → coalesced `turn/start`. Without the
        // post-enqueue recheck, the row would sit stranded with no future
        // lifecycle notification to trigger a flush.
        let _ = release_tx.send(());

        // Server side: expect ONE `turn/start` with our observation text.
        let req = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match server.next().await.expect("frame").expect("ws ok") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v.get("method").and_then(Value::as_str) == Some("turn/start") {
                            break v;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect(
            "#327: stranded row must be flushed via post-enqueue recheck — \
             no `turn/start` arrived within 3s, meaning the row is sitting \
             stranded in the queue after the reconciler walked past",
        );

        let text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            text, "resume-race-observation",
            "#327: the forced flush must carry the just-appended observation"
        );

        let id = req.get("id").cloned().unwrap();
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "turn": { "id": "t-327-race-close" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        let outcome = push.await.unwrap().unwrap();
        assert_eq!(
            outcome,
            PushOutcome::Enqueued,
            "#327: outcome is still Enqueued (the post-enqueue recheck is a \
             best-effort safety net; the public contract is unchanged)"
        );

        assert!(
            handle.queue.lock().await.is_empty(),
            "#327: queue must drain after the forced flush"
        );

        let _server = server;
    }

    /// #325 round-2 P2 — `rehydrate_queue_from_persist` must drop rows
    /// whose `envelope_id <= watermark` (the prior process's flush
    /// succeeded and bumped the watermark, but the `dequeue` write didn't
    /// commit — or committed and the row is stale). Without filtering,
    /// those rows would be re-pushed into the in-memory queue and
    /// redelivered to codex, bypassing the durable watermark that
    /// `events_since(watermark)` itself correctly skips.
    ///
    /// Scenario: watermark=10; rehydrate sees rows with envelope_ids
    /// `[5, 7, 12]`. 5 and 7 are stale (already delivered) and must be
    /// physically dequeued; 12 is live and must be queued + returned.
    #[tokio::test]
    async fn rehydrate_filters_stale_rows_against_watermark() {
        use std::sync::Mutex as StdMutex;

        let (handle, _server) = fake_handle().await;

        // Stub persist: pre-seed three rows; one above watermark, two
        // below. Track dequeue ids so we can assert the stale rows are
        // physically removed.
        let stored: Arc<StdMutex<Vec<(i64, i64, String)>>> = Arc::new(StdMutex::new(vec![
            (501, 5, "stale-five".to_string()),
            (502, 7, "stale-seven".to_string()),
            (503, 12, "live-twelve".to_string()),
        ]));
        let dequeued: Arc<StdMutex<Vec<i64>>> = Arc::new(StdMutex::new(Vec::new()));
        let stored_for_list = Arc::clone(&stored);
        let dequeued_for_close = Arc::clone(&dequeued);
        let persist = QueuePersist {
            enqueue: Arc::new(|_, _| Box::pin(async { None })),
            dequeue: Arc::new(move |ids| {
                let dequeued = Arc::clone(&dequeued_for_close);
                Box::pin(async move {
                    dequeued.lock().unwrap().extend(ids);
                })
            }),
            list: Arc::new(move || {
                let snap = stored_for_list.lock().unwrap().clone();
                Box::pin(async move { snap })
            }),
        };
        handle.install_queue_persist(persist).await;

        // watermark=10 — rows with envelope_id <= 10 (i.e. 5, 7) are
        // already delivered and must be dropped + dequeued.
        let live_ids = handle.rehydrate_queue_from_persist(10).await;
        assert_eq!(
            live_ids,
            vec![12],
            "#325 round-2 P2: only rows with envelope_id > watermark must be \
             returned for the catch-up skip-set"
        );

        // In-memory queue must contain ONLY the live row.
        let q = handle.queue.lock().await;
        assert_eq!(
            q.len(),
            1,
            "#325 round-2 P2: stale rows (envelope_id <= watermark) must be \
             skipped during in-memory rehydrate"
        );
        assert_eq!(q[0].envelope_id, 12);
        assert_eq!(q[0].text, "live-twelve");
        assert_eq!(q[0].db_id, Some(503));
        drop(q);

        // Stale rows must be physically dequeued so the NEXT boot doesn't
        // see them again. Order in `dequeued` matches stored-row iteration
        // order (5 before 7) — the rehydrate code processes the list in
        // order and collects stale_db_ids in encounter order.
        let mut deq = dequeued.lock().unwrap().clone();
        deq.sort();
        assert_eq!(
            deq,
            vec![501, 502],
            "#325 round-2 P2: stale rows must be physically deleted via the \
             persist.dequeue callback so a future boot doesn't see them again"
        );
    }

    /// Shared B1 harness: build a fake handle whose server faithfully models
    /// codex's **silent-drop** — a `turn/start` that arrives while a turn is
    /// already active is OK-acked but produces NO `turn/started`/
    /// `turn/completed` and its observation is NEVER recorded as delivered.
    /// The `driver` runs the push/flush sequence under test; afterwards we
    /// let the consumer flush any deferred (enqueued) observation across a
    /// later cycle, then return the set of observation lines the server
    /// actually delivered. A correct (single-winner) implementation loses
    /// nothing; the pre-fix double-issue drops one.
    async fn b1_collect_delivered<F, Fut>(driver: F) -> std::collections::HashSet<String>
    where
        F: FnOnce(Arc<SpecPushHandle>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use futures_util::{SinkExt, StreamExt};
        use std::collections::HashSet;
        use tokio_tungstenite::tungstenite::Message;

        let (handle, server) = fake_handle().await;
        let handle = Arc::new(handle);
        // Phase = TurnCompleted (between turns) with A already queued. The
        // envelope ids on queue items are arbitrary in this test (delivery
        // is asserted on `text`, not id) — pick 1 for A, the driver picks
        // 2 for B.
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::TurnCompleted;
            handle.queue.lock().await.push_back(QueuedObservation {
                envelope_id: 1,
                text: "A".to_string(),
                db_id: None,
            });
        }

        // Server task: model codex's silent-drop. A turn is "active" for a
        // short WINDOW (started → +TURN_RUN → completed). The read loop keeps
        // reading WHILE a turn is active (select! against the completion
        // timer), so a `turn/start` arriving during that window is genuinely
        // the dropped case. Without holding `active` across reads, two serial
        // turn/starts would never overlap and nothing would drop.
        let server_task = tokio::spawn(async move {
            const TURN_RUN: Duration = Duration::from_millis(60);
            let mut server = server;
            let mut delivered: HashSet<String> = HashSet::new();
            let mut turn_seq = 0u32;
            let mut active_until: Option<tokio::time::Instant> = None;
            let overall = tokio::time::Instant::now() + Duration::from_secs(3);
            loop {
                if tokio::time::Instant::now() >= overall {
                    break;
                }
                let next = if let Some(done_at) = active_until {
                    tokio::select! {
                        biased;
                        _ = tokio::time::sleep_until(done_at) => {
                            // Turn window elapsed → emit turn/completed, idle.
                            turn_seq += 1;
                            let turn_id = format!("turn-{turn_seq}");
                            server
                                .send(Message::Text(
                                    serde_json::to_string(&serde_json::json!({
                                        "jsonrpc": "2.0", "method": "turn/completed",
                                        "params": { "threadId": "thread-test", "turn": { "id": turn_id } }
                                    }))
                                    .unwrap(),
                                ))
                                .await
                                .ok();
                            active_until = None;
                            continue;
                        }
                        f = server.next() => f,
                    }
                } else {
                    match tokio::time::timeout(Duration::from_millis(200), server.next()).await {
                        Ok(f) => f,
                        Err(_) => continue, // idle read tick
                    }
                };
                let frame = match next {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(_)) => continue,
                    _ => break, // stream closed
                };
                let v: Value = match serde_json::from_str(&frame) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("method").and_then(Value::as_str) != Some("turn/start") {
                    continue;
                }
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                let text = v
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                turn_seq += 1;
                let turn_id = format!("turn-{turn_seq}");
                // Always OK-ack (codex acks even the silently-dropped turn).
                server
                    .send(Message::Text(
                        serde_json::to_string(&serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "turn": { "id": turn_id } }
                        }))
                        .unwrap(),
                    ))
                    .await
                    .ok();
                if active_until.is_some() {
                    // SILENT DROP — a turn is already active. No record, no
                    // lifecycle. (This is the codex behavior B1 guards.)
                    continue;
                }
                // This turn runs: record each coalesced line + emit
                // turn/started, and stay active for TURN_RUN.
                for line in text.split('\n') {
                    delivered.insert(line.to_string());
                }
                server
                    .send(Message::Text(
                        serde_json::to_string(&serde_json::json!({
                            "jsonrpc": "2.0", "method": "turn/started",
                            "params": { "threadId": "thread-test", "turn": { "id": turn_id } }
                        }))
                        .unwrap(),
                    ))
                    .await
                    .ok();
                active_until = Some(tokio::time::Instant::now() + TURN_RUN);
            }
            delivered
        });

        // Run the push/flush sequence under test.
        driver(Arc::clone(&handle)).await;

        // Let the consumer task flush any deferred (enqueued) observation on
        // the server's `turn/completed`. Poll until the queue drains or a
        // bounded budget elapses ("A then B across two cycles" settles here),
        // then a small grace so the final flush turn/start is recorded.
        let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if handle.queue.lock().await.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= drain_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
        drop(handle);

        tokio::time::timeout(Duration::from_secs(4), server_task)
            .await
            .expect("server task joins")
            .expect("server task ok")
    }

    /// B1 regression (DETERMINISTIC) — the canonical flush-vs-push interleave
    /// must NEVER drop an observation.
    ///
    /// Phase `TurnCompleted` with `[A]` queued; run `push_observation("B")`
    /// to completion, THEN `flush_push_queue`. This is the exact ordering the
    /// pre-fix code mishandled: pre-fix `push` flips `TurnRunning` and issues
    /// `turn/start("B")` WITHOUT draining the queue, so `A` is left for the
    /// flush, which then issues `turn/start("A")` while B's turn is active →
    /// codex silently drops it → **A is lost** (drained, OK-acked, never
    /// re-buffered). Post-fix the winning push drains `[A]` and appends `B`
    /// into ONE `turn/start("A\nB")`, and the flush sees `Issuing` and
    /// no-ops, so both survive. Deterministic: the ordering forces the bug,
    /// no scheduler luck required. (Verified to FAIL pre-fix / PASS post-fix.)
    #[tokio::test]
    async fn b1_push_then_flush_never_drops_observation() {
        let delivered = b1_collect_delivered(|handle| async move {
            // Winner push (drains [A] + appends B, post-fix). Pre-fix it
            // issues only "B", stranding A for the racy flush below.
            let _ = handle.push_observation(2, "B").await;
            // The flush that, pre-fix, issues the second (dropped) turn.
            flush_push_queue(
                handle.thread_id.as_deref().expect("fake handle thread id"),
                &handle.status,
                &handle.pusher().source,
                &handle.queue,
                &handle.watermark_sink,
                &handle.queue_persist,
            )
            .await;
        })
        .await;

        assert!(
            delivered.contains("A"),
            "observation A was DROPPED by the flush-vs-push race (B1) — delivered={delivered:?}"
        );
        assert!(
            delivered.contains("B"),
            "observation B was DROPPED (B1) — delivered={delivered:?}"
        );
    }

    /// B1 regression (CONCURRENT) — the same invariant under a genuinely
    /// concurrent drive: a `flush_push_queue` (what the consumer runs on
    /// `turn/completed`) and a dispatcher `push_observation` spawned and
    /// `join!`ed. Looped to shake out scheduling nondeterminism. Whichever
    /// issuer wins, neither A nor B may be lost (the loser only enqueues; the
    /// single winner coalesces / the deferred item flushes next cycle).
    #[tokio::test]
    async fn b1_concurrent_flush_and_push_never_drops_observation() {
        for iter in 0..40 {
            let delivered = b1_collect_delivered(|handle| async move {
                let h_flush = Arc::clone(&handle);
                let flush = tokio::spawn(async move {
                    flush_push_queue(
                        h_flush.thread_id.as_deref().expect("fake handle thread id"),
                        &h_flush.status,
                        &h_flush.pusher().source,
                        &h_flush.queue,
                        &h_flush.watermark_sink,
                        &h_flush.queue_persist,
                    )
                    .await;
                });
                let h_push = Arc::clone(&handle);
                let push = tokio::spawn(async move {
                    let _ = h_push.push_observation(2, "B").await;
                });
                let _ = tokio::join!(flush, push);
            })
            .await;

            assert!(
                delivered.contains("A"),
                "iter {iter}: observation A was DROPPED (B1 race) — delivered={delivered:?}"
            );
            assert!(
                delivered.contains("B"),
                "iter {iter}: observation B was DROPPED (B1 race) — delivered={delivered:?}"
            );
        }
    }

    #[tokio::test]
    async fn record_tracks_turn_lifecycle() {
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus::default()));

        record(
            &status,
            &Notification::TurnStarted {
                thread_id: "t1".into(),
                turn: serde_json::json!({ "id": "u1" }),
            },
        )
        .await;
        {
            let g = status.lock().await;
            assert_eq!(g.phase, SpecPushPhase::TurnRunning);
            assert_eq!(g.last_thread_id.as_deref(), Some("t1"));
            assert_eq!(g.last_turn_id.as_deref(), Some("u1"));
        }

        record(
            &status,
            &Notification::TurnCompleted {
                thread_id: "t1".into(),
                turn: serde_json::json!({ "id": "u1" }),
            },
        )
        .await;
        {
            let g = status.lock().await;
            assert_eq!(g.phase, SpecPushPhase::TurnCompleted);
        }
    }

    #[tokio::test]
    async fn record_does_not_reopen_wedged_phase() {
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            phase: SpecPushPhase::Wedged,
            last_thread_id: Some("t1".into()),
            last_turn_id: Some("u1".into()),
        }));

        record(
            &status,
            &Notification::TurnCompleted {
                thread_id: "t1".into(),
                turn: serde_json::json!({ "id": "u1", "status": "completed" }),
            },
        )
        .await;

        let g = status.lock().await;
        assert_eq!(g.phase, SpecPushPhase::Wedged);
        assert_eq!(g.last_turn_id.as_deref(), Some("u1"));
    }

    #[tokio::test]
    async fn watchdog_bail_marks_wedged_and_signals_recovery() {
        let (client, notifs, _server) = CodexAppServer::connect_pair_for_test().await;
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            phase: SpecPushPhase::TurnRunning,
            last_thread_id: Some("thread-test".into()),
            last_turn_id: Some("turn-wedged".into()),
        }));
        let (signal, mut rx) = recovery_signal_channel(WaveId::from("wave-test".to_string()));
        let mut consumer = NotificationConsumer {
            notifs,
            thread_id_slot: Arc::new(Mutex::new(Some("thread-test".to_string()))),
            status: status.clone(),
            source: SpecPusherSource::Legacy {
                client: Arc::new(client),
            },
            queue: Arc::new(Mutex::new(VecDeque::new())),
            watermark_sink: Arc::new(Mutex::new(None)),
            queue_persist: Arc::new(Mutex::new(None)),
            initial_prompt_ready_sink: Arc::new(Mutex::new(None)),
            watchdog: TurnWatchdogConfig::default(),
            recovery_signal: Some(signal),
            active_turn: Some(ActiveTurnWatchdog {
                turn_id: "turn-wedged".to_string(),
                deadline: TokioInstant::now(),
            }),
            initial_prompt_ready_attempted: false,
        };

        consumer
            .signal_process_recovery(
                "turn-wedged".to_string(),
                SpecRecoveryReason::InterruptedCompletionTimedOut,
            )
            .await;

        let req = rx.recv().await.expect("recovery request");
        assert_eq!(req.wave_id, WaveId::from("wave-test".to_string()));
        assert_eq!(req.thread_id, "thread-test");
        assert_eq!(req.turn_id, "turn-wedged");
        assert_eq!(
            req.reason,
            SpecRecoveryReason::InterruptedCompletionTimedOut
        );
        assert_eq!(status.lock().await.phase, SpecPushPhase::Wedged);
    }

    #[tokio::test]
    async fn watchdog_interrupt_race_accepts_natural_completion_without_recovery() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (client, notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
            phase: SpecPushPhase::TurnRunning,
            last_thread_id: Some("thread-test".into()),
            last_turn_id: Some("turn-race".into()),
        }));
        let (signal, mut rx) = recovery_signal_channel(WaveId::from("wave-test".to_string()));
        let mut consumer = NotificationConsumer {
            notifs,
            thread_id_slot: Arc::new(Mutex::new(Some("thread-test".to_string()))),
            status: status.clone(),
            source: SpecPusherSource::Legacy {
                client: Arc::new(client),
            },
            queue: Arc::new(Mutex::new(VecDeque::new())),
            watermark_sink: Arc::new(Mutex::new(None)),
            queue_persist: Arc::new(Mutex::new(None)),
            initial_prompt_ready_sink: Arc::new(Mutex::new(None)),
            watchdog: TurnWatchdogConfig {
                max_turn_duration: Duration::from_secs(30),
                interrupt_completion_budget: Duration::from_secs(5),
            },
            recovery_signal: Some(signal),
            active_turn: Some(ActiveTurnWatchdog {
                turn_id: "turn-race".to_string(),
                deadline: TokioInstant::now() + Duration::from_secs(30),
            }),
            initial_prompt_ready_attempted: false,
        };

        let server_task = tokio::spawn(async move {
            let frame = server
                .next()
                .await
                .expect("turn/interrupt frame")
                .expect("ws frame");
            let Message::Text(text) = frame else {
                panic!("expected text frame");
            };
            let req: Value = serde_json::from_str(&text).expect("interrupt json");
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("turn/interrupt")
            );
            let id = req.get("id").cloned().expect("request id");
            server
                .send(Message::Text(
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }).to_string(),
                ))
                .await
                .expect("send interrupt ack");
            server
                .send(Message::Text(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "threadId": "thread-test",
                            "turn": { "id": "turn-race", "status": "completed" }
                        }
                    })
                    .to_string(),
                ))
                .await
                .expect("send natural completion");
        });

        consumer
            .handle_watchdog_deadline("turn-race".to_string())
            .await;
        server_task.await.expect("server task");

        assert_eq!(status.lock().await.phase, SpecPushPhase::TurnCompleted);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "natural completion after interrupt ack must not signal process recovery"
        );
    }

    #[test]
    fn warn_on_approval_matches_request_shapes() {
        // These method names mirror the issue's approval/requestUserInput
        // server→client request shapes. `warn_on_approval` only logs, so
        // we assert the match predicate directly (no panic / no return
        // value) by reconstructing its condition.
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval",
            "item/tool/requestUserInput",
        ] {
            assert!(
                method.contains("requestApproval") || method.contains("requestUserInput"),
                "approval-shaped method should match the warn predicate: {method}"
            );
            // Exercise the function for coverage — it must not panic.
            warn_on_approval(&Notification::Item {
                method: method.to_string(),
                params: Value::Null,
            });
        }
        // A normal item must NOT match.
        let benign = "item/agentMessage/delta";
        assert!(!(benign.contains("requestApproval") || benign.contains("requestUserInput")));
    }

    /// B1 regression: a child spawned in its own process group is reaped by
    /// `kill(-pgid, …)`. This models the node-launcher/native-child shape
    /// with a `sh -c` launcher that itself spawns a long `sleep` grandchild
    /// in the SAME group; killing the group must reap BOTH (the bug was
    /// that a pid-only kill left the grandchild alive). We assert the
    /// grandchild is gone after a group SIGTERM.
    #[tokio::test]
    async fn kill_process_group_reaps_launcher_and_child() {
        use std::process::Stdio;
        // Launcher: print its child's pid, then exec a sleep so the
        // launcher itself stays alive too. The grandchild `sleep` inherits
        // the launcher's process group (we set `process_group(0)` on the
        // launcher, making it the group leader).
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 120 & echo $! ; wait")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(false)
            .spawn()
            .expect("spawn launcher");
        let pgid = i32::try_from(child.id().expect("launcher pid")).expect("pid fits i32");

        // Read the grandchild pid the launcher printed.
        use tokio::io::AsyncReadExt;
        let mut out = child.stdout.take().expect("stdout piped");
        let mut buf = Vec::new();
        // Bounded read: the launcher prints one line immediately.
        let grandchild_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut b = [0u8; 64];
                let n = out.read(&mut b).await.expect("read launcher stdout");
                if n == 0 {
                    panic!("launcher closed stdout before printing child pid");
                }
                buf.extend_from_slice(&b[..n]);
                if let Some(nl) = buf.iter().position(|&c| c == b'\n') {
                    let line = String::from_utf8_lossy(&buf[..nl]);
                    break line.trim().parse::<i32>().expect("grandchild pid");
                }
            }
        })
        .await
        .expect("timed out reading grandchild pid");

        // Sanity: the grandchild shares the launcher's process group.
        // (`/proc/<pid>/stat` field 5 is pgrp.)
        let stat = std::fs::read_to_string(format!("/proc/{grandchild_pid}/stat"))
            .expect("grandchild /proc/.../stat readable while alive");
        // Field layout: pid (comm) state ppid pgrp ... ; comm can contain
        // spaces/parens, so split on the last ')'.
        let after = stat.rsplit_once(')').expect("stat has comm").1;
        let pgrp: i32 = after
            .split_whitespace()
            .nth(2)
            .expect("pgrp field")
            .parse()
            .expect("pgrp int");
        assert_eq!(
            pgrp, pgid,
            "grandchild must share the launcher process group"
        );

        // The load-bearing reap: signal the whole group.
        assert!(
            signal_process_group(pgid, libc::SIGTERM),
            "group SIGTERM should reach at least one process"
        );

        // Both launcher and grandchild must die. Poll briefly.
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let mut gone = false;
        for _ in 0..50 {
            // kill(pid, 0) probes existence without signaling.
            let alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            gone,
            "grandchild pid {grandchild_pid} must be reaped by the group kill (pid-only kill would have leaked it)"
        );
    }

    /// Refuse to signal a non-positive process group (persistence-corruption
    /// guard) — `kill(-1)`/`kill(0)` would hit far too much.
    #[test]
    fn signal_process_group_refuses_non_positive() {
        assert!(!signal_process_group(0, libc::SIGTERM));
        assert!(!signal_process_group(-5, libc::SIGTERM));
        assert!(!signal_process_group(1, libc::SIGTERM));
    }

    /// S1 — the overall boot deadline must be at least as large as the
    /// single largest inner per-step budget, otherwise the inner safety
    /// nets would never get a chance to fire under their own budget and
    /// the overall cap alone would govern (defeating "keep the per-step
    /// budgets as inner safety nets"). The chosen cap (45 s) is the hard
    /// ceiling on the whole sequence; this pins the relationship so a
    /// future tweak to either constant is a conscious change.
    #[test]
    fn overall_boot_budget_caps_the_sequence() {
        // Sanity: the overall cap is the binding ceiling and is at least
        // the largest single step (so a healthy step never trips the
        // overall deadline before its own budget).
        assert!(OVERALL_BOOT_BUDGET >= SOCKET_READY_BUDGET);
        assert_eq!(OVERALL_BOOT_BUDGET, Duration::from_secs(45));
    }

    /// S1 — the rollback every fallible boot path relies on. When the boot
    /// sequence is aborted (a `?` early return OR the boot wedge backstop
    /// firing), the still-armed [`SpawnRollback`] guard must (a) reap the
    /// child's whole process GROUP via `kill(-pgid)` — so no orphan
    /// `codex app-server` is leaked — and (b) clean the per-card socket dir.
    /// This test models the teardown directly: spawn a group-leader child,
    /// drop an armed `SpawnRollback` pointed at its pgid + a socket inside a
    /// tempdir, and assert the child is gone and the socket dir removed.
    #[tokio::test]
    async fn spawn_rollback_reaps_group_and_cleans_socket_dir_on_drop() {
        // A real group-leader child (mirrors the production spawn shape:
        // `process_group(0)` → pgid == pid).
        let mut child = Command::new("sleep")
            .arg("120")
            .process_group(0)
            .kill_on_drop(false)
            .spawn()
            .expect("spawn group-leader child");
        let pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");

        // Per-card socket dir + a stale socket file inside it, as
        // `spawn_spec_appserver` would have created.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-xyz");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("app.sock");
        std::fs::write(&sock, b"").expect("touch sock file");

        // Arm the guard, then drop it — exactly what the overall-timeout
        // arm (and every `?` early return) does on the way out.
        {
            let _rollback = SpawnRollback::new(pid, &sock);
            // armed by default; dropping here fires the reap + cleanup.
        }

        // The guard's group SIGTERM terminates the child; `wait()` reaps
        // the zombie so the subsequent `kill(pid, 0)` existence probe sees
        // ESRCH rather than a still-addressable zombie. (Without the wait,
        // a terminated-but-unreaped child still answers signal-0 as alive.)
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let mut gone = false;
        for _ in 0..50 {
            // kill(pid, 0) probes existence without signaling.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            gone,
            "armed SpawnRollback drop must reap the child's process group (pid {pid})"
        );

        // The socket file + its now-empty per-card dir must be cleaned.
        assert!(
            !sock.exists(),
            "armed SpawnRollback drop must remove the listen socket file"
        );
        assert!(
            !sock_dir.exists(),
            "armed SpawnRollback drop must remove the now-empty per-card socket dir"
        );
    }

    /// S1 — a *disarmed* rollback (the success path, after the
    /// [`SpecPushHandle`] takes ownership of teardown) must NOT reap the
    /// group or touch the socket dir.
    #[tokio::test]
    async fn spawn_rollback_disarmed_is_a_noop() {
        let mut child = Command::new("sleep")
            .arg("120")
            .process_group(0)
            .kill_on_drop(true) // we own teardown here; ensure no leak.
            .spawn()
            .expect("spawn group-leader child");
        let pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");

        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-disarm");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("app.sock");
        std::fs::write(&sock, b"").expect("touch sock file");

        {
            let mut rollback = SpawnRollback::new(pid, &sock);
            rollback.disarm();
            // dropping a disarmed guard must do nothing.
        }

        // Child still alive (the handle, not the guard, would reap it).
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "disarmed SpawnRollback must NOT reap the group"
        );
        // Socket dir untouched.
        assert!(
            sock.exists(),
            "disarmed rollback must not remove the socket"
        );
        assert!(
            sock_dir.exists(),
            "disarmed rollback must not remove the socket dir"
        );

        // Teardown: kill the child ourselves (kill_on_drop also covers it).
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    /// #313 problem #1 round-3 (B1) — `socket_owned_by_appserver` returns
    /// `false` when the persisted socket *path does not exist* (graceful
    /// teardown wiped it, or host reboot lost the tmpfs entry). The
    /// caller must NOT signal the persisted pgid in this case; the
    /// persisted pgid is presumably recycled.
    #[tokio::test]
    async fn socket_ownership_probe_missing_path_returns_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp
            .path()
            .join("appserver")
            .join("missing-card")
            .join("sock");
        // Note: we don't create the parent dirs — the path is entirely absent.
        assert!(
            !sock.exists(),
            "test precondition: socket path should be absent"
        );
        let ok = socket_owned_by_appserver(&sock).await;
        assert!(
            !ok,
            "socket_owned_by_appserver must return false for an absent path \
             (kill of persisted pgid would be unsafe — could hit unrelated process)"
        );
    }

    /// #313 problem #1 round-3 (B1) — `socket_owned_by_appserver` returns
    /// `false` when the socket FILE exists but no process is bound
    /// (stale dirent from a crashed launcher). `UnixStream::connect`
    /// returns `ECONNREFUSED` in that case; we treat it the same as
    /// missing: skip the kill, just `cleanup_sock_dir` and respawn.
    #[tokio::test]
    async fn socket_ownership_probe_stale_dirent_returns_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-stale");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("sock");
        // Touch a regular file at the socket path — `UnixStream::connect`
        // on a non-socket file returns `ECONNREFUSED` (or similar
        // not-a-socket error mapped to `ConnectionRefused` on most
        // Unixes). Either way the probe must return false.
        std::fs::write(&sock, b"").expect("touch sock file");
        assert!(sock.exists(), "test precondition: socket path should exist");
        let ok = socket_owned_by_appserver(&sock).await;
        assert!(
            !ok,
            "socket_owned_by_appserver must return false for a stale dirent / \
             non-socket path (no live listener = no ownership = unsafe to kill)"
        );
    }

    /// #335 PR2 — a bare listener is not enough ownership evidence:
    /// takeover must complete a codex JSON-RPC `initialize` probe before
    /// it is allowed to kill the persisted process group.
    #[tokio::test]
    async fn socket_ownership_probe_non_jsonrpc_listener_returns_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-live");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("sock");
        let listener = tokio::net::UnixListener::bind(&sock).expect("bind listener");
        let accept_task = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        let ok = socket_owned_by_appserver(&sock).await;
        assert!(
            !ok,
            "socket_owned_by_appserver must return false for a listener that \
             cannot complete the codex initialize probe"
        );
        let _ = tokio::time::timeout(Duration::from_secs(1), accept_task).await;
    }

    /// #335 PR2 — positive takeover probe: a listener must accept the WS
    /// upgrade and answer JSON-RPC `initialize`.
    #[tokio::test]
    async fn socket_ownership_probe_initialize_listener_returns_true() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock_dir = tmp.path().join("appserver").join("card-jsonrpc");
        std::fs::create_dir_all(&sock_dir).expect("mkdir sock dir");
        let sock = sock_dir.join("sock");
        let listener = tokio::net::UnixListener::bind(&sock).expect("bind listener");
        let accept_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("ws accept");
            let (mut write, mut read) = futures_util::StreamExt::split(ws);
            if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) =
                futures_util::StreamExt::next(&mut read).await
            {
                let req: Value = serde_json::from_str(&text).expect("initialize json");
                let id = req.get("id").cloned().expect("id");
                let reply = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "userAgent": "fake-codex-app-server/0" }
                });
                futures_util::SinkExt::send(
                    &mut write,
                    tokio_tungstenite::tungstenite::Message::Text(reply.to_string()),
                )
                .await
                .expect("send initialize result");
            }
        });
        let ok = socket_owned_by_appserver(&sock).await;
        assert!(
            ok,
            "socket_owned_by_appserver must return true after initialize succeeds"
        );
        let _ = tokio::time::timeout(Duration::from_secs(1), accept_task).await;
    }

    /// #318 INV-4 (codex P1) — sanity on the budget constant: short enough
    /// that the boot-takeover catch-up delay is bounded, long enough that
    /// a healthy mid-turn server's in-flight `turn/completed` can land
    /// before we falsely promote and (potentially) silent-drop a flush.
    #[test]
    fn resumed_reconcile_budget_sane() {
        assert!(RESUMED_RECONCILE_BUDGET >= Duration::from_secs(1));
        assert!(RESUMED_RECONCILE_BUDGET <= Duration::from_secs(30));
        assert_eq!(RESUMED_RECONCILE_BUDGET, Duration::from_secs(5));
    }

    /// #318 INV-4 (codex P1) — idle-resume case: the reconcile timer fires
    /// (no `turn/started`/`turn/completed` ever arrives on the resumed
    /// stream because the prior boot's turn already completed before
    /// kernel crash), promotes `Resumed` -> `TurnCompleted`, and flushes a
    /// queued catch-up observation as a coalesced `turn/start`.
    ///
    /// Without this timer (pre-fix), the observation sits in the in-memory
    /// queue forever — codex never sends `turn/completed` on a truly-idle
    /// thread, so the consumer task's `flush_push_queue` never fires.
    ///
    /// Drives time with `tokio::time::pause()` / `advance` so the test is
    /// fully deterministic and doesn't depend on the wall-clock 5s budget.
    #[tokio::test(start_paused = true)]
    async fn resume_reconcile_flushes_queue_on_idle_resume() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (handle, mut server) = fake_handle().await;
        // Seed the post-`thread/resume` state: phase = Resumed, with a
        // catch-up observation already buffered (the dispatcher's
        // `decide(Resumed) == Enqueue` placed it here, awaiting a
        // lifecycle signal that, in the idle-resume case, never arrives).
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::Resumed;
            handle.queue.lock().await.push_back(QueuedObservation {
                envelope_id: 7,
                text: "catch-up obs".to_string(),
                db_id: None,
            });
        }

        // Spawn the reconciler the way `build_handle_after_spawn_resume`
        // does (via the extracted helper), with a tiny test budget so the
        // assertions about timing are explicit.
        let budget = Duration::from_secs(5);
        let reconciler = tokio::spawn(resume_reconcile_task(
            budget,
            handle.thread_id.clone().expect("fake handle thread id"),
            handle.status.clone(),
            handle.pusher().source,
            handle.queue.clone(),
            handle.watermark_sink.clone(),
            handle.queue_persist.clone(),
        ));

        // Before the budget elapses, NOTHING should happen — phase is
        // still Resumed and the queue is untouched. Advance just shy of
        // the budget and confirm.
        tokio::time::advance(budget - Duration::from_millis(50)).await;
        tokio::task::yield_now().await;
        assert_eq!(handle.status.lock().await.phase, SpecPushPhase::Resumed);
        assert_eq!(handle.queue.lock().await.len(), 1);

        // Advance past the budget — the timer fires, CAS-promotes to
        // TurnCompleted, then `flush_push_queue` claims `Issuing` and
        // issues a `turn/start` carrying the queued observation. Read it
        // off the wire to prove the flush actually happened.
        tokio::time::advance(Duration::from_millis(200)).await;

        // The flush will issue `turn/start` and await its response.
        // Resume real wall-clock so the WS round-trip doesn't deadlock on
        // paused time waiting for a frame that arrives via the IO runtime.
        tokio::time::resume();

        let req = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match server.next().await.expect("frame").expect("ws ok") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v.get("method").and_then(Value::as_str) == Some("turn/start") {
                            break v;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect("reconcile flush must issue a turn/start within budget");

        let input_text = req
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            input_text, "catch-up obs",
            "reconcile flush must deliver the queued catch-up observation"
        );

        // Answer the turn/start so the flush's await resolves cleanly and
        // the reconciler task completes (otherwise its outstanding request
        // would dangle on the fake server's WS pair).
        let id = req.get("id").cloned().unwrap();
        server
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "turn": { "id": "t-reconcile-1" } }
                }))
                .unwrap(),
            ))
            .await
            .unwrap();

        reconciler
            .await
            .expect("reconcile task must complete after issuing the flush");

        // Post-conditions: queue drained, phase rolled forward off Resumed.
        assert!(handle.queue.lock().await.is_empty());
        let phase = handle.status.lock().await.phase;
        // After a successful turn/start, phase is `Issuing` (waiting on
        // server's `turn/started` to reconcile to `TurnRunning`). The key
        // invariant: it is NO LONGER `Resumed`, so a later push will not
        // route to `Enqueue` again for the wrong reason.
        assert_ne!(
            phase,
            SpecPushPhase::Resumed,
            "phase must advance past Resumed after the reconcile flush"
        );
        let _server = server;
    }

    /// #318 INV-4 (codex P1) — race-window: if a real `turn/started` (or
    /// `turn/completed`) lands DURING the reconcile budget, the consumer
    /// task's `record()` advances the phase off `Resumed` and the timer's
    /// CAS must NO-OP (no spurious `turn/start` issued). This guards the
    /// "mid-turn resume that emits its `turn/completed` within budget"
    /// case — we must not double-issue when the natural lifecycle is
    /// about to drive the flush itself.
    #[tokio::test(start_paused = true)]
    async fn resume_reconcile_no_ops_when_notification_arrives_first() {
        let (handle, server) = fake_handle().await;
        {
            let mut g = handle.status.lock().await;
            g.phase = SpecPushPhase::Resumed;
            // Queue intentionally empty — this test only asserts the CAS
            // no-op behavior, not the flush body.
        }

        let budget = Duration::from_secs(5);
        let reconciler = tokio::spawn(resume_reconcile_task(
            budget,
            handle.thread_id.clone().expect("fake handle thread id"),
            handle.status.clone(),
            handle.pusher().source,
            handle.queue.clone(),
            handle.watermark_sink.clone(),
            handle.queue_persist.clone(),
        ));

        // Within the budget, simulate the consumer task seeing a real
        // `turn/started` -> `record()` flips phase to TurnRunning.
        tokio::time::advance(Duration::from_secs(1)).await;
        record(
            &handle.status,
            &Notification::TurnStarted {
                thread_id: handle.thread_id.clone().expect("fake handle thread id"),
                turn: serde_json::json!({ "id": "u-resume-1" }),
            },
        )
        .await;
        assert_eq!(handle.status.lock().await.phase, SpecPushPhase::TurnRunning);

        // Now advance past the budget. The timer fires, observes
        // `phase != Resumed`, and no-ops (does NOT issue a flush).
        tokio::time::advance(budget).await;
        reconciler
            .await
            .expect("reconcile task must complete (no-op branch)");

        // Phase must still be TurnRunning — the timer did not clobber it
        // back to TurnCompleted (which would be a correctness bug: a turn
        // really IS running on the server).
        assert_eq!(
            handle.status.lock().await.phase,
            SpecPushPhase::TurnRunning,
            "reconcile timer must not overwrite a real lifecycle-driven phase"
        );
        let _server = server;
    }
}

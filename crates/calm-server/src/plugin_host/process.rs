//! Child-process supervision for a single plugin.
//!
//! Each `PluginProcess` owns one running child plus its stderr capture task.
//! The supervisor task lives in `mod.rs` — this file only deals with
//! spawn / stop / stderr-tail; restart-backoff and crash-loop disabling are
//! the host's concern, not the process's.
//!
//! Design references:
//!   * §2.4 (shutdown ladder: notify → SIGTERM(grace) → SIGKILL)
//!   * §2.1 (install_path vs data_dir split — we cwd into data_dir)
//!   * §6   (NEIGE_PLUGIN_TOKEN / NEIGE_PLUGIN_ID env injection)

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use super::error::ProcessError;
use super::manifest::Manifest;

/// Stderr ring buffer cap. Design doc §2.4 says "8 KiB" but in practice an
/// entry-bounded ring (1024 lines, drop oldest) is friendlier — a single
/// pathological line can't blow past the cap. Each line is also clipped to
/// 4 KiB before insertion to bound the worst-case memory.
const STDERR_RING_CAP: usize = 1024;
const STDERR_LINE_CLAMP: usize = 4096;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct PluginProcess {
    /// Stable plugin id (matches `Manifest.id`). Cloned into log lines and
    /// the supervisor's `RunningPlugin` so we can correlate by id everywhere.
    pub id: String,

    /// The actual child. We wrap it in an `Option` + `Mutex` so `stop` can
    /// take ownership for the wait, while `is_alive` checks state without
    /// blocking on the same lock.
    child: Mutex<Option<Child>>,

    /// Held only long enough for the MCP client to take over; after that
    /// `take_stdio` returns these to the caller and the slots go `None`.
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<ChildStdout>>,

    /// Joinable task that drains stderr into `stderr_ring`. Cancelled on stop.
    stderr_task: Mutex<Option<JoinHandle<()>>>,
    stderr_ring: Arc<Mutex<VecDeque<String>>>,

    /// Wall-clock start time for `tracing` + crash-window bookkeeping.
    pub started_at: Instant,

    /// Cached PID for SIGTERM (we can't ask `Child` after `take()`).
    pid: Option<u32>,
}

impl PluginProcess {
    /// Spawn the manifest's entrypoint as a child process, wiring stdin/stdout
    /// for MCP framing and capturing stderr into a bounded ring buffer.
    ///
    /// `plugins_data_dir` is the per-plugin mutable-state root from §2.1 —
    /// we create `<plugins_data_dir>/<id>/` if it doesn't exist and cwd the
    /// child into it. The install dir (where `entrypoint.command` lives) is
    /// reached via `install_path`, which `PluginHost` resolves from the
    /// registry before calling us.
    pub fn spawn(
        manifest: &Manifest,
        install_path: &Path,
        plugins_data_dir: &Path,
        token: &str,
    ) -> Result<Self, ProcessError> {
        let plugin_data_dir = plugins_data_dir.join(&manifest.id);
        if !plugin_data_dir.exists() {
            std::fs::create_dir_all(&plugin_data_dir).map_err(ProcessError::Spawn)?;
        }

        // Resolve the entrypoint binary relative to install_path. Slice A's
        // manifest validator already rejected absolute paths and `..` escapes,
        // so a plain `join` is safe here.
        let bin = install_path.join(&manifest.entrypoint.command);

        let mut cmd = Command::new(&bin);
        cmd.args(&manifest.entrypoint.args)
            .current_dir(&plugin_data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Inherit PATH + other vars by default (don't `env_clear()`); the
            // manifest's env entries then layer on top. NEIGE_* are kernel-owned.
            .envs(&manifest.entrypoint.env)
            .env("NEIGE_PLUGIN_TOKEN", token)
            .env("NEIGE_PLUGIN_ID", &manifest.id)
            // Doc §6: we'll also redeliver the token over stdin in Slice H. For
            // Slice B, the env is sufficient.
            .env(
                "NEIGE_PLUGIN_DATA_DIR",
                plugin_data_dir.to_string_lossy().to_string(),
            );

        // kill_on_drop is important: if the host is dropped (panic in tests,
        // ctrl-c during dev) we'd rather SIGKILL the child than leave orphans.
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            tracing::error!(
                plugin_id = %manifest.id,
                bin = %bin.display(),
                error = %e,
                "plugin spawn failed",
            );
            ProcessError::Spawn(e)
        })?;

        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Stderr ring buffer + drainer task. We log every line at debug so
        // operators running with `RUST_LOG=calm_server=debug` get a live tail
        // alongside the in-memory snapshot.
        let stderr_ring: Arc<Mutex<VecDeque<String>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_RING_CAP)));
        let stderr_task =
            stderr.map(|s| spawn_stderr_drainer(manifest.id.clone(), s, stderr_ring.clone()));

        tracing::info!(
            plugin_id = %manifest.id,
            pid,
            cwd = %plugin_data_dir.display(),
            "plugin process spawned",
        );

        Ok(Self {
            id: manifest.id.clone(),
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            stderr_task: Mutex::new(stderr_task),
            stderr_ring,
            started_at: Instant::now(),
            pid,
        })
    }

    /// Take ownership of the stdin/stdout pair so the MCP client can drive
    /// them. Returns `None` if they were already taken (programming error in
    /// the supervisor).
    pub fn take_stdio(&self) -> Option<(ChildStdin, ChildStdout)> {
        let stdin = self.stdin.lock().unwrap().take()?;
        let stdout = self.stdout.lock().unwrap().take()?;
        Some((stdin, stdout))
    }

    /// PID of the spawned child. Stays `Some` for the lifetime of this
    /// `PluginProcess`, even after the supervisor task takes the `Child` via
    /// `take_child` — the OS pid stays meaningful for diagnostics until the
    /// kernel reaps it, and our drop guard ensures it goes away eventually.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// True if we still own a `Child` handle. Note this does NOT call
    /// `try_wait` — that's the supervisor's job. After `take_child` (which
    /// the supervisor does in normal startup), this returns false even
    /// though the kernel-level process may still be running; the supervisor
    /// task itself is the source of truth then.
    pub fn has_child_handle(&self) -> bool {
        self.child.lock().unwrap().is_some()
    }

    /// True if the kernel-level child has not yet been observed exited.
    /// Slice B's callers use this only for cosmetic status; the supervisor
    /// owns the canonical state. Implemented as "haven't seen `stop` consume
    /// the handle yet"; once `take_child` has run, we lose visibility and
    /// fall back to `true` (let the supervisor's state event drive UX).
    pub fn is_alive(&self) -> bool {
        // Either we still hold the Child, or the supervisor took it but
        // hasn't yet reported back via the host's status map.
        self.pid.is_some()
    }

    /// Snapshot the last `n` stderr lines (oldest → newest). Used by Slice D's
    /// `GET /api/plugins/:id/log` and by the supervisor when emitting a
    /// `Crashed{last_error}` state event.
    pub fn stderr_tail(&self, n: usize) -> Vec<String> {
        let ring = self.stderr_ring.lock().unwrap();
        let len = ring.len();
        let skip = len.saturating_sub(n);
        ring.iter().skip(skip).cloned().collect()
    }

    /// Take the underlying `Child` for the supervisor's `wait()` loop. After
    /// this, `is_alive` returns false and `stop` returns `AlreadyDead`.
    pub fn take_child(&self) -> Option<Child> {
        self.child.lock().unwrap().take()
    }

    /// Stop the child gracefully: SIGTERM, wait up to `grace`, then SIGKILL.
    /// Returns the exit status the kernel saw.
    ///
    /// This consumes the internal `Child` handle. Subsequent calls return
    /// `AlreadyDead`. The supervisor's wait task is expected to be cancelled
    /// or have already noticed the exit before calling `stop` — but we don't
    /// enforce that here; the worst case is a second `wait()` returning an
    /// `Io` error which we surface as `Wait`.
    pub async fn stop(&self, grace: Duration) -> Result<ExitStatus, ProcessError> {
        let mut child = match self.child.lock().unwrap().take() {
            Some(c) => c,
            None => return Err(ProcessError::AlreadyDead),
        };

        // 1. Polite SIGTERM. `tokio::process::Child::kill` is SIGKILL on unix
        //    which skips the grace period — so on unix we shell out to `nix`.
        send_sigterm(&child).ok();

        // 2. Wait for exit, up to `grace`.
        match tokio::time::timeout(grace, child.wait()).await {
            Ok(Ok(status)) => {
                self.cancel_stderr_task();
                tracing::info!(
                    plugin_id = %self.id,
                    status = ?status,
                    "plugin exited within grace after SIGTERM",
                );
                Ok(status)
            }
            Ok(Err(e)) => {
                self.cancel_stderr_task();
                Err(ProcessError::Wait(e))
            }
            Err(_grace_elapsed) => {
                // 3. SIGKILL fallback. `tokio` Child::kill on unix is SIGKILL.
                tracing::warn!(
                    plugin_id = %self.id,
                    grace_ms = grace.as_millis() as u64,
                    "plugin ignored SIGTERM; escalating to SIGKILL",
                );
                if let Err(e) = child.kill().await {
                    self.cancel_stderr_task();
                    return Err(ProcessError::Wait(e));
                }
                // After SIGKILL the kernel must reap within a small bound.
                // Cap at 2s so we don't hang the host forever on a zombie.
                match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                    Ok(Ok(status)) => {
                        self.cancel_stderr_task();
                        Ok(status)
                    }
                    Ok(Err(e)) => {
                        self.cancel_stderr_task();
                        Err(ProcessError::Wait(e))
                    }
                    Err(_) => {
                        self.cancel_stderr_task();
                        Err(ProcessError::KillTimeout)
                    }
                }
            }
        }
    }

    fn cancel_stderr_task(&self) {
        if let Some(task) = self.stderr_task.lock().unwrap().take() {
            task.abort();
        }
    }

    /// Convenience for tests + diagnostics.
    pub fn data_dir(plugins_data_dir: &Path, id: &str) -> PathBuf {
        plugins_data_dir.join(id)
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        // Belt-and-braces: tokio's `kill_on_drop` will SIGKILL the child if
        // we still hold a `Child`, but the stderr task is ours to clean up.
        if let Ok(mut slot) = self.stderr_task.lock()
            && let Some(t) = slot.take()
        {
            t.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn spawn_stderr_drainer(
    plugin_id: String,
    stderr: ChildStderr,
    ring: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(mut line)) => {
                    if line.len() > STDERR_LINE_CLAMP {
                        line.truncate(STDERR_LINE_CLAMP);
                    }
                    tracing::debug!(plugin_id = %plugin_id, "stderr: {line}");
                    let mut r = ring.lock().unwrap();
                    if r.len() == STDERR_RING_CAP {
                        r.pop_front();
                    }
                    r.push_back(line);
                }
                Ok(None) => {
                    // EOF — child closed its stderr (likely exited).
                    return;
                }
                Err(e) => {
                    tracing::warn!(plugin_id = %plugin_id, error = %e, "stderr read error");
                    return;
                }
            }
        }
    })
}

#[cfg(unix)]
fn send_sigterm(child: &Child) -> Result<(), std::io::Error> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    if let Some(raw_pid) = child.id() {
        // `Pid::from_raw` expects i32; tokio gives u32 (`pid_t` is i32 on unix).
        kill(Pid::from_raw(raw_pid as i32), Signal::SIGTERM)
            .map_err(|e| std::io::Error::other(format!("kill(SIGTERM) failed: {e}")))?;
    }
    Ok(())
}

// On non-unix targets we don't have SIGTERM; fall back to whatever `kill`
// does on that platform. Windows: `TerminateProcess`. No grace period there.
#[cfg(not(unix))]
fn send_sigterm(_child: &Child) -> Result<(), std::io::Error> {
    Ok(())
}

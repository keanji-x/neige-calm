//! Child-process supervision for a single plugin: spawn / stop / stderr-tail. Restart-backoff is the host's concern.

use std::collections::VecDeque;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use super::error::ProcessError;
use super::manifest::Manifest;

/// Entry-bounded stderr ring (drop oldest); each line is clipped to 4 KiB before insertion.
const STDERR_RING_CAP: usize = 1024;
const STDERR_LINE_CLAMP: usize = 4096;

pub struct PluginProcess {
    pub id: String,

    /// `Option` + `Mutex` so `stop` can take ownership for the wait.
    child: Mutex<Option<Child>>,

    /// Held only until `take_stdio` hands them to the MCP client.
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<ChildStdout>>,

    /// Joinable task that drains stderr into `stderr_ring`. Cancelled on stop.
    stderr_task: Mutex<Option<JoinHandle<()>>>,
    stderr_ring: Arc<Mutex<VecDeque<String>>>,

    /// Cached PID for SIGTERM (we can't ask `Child` after `take()`).
    pid: Option<u32>,
}

impl PluginProcess {
    /// Spawn the manifest's entrypoint, wiring stdin/stdout for MCP framing and capturing stderr into a bounded ring.
    /// The child is cwd'd into `<plugins_data_dir>/<id>/`, created if missing.
    pub fn spawn(
        manifest: &Manifest,
        install_path: &Path,
        plugins_data_dir: &Path,
        token: &str,
    ) -> Result<Self, ProcessError> {
        // `entrypoint` is required for `kind: app` and this is only reachable from the app spawn arm; fail loudly rather than unwrap.
        let entrypoint = manifest.entrypoint.as_ref().ok_or_else(|| {
            ProcessError::Spawn(std::io::Error::other(format!(
                "plugin `{}` has no `entrypoint` (kind `{}` has no supervised process)",
                manifest.id,
                manifest.kind.wire_name()
            )))
        })?;

        let plugin_data_dir = plugins_data_dir.join(&manifest.id);
        if !plugin_data_dir.exists() {
            std::fs::create_dir_all(&plugin_data_dir).map_err(ProcessError::Spawn)?;
        }

        // The manifest validator already rejected absolute paths and `..` escapes, so a plain `join` is safe.
        let bin = install_path.join(&entrypoint.command);

        let mut cmd = Command::new(&bin);
        cmd.args(&entrypoint.args)
            .current_dir(&plugin_data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Inherit PATH etc. (no `env_clear()`); NEIGE_* are kernel-owned.
            .envs(&entrypoint.env)
            .env("NEIGE_PLUGIN_TOKEN", token)
            .env("NEIGE_PLUGIN_ID", &manifest.id)
            .env(
                "NEIGE_PLUGIN_DATA_DIR",
                plugin_data_dir.to_string_lossy().to_string(),
            );

        // kill_on_drop: if the host is dropped we'd rather SIGKILL the child than leave orphans.
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

        // Every stderr line is also logged at debug for a live tail.
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
            pid,
        })
    }

    /// Returns `None` if the pair was already taken.
    pub fn take_stdio(&self) -> Option<(ChildStdin, ChildStdout)> {
        let stdin = self.stdin.lock().unwrap().take()?;
        let stdout = self.stdout.lock().unwrap().take()?;
        Some((stdin, stdout))
    }

    /// Stays `Some` even after `take_child`; the pid stays meaningful for diagnostics.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Snapshot the last `n` stderr lines (oldest → newest).
    pub fn stderr_tail(&self, n: usize) -> Vec<String> {
        let ring = self.stderr_ring.lock().unwrap();
        let len = ring.len();
        let skip = len.saturating_sub(n);
        ring.iter().skip(skip).cloned().collect()
    }

    /// After this, `stop` returns `AlreadyDead`.
    pub fn take_child(&self) -> Option<Child> {
        self.child.lock().unwrap().take()
    }

    /// SIGTERM, wait up to `grace`, then SIGKILL. Consumes the `Child`; later calls return `AlreadyDead`.
    pub async fn stop(&self, grace: Duration) -> Result<ExitStatus, ProcessError> {
        let mut child = match self.child.lock().unwrap().take() {
            Some(c) => c,
            None => return Err(ProcessError::AlreadyDead),
        };

        // `tokio::process::Child::kill` is SIGKILL on unix, which skips the grace period — so SIGTERM goes through `nix`.
        send_sigterm(&child).ok();

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
                tracing::warn!(
                    plugin_id = %self.id,
                    grace_ms = grace.as_millis() as u64,
                    "plugin ignored SIGTERM; escalating to SIGKILL",
                );
                if let Err(e) = child.kill().await {
                    self.cancel_stderr_task();
                    return Err(ProcessError::Wait(e));
                }
                // Cap the post-SIGKILL reap so a zombie can't hang the host.
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
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        // `kill_on_drop` handles the child; the stderr task is ours to clean up.
        if let Ok(mut slot) = self.stderr_task.lock()
            && let Some(t) = slot.take()
        {
            t.abort();
        }
    }
}

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

// Non-unix targets have no SIGTERM and no grace period.
#[cfg(not(unix))]
fn send_sigterm(_child: &Child) -> Result<(), std::io::Error> {
    Ok(())
}

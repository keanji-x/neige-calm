//! Stopping a terminal's child so the same terminal row can run a replacement (#2516).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_session::control::ProcSignal;
use tokio::time::{Instant, timeout};

use super::{TerminalRendererRegistry, signal_child_direct};
use crate::error::{CalmError, Result};

/// How long a child has to exit after SIGTERM before SIGKILL. A ceiling, not a sleep: the wait
/// ends at the first probe that sees the child gone.
const RESPAWN_TERM_GRACE: Duration = Duration::from_secs(3);
/// How long the group has to disappear after SIGKILL before the respawn is refused.
const RESPAWN_KILL_GRACE: Duration = Duration::from_secs(2);
/// The budget of one probe or signal exchange outside a grace: a wedged supervisor must not hold
/// the operation drive, which is serial.
const SUPERVISOR_EXCHANGE_BUDGET: Duration = Duration::from_secs(2);
const RESPAWN_PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// What one bounded probe saw.
enum Probe {
    Running,
    Gone,
    /// The probe failed (no supervisor listening there, a protocol error).
    Failed,
    /// The supervisor did not answer within the budget.
    Unanswered,
}

/// Held from a respawn's stop through its spawn: no renderer may be set up for the terminal in
/// that window but the respawn's own (`ensure_respawn`). A lazy reattach meanwhile is refused,
/// so no reader can attach to the dying child and end the replacement's runtime with its exit.
pub(crate) struct RespawnFence {
    registry: Arc<TerminalRendererRegistry>,
    terminal_id: String,
}

impl RespawnFence {
    pub(crate) fn check(&self, terminal_id: &str) -> anyhow::Result<()> {
        if self.terminal_id != terminal_id {
            anyhow::bail!(
                "respawn fence of terminal {} used for terminal {terminal_id}",
                self.terminal_id
            );
        }
        Ok(())
    }
}

impl Drop for RespawnFence {
    fn drop(&mut self) {
        if let Ok(mut fences) = self.registry.respawn_fences.lock() {
            fences.remove(&self.terminal_id);
        }
    }
}

pub(crate) fn fenced_error(terminal_id: &str) -> anyhow::Error {
    anyhow::anyhow!("terminal {terminal_id} is being respawned; no renderer is set up meanwhile")
}

pub(crate) fn stale_entry_error(terminal_id: &str) -> anyhow::Error {
    anyhow::anyhow!("a renderer still serves terminal {terminal_id}; the respawn does not reuse it")
}

impl TerminalRendererRegistry {
    /// Fence `terminal_id` for a respawn; refused while another respawn holds it.
    pub(crate) fn fence_respawn(self: &Arc<Self>, terminal_id: &str) -> Result<RespawnFence> {
        let mut fences = self
            .respawn_fences
            .lock()
            .map_err(|_| CalmError::Internal("respawn fence mutex poisoned".into()))?;
        if !fences.insert(terminal_id.to_owned()) {
            return Err(CalmError::Conflict(format!(
                "terminal {terminal_id} is already being respawned"
            )));
        }
        Ok(RespawnFence {
            registry: Arc::clone(self),
            terminal_id: terminal_id.to_owned(),
        })
    }

    pub(crate) fn respawn_fenced(&self, terminal_id: &str) -> bool {
        self.respawn_fences
            .lock()
            .map(|fences| fences.contains(terminal_id))
            .unwrap_or(true)
    }

    /// Stop the child of `terminal_id`, live or not and with or without a renderer entry:
    /// SIGTERM, a bounded wait, then SIGKILL to its process group. The entry is detached first,
    /// so its reader persists nothing more; a child that exits before that detach can still end
    /// only the runtime its reader was attached for (`ExitedRuntime::AttachedFor`), never the
    /// replacement's. Unlike `drop_entry_with_outcome`, which tears a renderer down and waits on
    /// its reader persisting the exit, this keeps the exit off the row its replacement reuses,
    /// covers a live child no entry holds (a server restart), and waits on the supervisor's
    /// probe. Every supervisor exchange is bounded: an unanswered probe refuses the respawn.
    pub(crate) async fn stop_for_respawn(
        &self,
        fence: &RespawnFence,
        configured_sock: Option<&Path>,
    ) -> Result<()> {
        let terminal_id = fence.terminal_id.as_str();
        let entry = self
            .entries
            .lock()
            .ok()
            .and_then(|mut entries| entries.remove(terminal_id));
        let sock = match &entry {
            Some(entry) => {
                entry.abort_tasks();
                entry.supervisor_sock.clone()
            }
            None => crate::proc_supervisor::resolve_control_sock(configured_sock).await?,
        };
        let proc_id = format!("term:{terminal_id}");
        // A failed probe (no supervisor listening) has no child to stop: the replacement's own
        // `EnsureProc` then returns a live child rather than start a second one.
        let was_running = match probe(&sock, terminal_id, SUPERVISOR_EXCHANGE_BUDGET).await {
            Probe::Running => true,
            Probe::Gone | Probe::Failed => false,
            Probe::Unanswered => return Err(unanswered(terminal_id)),
        };
        if was_running {
            tracing::info!(terminal_id, "stopping the live child before its respawn");
            signal(
                &sock,
                &proc_id,
                ProcSignal::Term,
                SUPERVISOR_EXCHANGE_BUDGET,
            )
            .await;
            if !child_gone_within(&sock, terminal_id, RESPAWN_TERM_GRACE).await {
                tracing::warn!(terminal_id, "child outlived SIGTERM before its respawn");
            }
        }
        if was_running || entry.is_some() {
            // As in `drop_entry`: unconditional, since the group signal is what reaps members that
            // outlived the leader.
            signal(
                &sock,
                &proc_id,
                ProcSignal::Kill,
                SUPERVISOR_EXCHANGE_BUDGET,
            )
            .await;
        }
        if was_running && !child_gone_within(&sock, terminal_id, RESPAWN_KILL_GRACE).await {
            return Err(CalmError::Internal(format!(
                "terminal {terminal_id} child is still running after SIGKILL; respawn refused"
            )));
        }
        Ok(())
    }

    /// Whether `terminal_id` has a running child now, by one bounded probe; `None` when the probe
    /// could not tell (it failed, or the supervisor did not answer).
    pub(crate) async fn child_running(
        &self,
        configured_sock: Option<&Path>,
        terminal_id: &str,
    ) -> Option<bool> {
        let sock = self
            .supervisor_sock_for(configured_sock, terminal_id)
            .await?;
        match probe(&sock, terminal_id, SUPERVISOR_EXCHANGE_BUDGET).await {
            Probe::Running => Some(true),
            Probe::Gone => Some(false),
            Probe::Failed | Probe::Unanswered => None,
        }
    }

    async fn supervisor_sock_for(
        &self,
        configured_sock: Option<&Path>,
        terminal_id: &str,
    ) -> Option<PathBuf> {
        if let Some(entry) = self.get(terminal_id) {
            return Some(entry.supervisor_sock.clone());
        }
        crate::proc_supervisor::resolve_control_sock(configured_sock)
            .await
            .ok()
    }

    /// Forget `terminal_id`'s renderer without touching its child, as a server restart does: the
    /// supervisor keeps the PTY and this process has no entry for it.
    #[cfg(feature = "fixtures")]
    pub fn forget_entry_for_test(&self, terminal_id: &str) {
        let entry = self
            .entries
            .lock()
            .ok()
            .and_then(|mut entries| entries.remove(terminal_id));
        if let Some(entry) = entry {
            entry.abort_tasks();
        }
    }
}

fn unanswered(terminal_id: &str) -> CalmError {
    CalmError::Internal(format!(
        "the proc supervisor did not answer a probe of terminal {terminal_id}; respawn refused"
    ))
}

async fn probe(sock: &Path, terminal_id: &str, budget: Duration) -> Probe {
    match timeout(
        budget,
        crate::probe_supervisor_for_terminal_at(Some(sock), terminal_id),
    )
    .await
    {
        Ok(Ok(true)) => Probe::Running,
        Ok(Ok(false)) => Probe::Gone,
        Ok(Err(error)) => {
            tracing::warn!(terminal_id, %error, "supervisor probe failed");
            Probe::Failed
        }
        Err(_) => Probe::Unanswered,
    }
}

async fn signal(sock: &Path, proc_id: &str, sig: ProcSignal, budget: Duration) {
    if timeout(budget, signal_child_direct(sock, proc_id, sig))
        .await
        .is_err()
    {
        tracing::warn!(proc_id, ?sig, "supervisor signal exchange timed out");
    }
}

/// Polls until the child is gone or `budget` is spent; each probe gets only what remains of it.
async fn child_gone_within(sock: &Path, terminal_id: &str, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        if matches!(
            probe(sock, terminal_id, remaining).await,
            Probe::Gone | Probe::Failed
        ) {
            return true;
        }
        tokio::time::sleep(RESPAWN_PROBE_INTERVAL.min(remaining)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A supervisor that accepts a connection and never answers must not hold a respawn, and with
    /// it the serial operation drive.
    #[tokio::test]
    async fn an_unanswering_supervisor_refuses_the_respawn_within_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("wedged.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let held = tokio::spawn(async move {
            let mut connections = Vec::new();
            while let Ok((connection, _)) = listener.accept().await {
                connections.push(connection);
            }
        });
        let registry = TerminalRendererRegistry::new();
        let fence = registry.fence_respawn("wedged").unwrap();

        let stopped = timeout(
            Duration::from_secs(60),
            registry.stop_for_respawn(&fence, Some(&sock)),
        )
        .await
        .expect("every supervisor exchange is bounded");

        assert!(stopped.is_err(), "an unanswered probe refuses the respawn");
        held.abort();
    }
}

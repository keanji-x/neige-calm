//! Stopping a terminal's child so the same terminal row can run a replacement (#2516).

use std::path::Path;
use std::time::Duration;

use calm_session::control::ProcSignal;

use super::{TerminalRendererRegistry, signal_child_direct};
use crate::error::{CalmError, Result};

/// How long a child has to exit after SIGTERM before SIGKILL. A ceiling, not a sleep: the wait
/// ends at the first probe that sees the child gone.
const RESPAWN_TERM_GRACE: Duration = Duration::from_secs(3);
/// How long the group has to disappear after SIGKILL before the respawn is refused.
const RESPAWN_KILL_GRACE: Duration = Duration::from_secs(2);
const RESPAWN_PROBE_INTERVAL: Duration = Duration::from_millis(50);

impl TerminalRendererRegistry {
    /// Stop the child of `terminal_id`, live or not and with or without a renderer entry:
    /// SIGTERM, a bounded wait, then SIGKILL to its process group. The entry is detached first,
    /// so the old child's exit is never written onto the row its replacement reuses and never
    /// ends the replacement's runtime. A supervisor that cannot be probed is logged and treated
    /// as holding no child: the replacement's own `EnsureProc` then returns a live child rather
    /// than start a second one.
    pub(crate) async fn stop_for_respawn(
        &self,
        configured_sock: Option<&Path>,
        terminal_id: &str,
    ) -> Result<()> {
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
        let was_running = child_running(&sock, terminal_id).await;
        if was_running {
            tracing::info!(terminal_id, "stopping the live child before its respawn");
            signal_child_direct(&sock, &proc_id, ProcSignal::Term).await;
            if !child_gone_within(&sock, terminal_id, RESPAWN_TERM_GRACE).await {
                tracing::warn!(terminal_id, "child outlived SIGTERM before its respawn");
            }
        }
        if was_running || entry.is_some() {
            // As in `drop_entry`: unconditional, since the group signal is what reaps members that
            // outlived the leader.
            signal_child_direct(&sock, &proc_id, ProcSignal::Kill).await;
        }
        if was_running && !child_gone_within(&sock, terminal_id, RESPAWN_KILL_GRACE).await {
            return Err(CalmError::Internal(format!(
                "terminal {terminal_id} child is still running after SIGKILL; respawn refused"
            )));
        }
        Ok(())
    }
}

async fn child_running(sock: &Path, terminal_id: &str) -> bool {
    match crate::probe_supervisor_for_terminal_at(Some(sock), terminal_id).await {
        Ok(running) => running,
        Err(error) => {
            tracing::warn!(
                terminal_id,
                %error,
                "supervisor probe failed before a respawn; treating the terminal as having no child"
            );
            false
        }
    }
}

async fn child_gone_within(sock: &Path, terminal_id: &str, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if !child_running(sock, terminal_id).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(RESPAWN_PROBE_INTERVAL).await;
    }
}

//! Historical exit evidence, separate from live renderer observations and input fences.
use super::*;

impl TerminalInteraction {
    /// An ended session can expose its recorded ANSI output without creating a renderer,
    /// connection, observation revision or input capability. Never reconstruct a live model
    /// from the lossy legacy serialization (`ModelView` requires the actual PTY stream).
    pub(super) async fn persisted_exit(
        &self,
        identity: &ToolCallIdentity,
        resolved: &target::Resolved,
        offset: usize,
        mode: WaitFor,
    ) -> Result<Option<Value>> {
        if resolved.session_active {
            return Ok(None);
        }
        let Some(term) = self
            .repo
            .terminal_get(&resolved.binding.terminal_id)
            .await?
        else {
            return Ok(None);
        };
        if term.exit_code.is_none() && !term.signal_killed {
            return Ok(None);
        }
        ensure!(
            offset == 0,
            "persisted exit output has no live viewport; scroll_offset must be 0"
        );
        let (output, truncated) = self
            .renderer
            .persisted_output(self.repo.as_ref(), &term)
            .await?;
        let current =
            Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, false).await?;
        ensure!(
            !current.session_active,
            "worker session changed while reading persisted exit output"
        );
        Ok(Some(json!({
            "source": "persisted_exit",
            "terminal_id": term.id,
            "card_id": current.binding.card_id,
            "worker_session_id": current.binding.worker_session_id,
            "task": current.binding.task,
            "task_status": current.task_status,
            "controllable": false,
            "exited": true,
            "exit_code": term.exit_code,
            "signal_killed": term.signal_killed,
            "pty_output": output,
            "pty_output_truncated": truncated,
            "wait": {"mode": mode.name(), "outcome": "exited", "waited_ms": 0}
        })))
    }
}

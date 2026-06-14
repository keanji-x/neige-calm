use std::path::PathBuf;

use async_trait::async_trait;
use calm_exec::{SpawnCtx, WorkerProvider};
use calm_types::error::CoreError;
use calm_types::worker::{ExitEvidence, ExitInterpretation, Liveness, SessionMode, WorkerSession};

use super::supervisor::probe_terminal_liveness;
use super::terminal::ephemeral_interpret_exit;

#[derive(Clone, Debug)]
pub struct ClaudeProvider {
    supervisor_sock: PathBuf,
}

impl ClaudeProvider {
    pub fn new(supervisor_sock: impl Into<PathBuf>) -> Self {
        Self {
            supervisor_sock: supervisor_sock.into(),
        }
    }
}

#[async_trait]
impl WorkerProvider for ClaudeProvider {
    fn kind(&self) -> &'static str {
        "claude"
    }

    fn session_mode(&self) -> SessionMode {
        SessionMode::Ephemeral
    }

    async fn probe_liveness(
        &self,
        session: &WorkerSession,
        ctx: &SpawnCtx,
    ) -> Result<Liveness, CoreError> {
        Ok(probe_terminal_liveness(
            &self.supervisor_sock,
            session.terminal_run_id.as_deref(),
            ctx.now_ms,
        )
        .await)
    }

    async fn interpret_exit(
        &self,
        _session: &WorkerSession,
        evidence: &ExitEvidence,
        _ctx: &SpawnCtx,
    ) -> Result<ExitInterpretation, CoreError> {
        Ok(claude_interpret_exit(evidence))
    }
}

pub(crate) fn claude_interpret_exit(evidence: &ExitEvidence) -> ExitInterpretation {
    ephemeral_interpret_exit("claude", evidence)
}

#[cfg(test)]
mod tests {
    use calm_types::worker::{ExitEvidence, ExitSource};

    use super::*;

    fn evidence(exit_code: Option<i32>, signal_killed: bool) -> ExitEvidence {
        ExitEvidence {
            exit_code,
            signal_killed,
            observed_at_ms: 1,
            source: ExitSource::AttachReader,
        }
    }

    #[test]
    fn claude_exit_zero_completes() {
        assert_eq!(
            claude_interpret_exit(&evidence(Some(0), false)),
            ExitInterpretation::Completed
        );
    }

    #[test]
    fn claude_signal_fails() {
        assert_eq!(
            claude_interpret_exit(&evidence(None, true)),
            ExitInterpretation::Failed {
                reason: "claude worker was signal-killed".into()
            }
        );
    }

    #[test]
    fn claude_nonzero_fails() {
        assert_eq!(
            claude_interpret_exit(&evidence(Some(2), false)),
            ExitInterpretation::Failed {
                reason: "claude worker exited with code 2".into()
            }
        );
    }
}

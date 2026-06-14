use std::path::PathBuf;

use async_trait::async_trait;
use calm_exec::{SpawnCtx, WorkerProvider};
use calm_types::error::CoreError;
use calm_types::worker::{
    ExitEvidence, ExitInterpretation, ExitSource, Liveness, SessionMode, WorkerSession,
};

use super::supervisor::probe_terminal_liveness;

#[derive(Clone, Debug)]
pub struct TerminalProvider {
    supervisor_sock: PathBuf,
}

impl TerminalProvider {
    pub fn new(supervisor_sock: impl Into<PathBuf>) -> Self {
        Self {
            supervisor_sock: supervisor_sock.into(),
        }
    }
}

#[async_trait]
impl WorkerProvider for TerminalProvider {
    fn kind(&self) -> &'static str {
        "terminal"
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
        Ok(terminal_interpret_exit(evidence))
    }
}

pub(crate) fn terminal_interpret_exit(evidence: &ExitEvidence) -> ExitInterpretation {
    ephemeral_interpret_exit("terminal", evidence)
}

pub(crate) fn ephemeral_interpret_exit(kind: &str, evidence: &ExitEvidence) -> ExitInterpretation {
    if evidence.signal_killed {
        return ExitInterpretation::Failed {
            reason: format!("{kind} worker was signal-killed"),
        };
    }
    if evidence.exit_code == Some(0) {
        return ExitInterpretation::Completed;
    }
    ExitInterpretation::Failed {
        reason: failed_reason(kind, evidence),
    }
}

pub(crate) fn failed_reason(kind: &str, evidence: &ExitEvidence) -> String {
    if evidence.source == ExitSource::Probe {
        return format!("{kind} worker exited (outcome unknown; observed via supervisor probe)");
    }
    match (evidence.exit_code, evidence.signal_killed) {
        (_, true) => format!("{kind} worker was signal-killed"),
        (Some(code), false) => format!("{kind} worker exited with code {code}"),
        (None, false) => format!("{kind} worker exited without a code"),
    }
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
    fn terminal_exit_zero_completes() {
        assert_eq!(
            terminal_interpret_exit(&evidence(Some(0), false)),
            ExitInterpretation::Completed
        );
    }

    #[test]
    fn terminal_signal_fails() {
        assert_eq!(
            terminal_interpret_exit(&evidence(None, true)),
            ExitInterpretation::Failed {
                reason: "terminal worker was signal-killed".into()
            }
        );
    }

    #[test]
    fn terminal_nonzero_fails() {
        assert_eq!(
            terminal_interpret_exit(&evidence(Some(2), false)),
            ExitInterpretation::Failed {
                reason: "terminal worker exited with code 2".into()
            }
        );
    }

    #[test]
    fn failed_reason_probe_source_hides_exit_code_sentinel() {
        let reason = failed_reason(
            "terminal",
            &ExitEvidence {
                exit_code: Some(-1),
                signal_killed: false,
                observed_at_ms: 1,
                source: ExitSource::Probe,
            },
        );

        assert!(!reason.contains("-1"));
        assert!(reason.contains("outcome unknown"));
        assert!(reason.contains("supervisor probe"));
    }
}

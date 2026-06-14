use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use calm_exec::{SpawnCtx, SpawnHandle, WorkerProvider};
use calm_types::error::CoreError;
use calm_types::runtime::TimestampMs;
use calm_types::worker::{ExitEvidence, ExitInterpretation, Liveness, SessionMode, WorkerSession};

use super::supervisor::probe_terminal_liveness;
use super::terminal::failed_reason;

pub trait CodexDaemonProbe: Send + Sync {
    fn is_running(&self) -> bool;
    fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<String>;
    fn remote_uri(&self) -> String;
}

#[derive(Clone)]
pub struct CodexProvider {
    supervisor_sock: PathBuf,
    daemon: Arc<dyn CodexDaemonProbe>,
}

impl CodexProvider {
    pub fn new(supervisor_sock: impl Into<PathBuf>, daemon: Arc<dyn CodexDaemonProbe>) -> Self {
        Self {
            supervisor_sock: supervisor_sock.into(),
            daemon,
        }
    }
}

#[async_trait]
impl WorkerProvider for CodexProvider {
    fn kind(&self) -> &'static str {
        "codex"
    }

    fn session_mode(&self) -> SessionMode {
        SessionMode::Resumable
    }

    async fn probe_liveness(
        &self,
        session: &WorkerSession,
        ctx: &SpawnCtx,
    ) -> Result<Liveness, CoreError> {
        let pty = probe_terminal_liveness(
            &self.supervisor_sock,
            session.terminal_run_id.as_deref(),
            ctx.now_ms,
        )
        .await;
        let daemon_running = self.daemon.is_running();
        let active_turn_id = session
            .thread_id
            .as_deref()
            .and_then(|thread_id| self.daemon.active_turn_id_for_thread(thread_id));
        Ok(codex_liveness_from_evidence(
            pty,
            daemon_running,
            active_turn_id,
            ctx.now_ms,
        ))
    }

    async fn interpret_exit(
        &self,
        _session: &WorkerSession,
        evidence: &ExitEvidence,
        _ctx: &SpawnCtx,
    ) -> Result<ExitInterpretation, CoreError> {
        Ok(codex_interpret_exit(evidence))
    }

    /// PR8 will wire the returned command into the terminal/renderer spawn path.
    async fn resume(
        &self,
        session: &WorkerSession,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnHandle, CoreError> {
        let thread_id = session
            .thread_id
            .as_deref()
            .ok_or_else(|| CoreError::Internal("codex resume requires session.thread_id".into()))?;
        let command_line = resume_command(thread_id, &self.daemon.remote_uri());
        tracing::debug!(command_line, "codex resume command prepared");
        Err(CoreError::Internal(
            "codex resume spawn wiring lands with the reaper (#679 PR8)".into(),
        ))
    }
}

pub(crate) fn codex_liveness_from_evidence(
    pty: Liveness,
    daemon_running: bool,
    active_turn_id: Option<String>,
    now_ms: TimestampMs,
) -> Liveness {
    match pty {
        Liveness::Exited { evidence } => Liveness::Exited { evidence },
        _ => {
            // Codex is resumable: PTY Unknown means no terminal/probe failed, so daemon evidence is authoritative unless PTY proves Exited.
            if !daemon_running {
                return Liveness::Unknown { since_ms: now_ms };
            }
            if let Some(active_turn_id) = active_turn_id {
                Liveness::Alive {
                    active_turn_id: Some(active_turn_id),
                }
            } else {
                Liveness::Idle
            }
        }
    }
}

pub(crate) fn codex_interpret_exit(evidence: &ExitEvidence) -> ExitInterpretation {
    if evidence.exit_code == Some(0) && !evidence.signal_killed {
        return ExitInterpretation::Completed;
    }
    // Deliberately refines worker_cleanup::worker_spawn_failure_preserved: exit 0 completes, signals PreserveCard, non-zero fails.
    if evidence.signal_killed {
        return ExitInterpretation::PreserveCard;
    }
    ExitInterpretation::Failed {
        reason: failed_reason("codex", evidence),
    }
}

pub fn resume_command(thread_id: &str, remote_uri: &str) -> String {
    format!(
        "codex resume {} --remote {}",
        shell_single_quote(thread_id),
        shell_single_quote(remote_uri)
    )
}

fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
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
    fn codex_liveness_alive_with_turn() {
        assert_eq!(
            codex_liveness_from_evidence(
                Liveness::Alive {
                    active_turn_id: None
                },
                true,
                Some("turn-1".into()),
                7,
            ),
            Liveness::Alive {
                active_turn_id: Some("turn-1".into())
            }
        );
    }

    #[test]
    fn codex_liveness_idle_without_turn() {
        assert_eq!(
            codex_liveness_from_evidence(
                Liveness::Alive {
                    active_turn_id: None
                },
                true,
                None,
                7,
            ),
            Liveness::Idle
        );
    }

    #[test]
    fn codex_liveness_preserves_pty_exited() {
        let evidence = evidence(Some(-1), false);
        assert_eq!(
            codex_liveness_from_evidence(
                Liveness::Exited {
                    evidence: evidence.clone()
                },
                false,
                Some("turn-1".into()),
                7,
            ),
            Liveness::Exited { evidence }
        );
    }

    #[test]
    fn codex_liveness_daemon_not_running_is_unknown() {
        assert_eq!(
            codex_liveness_from_evidence(
                Liveness::Alive {
                    active_turn_id: None
                },
                false,
                Some("turn-1".into()),
                7,
            ),
            Liveness::Unknown { since_ms: 7 }
        );
    }

    #[test]
    fn codex_liveness_pty_unknown_uses_daemon_turn() {
        assert_eq!(
            codex_liveness_from_evidence(
                Liveness::Unknown { since_ms: 3 },
                true,
                Some("turn-1".into()),
                7,
            ),
            Liveness::Alive {
                active_turn_id: Some("turn-1".into())
            }
        );
    }

    #[test]
    fn codex_liveness_pty_unknown_uses_daemon_idle() {
        assert_eq!(
            codex_liveness_from_evidence(Liveness::Unknown { since_ms: 3 }, true, None, 7),
            Liveness::Idle
        );
    }

    #[test]
    fn codex_liveness_pty_unknown_and_daemon_down_is_unknown_since_now() {
        assert_eq!(
            codex_liveness_from_evidence(
                Liveness::Unknown { since_ms: 3 },
                false,
                Some("turn-1".into()),
                7,
            ),
            Liveness::Unknown { since_ms: 7 }
        );
    }

    #[test]
    fn codex_exit_zero_completes() {
        assert_eq!(
            codex_interpret_exit(&evidence(Some(0), false)),
            ExitInterpretation::Completed
        );
    }

    #[test]
    fn codex_signal_preserves_card() {
        assert_eq!(
            codex_interpret_exit(&evidence(None, true)),
            ExitInterpretation::PreserveCard
        );
    }

    #[test]
    fn codex_nonzero_fails() {
        assert_eq!(
            codex_interpret_exit(&evidence(Some(2), false)),
            ExitInterpretation::Failed {
                reason: "codex worker exited with code 2".into()
            }
        );
    }

    #[test]
    fn resume_command_quotes_safe_inputs_like_existing_adapter() {
        assert_eq!(
            resume_command("t-1", "ws://x"),
            "codex resume 't-1' --remote 'ws://x'"
        );
    }

    #[test]
    fn resume_command_quotes_embedded_single_quotes() {
        assert_eq!(
            resume_command("t'1", "unix:///tmp/codex'sock"),
            "codex resume 't'\\''1' --remote 'unix:///tmp/codex'\\''sock'"
        );
    }
}

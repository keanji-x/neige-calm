use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use calm_exec::{SpawnCtx, SpawnHandle, WorkerProvider};
use calm_types::error::CoreError;
use calm_types::runtime::TimestampMs;
use calm_types::worker::{
    DeathVerdict, ExitEvidence, ExitInterpretation, Liveness, SessionMode, WorkerSession,
};

use super::supervisor::probe_terminal_liveness;
use super::terminal::failed_reason;

/// calm-provider-local mirror of the wire `ThreadStatus` (#741 §1.3), keyed
/// to exactly what the death arbiter discriminates on. The daemon-probe impl
/// (calm-server) maps the upstream `thread/read` status into this; the
/// arbiter only ever asks "is it `Active`?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatusLite {
    NotLoaded,
    Idle,
    SystemError,
    /// A turn is running or blocked on a human (`waitingOnUserInput` /
    /// `waitingOnApproval`). Either flag ⇒ never reap (design §1.4).
    Active {
        waiting_on_user_input: bool,
        waiting_on_approval: bool,
    },
}

/// The liveness facts the death arbiter needs from a live `thread/read`
/// (#741 §1.3). Returned by [`CodexDaemonProbe::read_liveness_facts`];
/// `None` from that call means the RPC was unreachable (can't rule out a
/// live turn → `Unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexLivenessFacts {
    /// Whether the thread is currently loaded in daemon memory
    /// (`thread/loaded/list`). Secondary signal; the arbiter keys on
    /// `status` + `last_turn_completed_at`.
    pub loaded: bool,
    /// The thread's current status.
    pub status: ThreadStatusLite,
    /// The MOST RECENT turn's `completed_at`, design §0.1:
    /// * `None`            — no turns at all (no positive signal).
    /// * `Some(None)`      — last turn started but never finished
    ///   (died-mid-turn — the positive death signal).
    /// * `Some(Some(ts))`  — last turn finished cleanly OR was deliberately
    ///   aborted (both carry a timestamp) — NOT a death.
    pub last_turn_completed_at: Option<Option<i64>>,
}

#[async_trait]
pub trait CodexDaemonProbe: Send + Sync {
    fn is_running(&self) -> bool;
    fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<String>;
    fn remote_uri(&self) -> String;

    /// Wall-clock ms of the most recent successful daemon (re)connect (#741
    /// §1.3). Feeds the 741-3 reaper's `REBUILD_GRACE` — within the grace the
    /// loaded-thread roster isn't stable yet so S2 pulls are held off. Nothing
    /// consumes this until 741-3; it is tracked always-on in the connect path.
    fn daemon_connected_at_ms(&self) -> TimestampMs;

    /// Pull the §1.3 liveness facts for `thread_id` via a live `thread/read`
    /// (+ optional `thread/loaded/list`). Returns `None` on ANY RPC
    /// error / unreachable daemon — the arbiter treats that as "can't rule
    /// out a live turn" → `Unknown`.
    async fn read_liveness_facts(&self, thread_id: &str) -> Option<CodexLivenessFacts>;
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

    /// The codex worker death-arbiter (#741 §1.1 truth table). DORMANT:
    /// nothing calls this outside tests until 741-3 wires it into the reaper.
    async fn confirm_durable_death(
        &self,
        thread_id: &str,
        now_ms: TimestampMs,
        daemon_connected_at_ms: TimestampMs,
        rebuild_grace_ms: i64,
    ) -> DeathVerdict {
        // S1: daemon down — brain gone, no turn can run, nothing resumes.
        if !self.daemon.is_running() {
            return DeathVerdict::Dead;
        }
        // Within rebuild grace: the loaded-thread roster isn't stable yet.
        if now_ms - daemon_connected_at_ms < rebuild_grace_ms {
            return DeathVerdict::Unknown;
        }
        // S2: live, past-grace daemon — pull facts and apply the table.
        let facts = self.daemon.read_liveness_facts(thread_id).await;
        verdict_from_facts(facts)
    }

    fn daemon_connected_at_ms(&self) -> Option<TimestampMs> {
        Some(self.daemon.daemon_connected_at_ms())
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

/// The S2 leaf of the §1.1 truth table: given the facts pulled from a live,
/// past-grace daemon (or `None` if the pull was unreachable), decide the
/// verdict. Factored out so the truth table is unit-testable without a
/// daemon. The S1 (daemon-down) and within-grace branches live in
/// [`CodexProvider::confirm_durable_death`] above.
pub(crate) fn verdict_from_facts(facts: Option<CodexLivenessFacts>) -> DeathVerdict {
    let Some(facts) = facts else {
        // RPC unreachable — can't rule out a live turn.
        return DeathVerdict::Unknown;
    };
    // A turn is running OR blocked on a human ⇒ never reap (idle-worker
    // guard, §1.4). ANY flag still counts as Active.
    if matches!(facts.status, ThreadStatusLite::Active { .. }) {
        return DeathVerdict::Alive;
    }
    // Idle | SystemError | NotLoaded: no turn running. The DISCRIMINATOR is
    // the last turn's completed_at, NOT the status (§0.1).
    match facts.last_turn_completed_at {
        // S2: died-mid-turn — started, never finished, won't re-drive.
        Some(None) => DeathVerdict::Dead,
        // Finished cleanly OR deliberately aborted (both carry a ts) — not a death.
        Some(Some(_)) => DeathVerdict::Alive,
        // No turns at all — no positive signal → conservative no-reap.
        None => DeathVerdict::Unknown,
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

    // ===================================================================
    // confirm_durable_death — the #741 §1.1 truth table (the spec).
    // ===================================================================

    /// Scriptable [`CodexDaemonProbe`]: returns a fixed `is_running` and a
    /// fixed `read_liveness_facts` result, so each truth-table row can be
    /// driven through the real `CodexProvider::confirm_durable_death`.
    struct FakeCodexDaemonProbe {
        running: bool,
        facts: Option<CodexLivenessFacts>,
    }

    #[async_trait]
    impl CodexDaemonProbe for FakeCodexDaemonProbe {
        fn is_running(&self) -> bool {
            self.running
        }
        fn active_turn_id_for_thread(&self, _thread_id: &str) -> Option<String> {
            None
        }
        fn remote_uri(&self) -> String {
            "unix:///tmp/fake.sock".into()
        }
        fn daemon_connected_at_ms(&self) -> TimestampMs {
            0
        }
        async fn read_liveness_facts(&self, _thread_id: &str) -> Option<CodexLivenessFacts> {
            self.facts
        }
    }

    fn provider_with(running: bool, facts: Option<CodexLivenessFacts>) -> CodexProvider {
        CodexProvider::new(
            "/tmp/fake-supervisor.sock",
            Arc::new(FakeCodexDaemonProbe { running, facts }),
        )
    }

    fn facts(status: ThreadStatusLite, last: Option<Option<i64>>) -> CodexLivenessFacts {
        CodexLivenessFacts {
            loaded: true,
            status,
            last_turn_completed_at: last,
        }
    }

    const GRACE: i64 = 300_000; // 5 min, design D-2.

    /// Helper: now well past grace so S2 is reached (when daemon up).
    async fn verdict(running: bool, facts: Option<CodexLivenessFacts>) -> DeathVerdict {
        provider_with(running, facts)
            .confirm_durable_death("t-1", 1_000_000, 0, GRACE)
            .await
    }

    #[tokio::test]
    async fn arbiter_s1_daemon_down_is_dead() {
        // is_running()==false ⇒ Dead, regardless of facts (no pull needed).
        assert_eq!(verdict(false, None).await, DeathVerdict::Dead);
        assert_eq!(
            verdict(false, Some(facts(ThreadStatusLite::Idle, Some(None)))).await,
            DeathVerdict::Dead
        );
    }

    #[tokio::test]
    async fn arbiter_within_rebuild_grace_is_unknown() {
        // now - connected < grace ⇒ Unknown even with a died-mid-turn fact.
        let v = provider_with(true, Some(facts(ThreadStatusLite::Idle, Some(None))))
            .confirm_durable_death("t-1", GRACE - 1, 0, GRACE)
            .await;
        assert_eq!(v, DeathVerdict::Unknown);
    }

    #[tokio::test]
    async fn arbiter_grace_boundary_equal_is_not_within_grace() {
        // now - connected == grace ⇒ NOT within grace → reaches S2.
        // Pull shows died-mid-turn ⇒ Dead (proves we crossed the boundary).
        let v = provider_with(true, Some(facts(ThreadStatusLite::Idle, Some(None))))
            .confirm_durable_death("t-1", GRACE, 0, GRACE)
            .await;
        assert_eq!(v, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_pull_unreachable_is_unknown() {
        // daemon up, past grace, read_liveness_facts == None ⇒ Unknown.
        assert_eq!(verdict(true, None).await, DeathVerdict::Unknown);
    }

    #[tokio::test]
    async fn arbiter_active_no_flags_is_alive() {
        let f = facts(
            ThreadStatusLite::Active {
                waiting_on_user_input: false,
                waiting_on_approval: false,
            },
            // even with a non-completed last turn, Active wins.
            Some(None),
        );
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Alive);
    }

    #[tokio::test]
    async fn arbiter_active_waiting_on_user_input_is_alive() {
        let f = facts(
            ThreadStatusLite::Active {
                waiting_on_user_input: true,
                waiting_on_approval: false,
            },
            Some(None),
        );
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Alive);
    }

    #[tokio::test]
    async fn arbiter_active_waiting_on_approval_is_alive() {
        let f = facts(
            ThreadStatusLite::Active {
                waiting_on_user_input: false,
                waiting_on_approval: true,
            },
            Some(None),
        );
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Alive);
    }

    #[tokio::test]
    async fn arbiter_idle_died_mid_turn_is_dead() {
        // Idle + last turn Some(None) (started, never finished) ⇒ S2 Dead.
        let f = facts(ThreadStatusLite::Idle, Some(None));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_idle_last_turn_completed_is_alive() {
        // Idle + last turn Some(Some(ts)) — covers BOTH clean-complete AND
        // deliberate-abort (a TurnAborted carries a timestamp) ⇒ Alive.
        let f = facts(ThreadStatusLite::Idle, Some(Some(1700)));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Alive);
    }

    #[tokio::test]
    async fn arbiter_not_loaded_died_mid_turn_is_dead() {
        // NotLoaded + last turn Some(None) ⇒ Dead (read-from-disk rollout).
        let f = facts(ThreadStatusLite::NotLoaded, Some(None));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_system_error_died_mid_turn_is_dead() {
        // SystemError is just "no turn running" — discriminator is still
        // completed_at; Some(None) ⇒ Dead.
        let f = facts(ThreadStatusLite::SystemError, Some(None));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_no_turns_at_all_is_unknown() {
        // last_turn_completed_at == None (no turns) ⇒ conservative no-reap.
        let f = facts(ThreadStatusLite::Idle, None);
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Unknown);
    }

    #[test]
    fn arbiter_default_for_non_codex_providers_is_unknown() {
        // The WorkerProvider trait default returns Unknown — exercised here
        // via the pure S2 leaf with no facts (the same conservative output).
        assert_eq!(verdict_from_facts(None), DeathVerdict::Unknown);
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

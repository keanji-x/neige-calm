use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use calm_exec::{BoundKeys, MessageReady, SpawnCtx, TuiInput, WorkerProvider};
use calm_types::error::CoreError;
use calm_types::runtime::TimestampMs;
use calm_types::worker::{
    DeathVerdict, ExitEvidence, ExitInterpretation, Liveness, SessionMode, WorkerSession,
};

use super::supervisor::probe_terminal_liveness;
use super::terminal::failed_reason;

/// provider-local mirror of the wire `ThreadStatus`; the arbiter only ever asks "is it `Active`?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatusLite {
    NotLoaded,
    Idle,
    SystemError,
    /// A turn is running or blocked on a human (`waitingOnUserInput` / `waitingOnApproval`). Either flag ⇒ never reap.
    Active {
        waiting_on_user_input: bool,
        waiting_on_approval: bool,
    },
}

/// provider-local mirror of the wire `TurnStatus` (a required field of upstream `Turn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatusLite {
    Completed,
    Interrupted,
    Failed,
    InProgress,
    /// A status value this build does not model; never read as a failure.
    Unknown,
}

/// The thread's MOST RECENT turn, as a live `thread/read` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTurnFacts {
    pub cause: Option<super::WorkerFailureCause>,
    /// `None` = started but never finished (the positive death signal); `Some(ts)` (Unix SECONDS)
    /// = finished cleanly, failed or deliberately aborted — not a death.
    pub completed_at: Option<i64>,
    /// How the turn itself ended, independent of the thread's status.
    pub status: TurnStatusLite,
}

/// The liveness facts the death arbiter needs from a live `thread/read`; `None` from the probe means the RPC was unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLivenessFacts {
    /// Whether the thread is loaded in daemon memory. Secondary signal; the arbiter keys on `status` + the last turn's `completed_at`.
    pub loaded: bool,
    /// The thread's current status.
    pub status: ThreadStatusLite,
    /// The most recent turn; `None` = no turns at all.
    pub last_turn: Option<LastTurnFacts>,
}

#[async_trait]
pub trait CodexDaemonProbe: Send + Sync {
    fn is_running(&self) -> bool;
    fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<String>;

    /// Wall-clock ms of the most recent successful daemon (re)connect; within the rebuild grace the loaded-thread roster is not stable yet.
    fn daemon_connected_at_ms(&self) -> TimestampMs;

    /// Returns `None` on ANY RPC error / unreachable daemon — the arbiter treats that as "can't rule out a live turn".
    async fn read_liveness_facts(&self, thread_id: &str) -> Option<CodexLivenessFacts>;
}

#[async_trait]
pub trait CodexDaemonRoute: Send + Sync {
    async fn for_session(&self, id: &str) -> Option<Arc<dyn CodexDaemonProbe>>;
    async fn for_thread(&self, id: &str) -> Option<Arc<dyn CodexDaemonProbe>>;
}
pub struct FixedCodexDaemon(pub Arc<dyn CodexDaemonProbe>);
#[async_trait]
impl CodexDaemonRoute for FixedCodexDaemon {
    async fn for_session(&self, _: &str) -> Option<Arc<dyn CodexDaemonProbe>> {
        Some(self.0.clone())
    }
    async fn for_thread(&self, _: &str) -> Option<Arc<dyn CodexDaemonProbe>> {
        Some(self.0.clone())
    }
}

#[derive(Clone)]
pub struct CodexProvider {
    supervisor_sock: PathBuf,
    route: Arc<dyn CodexDaemonRoute>,
}

impl CodexProvider {
    pub fn new(supervisor_sock: impl Into<PathBuf>, daemon: Arc<dyn CodexDaemonProbe>) -> Self {
        Self::with_route(supervisor_sock, Arc::new(FixedCodexDaemon(daemon)))
    }
    pub fn with_route(
        supervisor_sock: impl Into<PathBuf>,
        route: Arc<dyn CodexDaemonRoute>,
    ) -> Self {
        Self {
            supervisor_sock: supervisor_sock.into(),
            route,
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

    /// A message is one bracketed paste and Enter (a new turn when idle, `turn/steer` while a
    /// turn runs); typed keys interrupt the turn without starting one (#1782). The task's first
    /// turn starts through the app-server before the terminal UI spawns (#2532).
    fn tui_input(&self) -> TuiInput {
        TuiInput::BracketedPasteSubmit {
            keys_while_bound: BoundKeys::Refused,
            ready: MessageReady::WhenRunning,
        }
    }

    async fn probe_liveness(
        &self,
        session: &WorkerSession,
        ctx: &SpawnCtx,
    ) -> Result<Liveness, CoreError> {
        let Some(daemon) = self.route.for_session(session.id.as_str()).await else {
            return Ok(Liveness::Unknown {
                since_ms: ctx.now_ms,
            });
        };
        let pty = probe_terminal_liveness(
            &self.supervisor_sock,
            session.terminal_run_id.as_deref(),
            ctx.now_ms,
        )
        .await;
        let daemon_running = daemon.is_running();
        let active_turn_id = session
            .thread_id
            .as_deref()
            .and_then(|thread_id| daemon.active_turn_id_for_thread(thread_id));
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

    /// The codex worker death-arbiter, consulted by `reaper::Reaper::sweep_all` for resumable sessions.
    async fn confirm_durable_death(
        &self,
        thread_id: &str,
        now_ms: TimestampMs,
        rebuild_grace_ms: i64,
    ) -> DeathVerdict {
        let Some(daemon) = self.route.for_thread(thread_id).await else {
            return DeathVerdict::Unknown;
        };
        let daemon_connected_at_ms = daemon.daemon_connected_at_ms();
        // S1: daemon down — brain gone, no turn can run, nothing resumes.
        if !daemon.is_running() {
            return DeathVerdict::Dead;
        }
        // Within rebuild grace: the loaded-thread roster isn't stable yet.
        if now_ms - daemon_connected_at_ms < rebuild_grace_ms {
            return DeathVerdict::Unknown;
        }
        // S2: live, past-grace daemon — pull facts and apply the table.
        let facts = daemon.read_liveness_facts(thread_id).await;
        verdict_from_facts(facts)
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

/// The S2 leaf of the truth table, factored out so it is unit-testable without a daemon.
pub(crate) fn verdict_from_facts(facts: Option<CodexLivenessFacts>) -> DeathVerdict {
    let Some(facts) = facts else {
        // RPC unreachable — can't rule out a live turn.
        return DeathVerdict::Unknown;
    };
    // A turn is running OR blocked on a human ⇒ never reap. ANY flag still counts as Active.
    if matches!(facts.status, ThreadStatusLite::Active { .. }) {
        return DeathVerdict::Alive;
    }
    // Idle | SystemError | NotLoaded: no turn running. The discriminator is the last turn's completed_at, NOT the status.
    match facts.last_turn.map(|turn| turn.completed_at) {
        // S2: died-mid-turn — started, never finished, won't re-drive.
        Some(None) => DeathVerdict::Dead,
        // Finished cleanly OR deliberately aborted (both carry a ts) — not a death.
        Some(Some(_)) => DeathVerdict::Alive,
        // No turns at all — no positive signal → conservative no-reap.
        None => DeathVerdict::Unknown,
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

    /// Scriptable [`CodexDaemonProbe`] with a fixed `is_running` and `read_liveness_facts` result.
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
        fn daemon_connected_at_ms(&self) -> TimestampMs {
            0
        }
        async fn read_liveness_facts(&self, _thread_id: &str) -> Option<CodexLivenessFacts> {
            self.facts.clone()
        }
    }

    fn provider_with(running: bool, facts: Option<CodexLivenessFacts>) -> CodexProvider {
        CodexProvider::new(
            "/tmp/fake-supervisor.sock",
            Arc::new(FakeCodexDaemonProbe { running, facts }),
        )
    }

    /// `last` is the last turn's `completed_at`; an unfinished turn reads back `inProgress`.
    fn facts(status: ThreadStatusLite, last: Option<Option<i64>>) -> CodexLivenessFacts {
        CodexLivenessFacts {
            loaded: true,
            status,
            last_turn: last.map(|completed_at| LastTurnFacts {
                cause: None,
                completed_at,
                status: match completed_at {
                    Some(_) => TurnStatusLite::Completed,
                    None => TurnStatusLite::InProgress,
                },
            }),
        }
    }

    const GRACE: i64 = 300_000; // 5 min

    /// Helper: now well past grace so S2 is reached (when daemon up).
    async fn verdict(running: bool, facts: Option<CodexLivenessFacts>) -> DeathVerdict {
        provider_with(running, facts)
            .confirm_durable_death("t-1", 1_000_000, GRACE)
            .await
    }

    #[tokio::test]
    async fn arbiter_s1_daemon_down_is_dead() {
        assert_eq!(verdict(false, None).await, DeathVerdict::Dead);
        assert_eq!(
            verdict(false, Some(facts(ThreadStatusLite::Idle, Some(None)))).await,
            DeathVerdict::Dead
        );
    }

    #[tokio::test]
    async fn arbiter_within_rebuild_grace_is_unknown() {
        let v = provider_with(true, Some(facts(ThreadStatusLite::Idle, Some(None))))
            .confirm_durable_death("t-1", GRACE - 1, GRACE)
            .await;
        assert_eq!(v, DeathVerdict::Unknown);
    }

    #[tokio::test]
    async fn arbiter_grace_boundary_equal_is_not_within_grace() {
        // now - connected == grace is NOT within grace; the died-mid-turn fact proves S2 was reached.
        let v = provider_with(true, Some(facts(ThreadStatusLite::Idle, Some(None))))
            .confirm_durable_death("t-1", GRACE, GRACE)
            .await;
        assert_eq!(v, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_pull_unreachable_is_unknown() {
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
        let f = facts(ThreadStatusLite::Idle, Some(None));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_idle_last_turn_completed_is_alive() {
        // Covers BOTH clean-complete AND deliberate-abort (a TurnAborted carries a timestamp).
        let f = facts(ThreadStatusLite::Idle, Some(Some(1700)));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Alive);
    }

    #[tokio::test]
    async fn arbiter_not_loaded_died_mid_turn_is_dead() {
        let f = facts(ThreadStatusLite::NotLoaded, Some(None));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_system_error_died_mid_turn_is_dead() {
        let f = facts(ThreadStatusLite::SystemError, Some(None));
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Dead);
    }

    #[tokio::test]
    async fn arbiter_no_turns_at_all_is_unknown() {
        let f = facts(ThreadStatusLite::Idle, None);
        assert_eq!(verdict(true, Some(f)).await, DeathVerdict::Unknown);
    }

    #[test]
    fn arbiter_default_for_non_codex_providers_is_unknown() {
        // Exercised via the pure S2 leaf with no facts (the same conservative output as the trait default).
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

    /// Typed keys interrupt a bound codex turn; the worker takes a message instead.
    #[test]
    fn codex_declares_message_delivery_and_refuses_bound_keys() {
        let tui = provider_with(true, None).tui_input();
        assert_eq!(
            tui,
            TuiInput::BracketedPasteSubmit {
                keys_while_bound: BoundKeys::Refused,
                ready: MessageReady::WhenRunning,
            }
        );
        assert!(tui.takes_message());
        assert_eq!(tui.keys_while_bound(), BoundKeys::Refused);
        assert_eq!(tui.message_ready(), Some(MessageReady::WhenRunning));
    }
}

#[cfg(test)]
mod agent_backend_tests {
    use super::*;
    struct Unresolved;
    #[async_trait]
    impl CodexDaemonRoute for Unresolved {
        async fn for_session(&self, _: &str) -> Option<Arc<dyn CodexDaemonProbe>> {
            None
        }
        async fn for_thread(&self, _: &str) -> Option<Arc<dyn CodexDaemonProbe>> {
            None
        }
    }
    #[tokio::test]
    async fn agent_backend_codex_provider_unresolved_route_is_unknown() {
        let provider = CodexProvider::with_route("/missing-supervisor", Arc::new(Unresolved));
        let session: WorkerSession = serde_json::from_value(serde_json::json!({
            "id":"worker", "track_id":"track", "provider":"codex", "mode":"resumable",
            "contract":"executor", "state":"running", "liveness":"unknown", "thread_id":"thread",
            "created_at_ms":0,"updated_at_ms":0
        }))
        .unwrap();
        assert_eq!(
            provider
                .probe_liveness(&session, &SpawnCtx::new(42))
                .await
                .unwrap(),
            Liveness::Unknown { since_ms: 42 }
        );
        assert_eq!(
            provider.confirm_durable_death("thread", 42, 0).await,
            DeathVerdict::Unknown
        );
    }
}

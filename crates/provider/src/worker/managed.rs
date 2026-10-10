//! Liveness of a managed conversation is owned by its host, not its per-turn child process.
use async_trait::async_trait;
use calm_exec::{SpawnCtx, TuiInput, WorkerProvider};
use calm_types::error::CoreError;
use calm_types::worker::{ExitEvidence, ExitInterpretation, Liveness, SessionMode, WorkerSession};
use std::sync::Arc;

#[async_trait]
pub trait ManagedSessionProbe: Send + Sync {
    /// None means the host has no observation, never positive death evidence.
    async fn active_turn(&self, worker_session_id: &str) -> Option<Option<String>>;
}

pub struct ManagedProvider {
    identity: &'static str,
    probe: Arc<dyn ManagedSessionProbe>,
}
impl ManagedProvider {
    pub fn new(identity: &'static str, probe: Arc<dyn ManagedSessionProbe>) -> Self {
        Self { identity, probe }
    }
}
#[async_trait]
impl WorkerProvider for ManagedProvider {
    fn kind(&self) -> &'static str {
        self.identity
    }
    fn session_mode(&self) -> SessionMode {
        SessionMode::Resumable
    }
    /// A managed session runs no terminal UI.
    fn tui_input(&self) -> TuiInput {
        TuiInput::KeysOnly
    }
    async fn probe_liveness(
        &self,
        session: &WorkerSession,
        ctx: &SpawnCtx,
    ) -> Result<Liveness, CoreError> {
        Ok(match self.probe.active_turn(session.id.as_str()).await {
            Some(Some(turn)) => Liveness::Alive {
                active_turn_id: Some(turn),
            },
            Some(None) => Liveness::Idle,
            None => Liveness::Unknown {
                since_ms: ctx.now_ms,
            },
        })
    }
    async fn interpret_exit(
        &self,
        _session: &WorkerSession,
        _evidence: &ExitEvidence,
        _ctx: &SpawnCtx,
    ) -> Result<ExitInterpretation, CoreError> {
        // A pipe/process exit does not settle its durable conversation or authorize a resend.
        Ok(ExitInterpretation::PreserveCard)
    }
}

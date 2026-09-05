//! Execution-period provider contract.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::runtime::TimestampMs;
use calm_types::worker::{
    DeathVerdict, ExitEvidence, ExitInterpretation, Liveness, SessionMode, WorkerSession,
};

/// Handle produced by a successful spawn or resume.
#[derive(Clone, Debug)]
pub enum SpawnHandle {
    Terminal {
        terminal_id: String,
        renderer_id: String,
    },
    Harness {
        worker_session_id: String,
    },
    NoOp,
}

/// Minimal execution-period context handed to [`WorkerProvider`] calls.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct SpawnCtx {
    pub now_ms: TimestampMs,
}

impl SpawnCtx {
    pub fn new(now_ms: TimestampMs) -> Self {
        Self { now_ms }
    }
}

/// Owns a worker session after spawn: liveness, exit interpretation, and resume.
///
/// Probes and interpretation must run outside the write lock; only the final
/// CAS transition is committed under it. `interpret_exit` is the sole exit
/// authority for every observation source.
#[async_trait]
pub trait WorkerProvider: Send + Sync {
    fn kind(&self) -> &'static str;

    fn session_mode(&self) -> SessionMode;

    /// One observation round against a live-or-unknown session.
    ///
    async fn probe_liveness(
        &self,
        session: &WorkerSession,
        ctx: &SpawnCtx,
    ) -> Result<Liveness, CoreError>;

    /// Interpret raw exit evidence before the kernel applies a CAS transition.
    async fn interpret_exit(
        &self,
        session: &WorkerSession,
        evidence: &ExitEvidence,
        ctx: &SpawnCtx,
    ) -> Result<ExitInterpretation, CoreError>;

    /// Re-attach a [`SessionMode::Resumable`] session whose exit was ruled
    /// [`ExitInterpretation::ResumeEligible`]. Default errors — ephemeral
    /// providers never override it.
    async fn resume(
        &self,
        _session: &WorkerSession,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnHandle, CoreError> {
        Err(CoreError::Internal(format!(
            "{} not resumable",
            self.kind()
        )))
    }

    /// Confirm durable death outside the write lock. Only `Dead` authorizes reap.
    async fn confirm_durable_death(
        &self,
        _thread_id: &str,
        _now_ms: TimestampMs,
        _daemon_connected_at_ms: TimestampMs,
        _rebuild_grace_ms: i64,
    ) -> DeathVerdict {
        DeathVerdict::Unknown
    }

    /// Wall-clock ms of the provider daemon's latest successful connection.
    fn daemon_connected_at_ms(&self) -> Option<TimestampMs> {
        None
    }
}

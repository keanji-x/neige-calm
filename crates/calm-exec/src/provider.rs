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

/// Whether typed keys (`text`, `submit`, `key`, `sequence`, a control claim) are accepted while
/// the terminal serves a task attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundKeys {
    Accepted,
    /// Typing interrupts the running turn without starting one (Codex remote TUI, #1782).
    Refused,
}

/// The terminal-input declaration of a worker provider (#2493): how the kernel may put a message
/// into its terminal UI, and whether typed keys are accepted while bound. Keys are refused only
/// beside a message delivery, so a bound worker always keeps one way to take input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiInput {
    /// One bracketed paste of the message, then Enter: a new turn when idle, a steer or a queued
    /// message while a turn runs.
    BracketedPasteSubmit { keys_while_bound: BoundKeys },
    /// The terminal runs no agent conversation that takes a message; typed keys are accepted.
    KeysOnly,
}

impl TuiInput {
    /// Whether the kernel can deliver a message.
    pub fn takes_message(self) -> bool {
        matches!(self, Self::BracketedPasteSubmit { .. })
    }

    /// Whether typed keys are accepted while the terminal serves a task attempt.
    pub fn keys_while_bound(self) -> BoundKeys {
        match self {
            Self::BracketedPasteSubmit { keys_while_bound } => keys_while_bound,
            Self::KeysOnly => BoundKeys::Accepted,
        }
    }
}

/// Owns a worker session after spawn: liveness, exit interpretation, and resume.
///
/// Probes and interpretation run outside the write lock; only the final CAS transition commits under it.
#[async_trait]
pub trait WorkerProvider: Send + Sync {
    fn kind(&self) -> &'static str;

    fn session_mode(&self) -> SessionMode;

    /// How its terminal takes a kernel message and typed keys while bound to a task attempt.
    fn tui_input(&self) -> TuiInput;

    /// One observation round against a live-or-unknown session.
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

    /// Re-attach a resumable session ruled `ResumeEligible`; the default errors and ephemeral providers never override it.
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

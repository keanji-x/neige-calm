//! `WorkerProvider` — extend the spawn saga into the execution period
//! (issue #679 §2).

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::runtime::{RuntimeId, TimestampMs};
use calm_types::worker::{ExitEvidence, ExitInterpretation, Liveness, SessionMode, WorkerSession};

/// Handle to whatever a successful spawn produced. Moved verbatim from
/// calm-server's `operation` module (#679 PR1) — the operations saga
/// constructs it, and `WorkerProvider::resume` returns it, so it belongs to
/// the exec contract layer rather than the saga internals. calm-server
/// re-exports it at the old path.
///
/// Note the variants are still runtime/card-era shaped (`Harness` carries a
/// `runtime_id`); PR6 re-keys them to sessions when the providers move in.
#[derive(Clone, Debug)]
pub enum SpawnHandle {
    Terminal {
        terminal_id: String,
        renderer_id: String,
    },
    Harness {
        runtime_id: RuntimeId,
    },
    NoOp,
}

/// Minimal execution-period context handed to [`WorkerProvider`] calls.
///
/// **Deliberately not** calm-server's `operation::SpawnCtx`, which bundles
/// `Arc<dyn RouteRepo>` + `Arc<DaemonClient>` + the event bus + the
/// terminal renderer registry — exactly the IO coupling this crate
/// firewalls away. Provider implementations receive their heavyweight
/// dependencies (daemon client, app-server handles) at **construction**
/// (PR6); per-call context carries only data the execution layer is
/// entitled to see.
///
/// PR1 ships the smallest honest version: a clock. The reaper's
/// `Unknown { since_ms }` deadlines and `ExitEvidence.observed_at_ms`
/// stamps need an injected "now" so L1 tests (PR5) stay zero-process and
/// deterministic. `#[non_exhaustive]` + [`SpawnCtx::new`] keep PR6 free to
/// widen it (cwd/env for `resume`) without breaking implementors.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct SpawnCtx {
    /// Unix-ms "now", injected by the driver (kernel reaper / test clock).
    pub now_ms: TimestampMs,
}

impl SpawnCtx {
    pub fn new(now_ms: TimestampMs) -> Self {
        Self { now_ms }
    }
}

/// The execution-period provider contract (issue #679 §2).
///
/// The existing `ProviderAdapter` saga (calm-server `operation/mod.rs`)
/// ends at `spawn_succeeded` and drops the `SpawnHandle` on the floor;
/// `WorkerProvider` is the part that owns the session **after** spawn:
/// liveness probing, exit interpretation, resume.
///
/// ## Relation to `ProviderAdapter`
///
/// The issue declares `trait WorkerProvider: ProviderAdapter`. PR1 cannot
/// express that supertrait yet — `ProviderAdapter` lives in calm-server and
/// is welded to sqlx transactions and the IO-coupled `SpawnCtx` (the very
/// coupling this crate exists to break). So PR1 defines `WorkerProvider`
/// standalone with its own `kind()` discriminator.
/// TODO(#679 PR6): when the adapters move into calm-provider and
/// `ProviderAdapter`'s tx/ctx types are abstracted, fold the spawn saga in —
/// either as the supertrait per the issue or by merging both into one
/// session-provider trait. Until then the two traits are bridged at
/// assembly by implementing both on the same concrete provider.
///
/// ## Driver obligations (the PR8 reaper)
///
/// * `probe_liveness` / `interpret_exit` are async provider calls and must
///   **never run inside the write lock** — three-phase: gather evidence
///   (unlocked) → interpret (unlocked) → CAS commit (`WHERE status IN
///   active` + the transition matrix). See issue §2 "Reaper".
/// * `interpret_exit` is the **single exit authority**: every exit
///   observation (attach reader, sweeper, probe, daemon) funnels through it
///   before any state write (issue hard-problem 3).
#[async_trait]
pub trait WorkerProvider: Send + Sync {
    /// Provider discriminator (`"codex"` / `"claude"` / `"terminal"` /
    /// `"fake"`…). Mirrors `ProviderAdapter::kind` so one concrete type can
    /// implement both traits with a single identity during the PR6 bridge.
    fn kind(&self) -> &'static str;

    /// Whether sessions of this provider can be resumed after the driving
    /// process dies (issue §2): terminal/claude are `Ephemeral`, codex
    /// threads are `Resumable`.
    fn session_mode(&self) -> SessionMode;

    /// One observation round against a live-or-unknown session.
    ///
    /// Probe reality per provider (issue §2, audited): terminal — supervisor
    /// `ControlMsg::Probe`; claude — same PTY probe; codex — composite
    /// evidence only (PTY + daemon `is_running` + in-memory turn cache),
    /// degrading to [`Liveness::Unknown`] after a daemon restart.
    ///
    /// T2: the resulting state update is an observation write — no event.
    async fn probe_liveness(
        &self,
        session: &WorkerSession,
        ctx: &SpawnCtx,
    ) -> Result<Liveness, CoreError>;

    /// The single exit authority (issue §2): given raw evidence, rule what
    /// it means for this session. The kernel commits the verdict via CAS +
    /// transition matrix; a `Failed` verdict on a session with no
    /// `TaskCompleted`/`TaskFailed` makes the kernel emit the convergence
    /// `TaskFailed` (same path as the existing spawn-failure fallback).
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
}

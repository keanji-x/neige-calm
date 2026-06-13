//! The reaction seam — agent→truth decision writes (issue #679 §6, L1).
//!
//! Issue context: "FakeRoot = FakeProvider + a scripted **reaction seam**
//! (`on TaskCompleted → decision-write lifecycle Done`, executed as the
//! root principal). The reaction seam is a first-class calm-exec contract,
//! not a provider-trait side effect" — the existing stub appserver is
//! transport-only (it swallows turns but cannot author writes; real writes
//! come from the codex process calling MCP tools, off the appserver path).
//!
//! Dependency order this seam unlocks (issue hard-problem 4): calm-exec
//! defines it (PR1) → FakeRoot exercises it in pure memory (PR5) → the real
//! spec harness implements it behind the provider boundary (PR6).
//!
//! Two halves, mirroring the one-way loop of axiom 1:
//!
//! * [`AgentReactor`] is the **agent side**: observation in, decision
//!   intents out. The real implementation is an LLM turn (harness issues a
//!   turn, the agent calls MCP decision tools); the scripted implementation
//!   is a pure table.
//! * [`DecisionSink`] is the **truth side**: commit one intent as a
//!   principal, through the single gated write entrance (T1: state + event
//!   in one transaction). PR1 has no implementation; calm-truth's gated
//!   entrance (PR2–PR4) is the production one, and PR5's L1 harness
//!   provides an in-memory recorder for assertions.

use async_trait::async_trait;
use calm_types::error::CoreError;
use calm_types::event::ArtifactRef;
use calm_types::ids::WaveId;
use calm_types::model::WaveLifecycle;
use calm_types::observation::Observation;
use calm_types::worker::Principal;
use serde_json::Value;

/// One decision-write intent, authored by an agent session, to be committed
/// **as a principal** through the gated entrance.
///
/// The variants are the agent-reachable rows of the issue §3 decision
/// matrix — deliberately typed commands rather than raw `Event`s, because
/// several decision writes are not a bare event append (report writes
/// project through the CRDT; lifecycle writes mutate the wave row in the
/// same tx). The gate decides per-variant whether `principal` may commit it
/// (recorder grant for lifecycle/report, planner contract for dispatch,
/// own-session for task results).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionIntent {
    /// Wave lifecycle command (matrix row "wave lifecycle + report writes",
    /// recorder-only). Legality of the edge itself stays with the kernel
    /// FSM (`calm_types::wave_lifecycle::validate_transition`).
    LifecycleTransition {
        wave_id: WaveId,
        to: WaveLifecycle,
        agent_message: Option<String>,
    },
    /// Wave-report write (recorder-only; #679 upgrades today's soft MCP
    /// gate to an in-tx rule). `None` = leave that half alone.
    ReportWrite {
        wave_id: WaveId,
        summary: Option<String>,
        body: Option<String>,
        agent_message: Option<String>,
    },
    /// `codex.worker_requested` (matrix row "dispatch", planner/root only,
    /// wave-scoped).
    DispatchCodexWorker {
        idempotency_key: String,
        goal: String,
        context: Value,
        acceptance_criteria: Option<String>,
        agent_message: Option<String>,
    },
    /// `terminal.worker_requested` (same dispatch row).
    DispatchTerminalWorker {
        idempotency_key: String,
        cmd: String,
        cwd: Option<String>,
        agent_message: Option<String>,
    },
    /// `task.completed` (matrix row "task.completed/failed" — own session
    /// only, reporting up to the requester).
    CompleteTask {
        idempotency_key: String,
        result: Value,
        artifacts: Vec<ArtifactRef>,
        agent_message: Option<String>,
    },
    /// `task.failed` (same row).
    FailTask {
        idempotency_key: String,
        reason: String,
        agent_message: Option<String>,
    },
}

/// Agent-side decision logic: one observation in, zero or more decision
/// intents out, executed as [`AgentReactor::principal`].
///
/// Implementations:
/// * **FakeRoot (PR5)** — a scripted, pure-memory table (e.g. "on
///   `Observation::TaskCompleted` → `LifecycleTransition { to: Done }`"),
///   making full wave convergence testable with zero LLMs and zero
///   processes.
/// * **Real harness (PR6)** — folds the observation into the spec session's
///   turn queue; the intents materialize asynchronously as the LLM calls
///   MCP decision tools. (The sync return is then empty — the seam's
///   contract is "intents this observation *directly* produces", which for
///   a real LLM is none at delivery time.)
#[async_trait]
pub trait AgentReactor: Send + Sync {
    /// The principal this reactor's intents are committed as. For a root
    /// session this is the `Principal::Agent` whose `session_id` matches
    /// `wave.root_session_id` — that derivation (not this method) is what
    /// the gate trusts.
    fn principal(&self) -> Principal;

    /// React to one delivered observation.
    async fn react(&self, observation: &Observation) -> Result<Vec<DecisionIntent>, CoreError>;
}

/// Truth-side commit point for decision intents.
///
/// The production implementation is calm-truth's single gated write
/// entrance (T1: the state mutation and its event land in one transaction;
/// the gate checks `principal` × intent inside that same transaction —
/// in-tx root check, no TOCTOU). PR5's L1 harness implements it as an
/// in-memory recorder so scripted reactions can be asserted without a
/// database.
#[async_trait]
pub trait DecisionSink: Send + Sync {
    /// Commit one intent as `principal`. A gate denial is an `Err` — the
    /// intent must leave no partial state behind (T1).
    async fn commit(&self, principal: &Principal, intent: DecisionIntent) -> Result<(), CoreError>;
}

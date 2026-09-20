//! Worker session and authority vocabulary.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

use crate::ids::{AreaId, CardId, TrackId};
use crate::runtime::TimestampMs;

/// Opaque execution-session identifier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct WorkerSessionId(pub String);

impl WorkerSessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for WorkerSessionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for WorkerSessionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for WorkerSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl AsRef<str> for WorkerSessionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Security principal. Agent grants are derived from the session row at gate time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Principal {
    User,
    Kernel,
    Agent {
        session_id: WorkerSessionId,
        track_id: TrackId,
        area_id: AreaId,
    },
}

/// The I/O shape of a worker. Root authority is derived separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerContract {
    /// Observations in, dispatch requests out.
    Planner,
    /// Goal in, result out (`task.completed` / `task.failed`).
    Executor,
    /// Artifacts and acceptance criteria in, verdict out.
    Validator,
}

impl WorkerContract {
    pub fn as_db_str(self) -> &'static str {
        match self {
            WorkerContract::Planner => "planner",
            WorkerContract::Executor => "executor",
            WorkerContract::Validator => "validator",
        }
    }
}

impl TryFrom<String> for WorkerContract {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "planner" => Ok(WorkerContract::Planner),
            "executor" => Ok(WorkerContract::Executor),
            "validator" => Ok(WorkerContract::Validator),
            other => Err(format!("unknown worker_sessions.contract value `{other}`")),
        }
    }
}

/// Whether a dead session's thread can be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Ephemeral,
    Resumable,
}

impl SessionMode {
    pub fn as_db_str(self) -> &'static str {
        match self {
            SessionMode::Ephemeral => "ephemeral",
            SessionMode::Resumable => "resumable",
        }
    }
}

impl TryFrom<String> for SessionMode {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "ephemeral" => Ok(SessionMode::Ephemeral),
            "resumable" => Ok(SessionMode::Resumable),
            other => Err(format!("unknown worker_sessions.mode value `{other}`")),
        }
    }
}

/// An unlocked liveness observation, not persisted session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Liveness {
    /// Process-level evidence says the session is executing; carries the in-flight turn when the provider can see one.
    Alive { active_turn_id: Option<String> },
    /// Alive but no in-flight turn.
    Idle,
    /// The provider observed an exit — evidence attached.
    Exited { evidence: ExitEvidence },
    /// The provider cannot tell; `since_ms` is when certainty was lost, so the reaper can escalate on a deadline.
    Unknown { since_ms: TimestampMs },
}

/// Only `Dead` authorizes a reap; `Alive` and `Unknown` are conservative no-reap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathVerdict {
    Dead,
    Alive,
    Unknown,
}

/// Which observer produced a piece of exit evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitSource {
    /// The PTY attach reader saw EOF / exit.
    AttachReader,
    /// The terminal sweeper's scan.
    Sweeper,
    /// A reaper `probe_liveness` round.
    Probe,
    /// The proc-supervisor daemon reported it (`child.wait()` sidecar).
    Daemon,
}

/// Raw, uninterpreted exit observation; only `WorkerProvider::interpret_exit` decides what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitEvidence {
    /// Child exit code, when the exit was a normal `exit()` / main return.
    pub exit_code: Option<i32>,
    /// True when the child died to a signal — mutually exclusive with `exit_code.is_some()` at the writer.
    pub signal_killed: bool,
    /// Unix-ms timestamp of the observation.
    pub observed_at_ms: TimestampMs,
    /// Which observer saw it.
    pub source: ExitSource,
}

/// `WorkerProvider::interpret_exit` verdict — the single exit authority's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExitInterpretation {
    /// Session ended having done its job; kernel emits no fallback event.
    Completed,
    /// Session died without producing `TaskCompleted`/`TaskFailed`; the kernel emits the convergence `TaskFailed`.
    Failed { reason: String },
    /// The session is dead but its card/projection must be kept for forensics.
    PreserveCard,
    /// `SessionMode::Resumable` and the thread is still alive — eligible for `WorkerProvider::resume`.
    ResumeEligible,
}

impl ExitInterpretation {
    /// The lowercase discriminator the `worker_sessions.exit_interpretation` column stores.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            ExitInterpretation::Completed => "completed",
            ExitInterpretation::Failed { .. } => "failed",
            ExitInterpretation::PreserveCard => "preserve_card",
            ExitInterpretation::ResumeEligible => "resume_eligible",
        }
    }
}

/// Provider discriminator persisted on the session row; in the session model the planner is just
/// another worker, so its provider is plain `codex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerProviderKind {
    Codex,
    Claude,
    Terminal,
}

impl WorkerProviderKind {
    /// The lowercase string the `worker_sessions.provider` CHECK pins.
    pub fn as_db_str(self) -> &'static str {
        match self {
            WorkerProviderKind::Codex => "codex",
            WorkerProviderKind::Claude => "claude",
            WorkerProviderKind::Terminal => "terminal",
        }
    }
}

impl TryFrom<String> for WorkerProviderKind {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "codex" => Ok(WorkerProviderKind::Codex),
            "claude" => Ok(WorkerProviderKind::Claude),
            "terminal" => Ok(WorkerProviderKind::Terminal),
            other => Err(format!("unknown worker_sessions.provider value `{other}`")),
        }
    }
}

/// Session state machine column (`worker_sessions.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerSessionState {
    Starting,
    Running,
    Idle,
    TurnPending,
    Exited,
    Failed,
    Superseded,
}

impl WorkerSessionState {
    /// States that carry live MCP authority; keep in lockstep with the `session_get_by_active_token_hash` SQL predicate.
    pub fn is_active_authority(self) -> bool {
        matches!(
            self,
            WorkerSessionState::Starting
                | WorkerSessionState::Running
                | WorkerSessionState::Idle
                | WorkerSessionState::TurnPending
        )
    }

    /// Terminal states never transition again.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkerSessionState::Exited
                | WorkerSessionState::Failed
                | WorkerSessionState::Superseded
        )
    }

    /// The snake_case string the `worker_sessions.state` CHECK pins.
    pub fn as_db_str(self) -> &'static str {
        match self {
            WorkerSessionState::Starting => "starting",
            WorkerSessionState::Running => "running",
            WorkerSessionState::Idle => "idle",
            WorkerSessionState::TurnPending => "turn_pending",
            WorkerSessionState::Exited => "exited",
            WorkerSessionState::Failed => "failed",
            WorkerSessionState::Superseded => "superseded",
        }
    }
}

impl TryFrom<String> for WorkerSessionState {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "starting" => Ok(WorkerSessionState::Starting),
            "running" => Ok(WorkerSessionState::Running),
            "idle" => Ok(WorkerSessionState::Idle),
            "turn_pending" => Ok(WorkerSessionState::TurnPending),
            "exited" => Ok(WorkerSessionState::Exited),
            "failed" => Ok(WorkerSessionState::Failed),
            "superseded" => Ok(WorkerSessionState::Superseded),
            other => Err(format!("unknown worker_sessions.state value `{other}`")),
        }
    }
}

/// Persisted liveness tag: the flattened, evidence-free projection of [`Liveness`], never event-emitting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LivenessTag {
    Alive,
    Idle,
    Exited,
    #[default]
    Unknown,
}

impl LivenessTag {
    /// The lowercase string the `worker_sessions.liveness` column stores.
    pub fn as_db_str(self) -> &'static str {
        match self {
            LivenessTag::Alive => "alive",
            LivenessTag::Idle => "idle",
            LivenessTag::Exited => "exited",
            LivenessTag::Unknown => "unknown",
        }
    }
}

impl TryFrom<String> for LivenessTag {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "alive" => Ok(LivenessTag::Alive),
            "idle" => Ok(LivenessTag::Idle),
            "exited" => Ok(LivenessTag::Exited),
            "unknown" => Ok(LivenessTag::Unknown),
            other => Err(format!("unknown worker_sessions.liveness value `{other}`")),
        }
    }
}

impl From<&Liveness> for LivenessTag {
    fn from(liveness: &Liveness) -> Self {
        match liveness {
            Liveness::Alive { .. } => LivenessTag::Alive,
            Liveness::Idle => LivenessTag::Idle,
            Liveness::Exited { .. } => LivenessTag::Exited,
            Liveness::Unknown { .. } => LivenessTag::Unknown,
        }
    }
}

/// The execution-truth entity: execution identity hangs off the **session**, and the card points at
/// it. Root-ness is **not** a field — it is derived from `track.root_session_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerSession {
    pub id: WorkerSessionId,
    pub track_id: TrackId,
    pub provider: WorkerProviderKind,
    pub mode: SessionMode,
    pub contract: WorkerContract,
    /// Delegation tree (≈ ppid). `None` for kernel-dispatched roots.
    pub parent_session_id: Option<WorkerSessionId>,
    /// Result routing: push key→requester, fallback root.
    pub requester_session_id: Option<WorkerSessionId>,
    pub state: WorkerSessionState,
    pub mcp_token_hash: Option<String>,
    pub thread_id: Option<String>,
    pub agent_session_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub terminal_run_id: Option<String>,
    /// Owning card. `Some` for every live/reachable session; `None` only for a pre-existing leaked
    /// placeholder whose card was deleted before the backfill.
    pub card_id: Option<CardId>,
    /// Planner `HarnessSnapshot` moves here as-is (opaque to the kernel).
    pub handle_state_json: Option<Value>,
    pub liveness: LivenessTag,
    pub liveness_probed_at_ms: Option<TimestampMs>,
    pub exit_code: Option<i32>,
    /// [`ExitInterpretation::as_db_str`] discriminator, once the single exit authority has ruled.
    pub exit_interpretation: Option<String>,
    /// The saga that minted this session (`operations.id`); coordination breadcrumb, not truth.
    pub spawn_op_id: Option<String>,
    /// Timestamp of the last observed thread activity. Observation-only writes never bump
    /// `updated_at_ms`, because projection reads use that to choose the winning active session per card.
    pub last_activity_ms: Option<TimestampMs>,
    /// The last observed thread status string (idle|active|waitingOnUserInput|waitingOnApproval|systemError|notLoaded).
    pub last_thread_status: Option<String>,
    pub created_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
    pub completed_at_ms: Option<TimestampMs>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_strings_round_trip() {
        for contract in [
            WorkerContract::Planner,
            WorkerContract::Executor,
            WorkerContract::Validator,
        ] {
            assert_eq!(
                WorkerContract::try_from(contract.as_db_str().to_string()).unwrap(),
                contract
            );
        }
        for mode in [SessionMode::Ephemeral, SessionMode::Resumable] {
            assert_eq!(
                SessionMode::try_from(mode.as_db_str().to_string()).unwrap(),
                mode
            );
        }
        for provider in [
            WorkerProviderKind::Codex,
            WorkerProviderKind::Claude,
            WorkerProviderKind::Terminal,
        ] {
            assert_eq!(
                WorkerProviderKind::try_from(provider.as_db_str().to_string()).unwrap(),
                provider
            );
        }
        for state in [
            WorkerSessionState::Starting,
            WorkerSessionState::Running,
            WorkerSessionState::Idle,
            WorkerSessionState::TurnPending,
            WorkerSessionState::Exited,
            WorkerSessionState::Failed,
            WorkerSessionState::Superseded,
        ] {
            assert_eq!(
                WorkerSessionState::try_from(state.as_db_str().to_string()).unwrap(),
                state
            );
        }
        for tag in [
            LivenessTag::Alive,
            LivenessTag::Idle,
            LivenessTag::Exited,
            LivenessTag::Unknown,
        ] {
            assert_eq!(
                LivenessTag::try_from(tag.as_db_str().to_string()).unwrap(),
                tag
            );
        }
    }

    #[test]
    fn session_state_terminal_set_matches_runtime_matrix() {
        let terminal = [
            WorkerSessionState::Exited,
            WorkerSessionState::Failed,
            WorkerSessionState::Superseded,
        ];
        let active = [
            WorkerSessionState::Starting,
            WorkerSessionState::Running,
            WorkerSessionState::Idle,
            WorkerSessionState::TurnPending,
        ];
        for s in terminal {
            assert!(s.is_terminal(), "{s:?} must be terminal");
            assert!(!s.is_active_authority(), "{s:?} must not authenticate");
        }
        for s in active {
            assert!(!s.is_terminal(), "{s:?} must be active");
            assert!(s.is_active_authority(), "{s:?} must authenticate");
        }
    }

    #[test]
    fn liveness_tag_projection() {
        assert_eq!(
            LivenessTag::from(&Liveness::Alive {
                active_turn_id: Some("turn-1".into())
            }),
            LivenessTag::Alive
        );
        assert_eq!(LivenessTag::from(&Liveness::Idle), LivenessTag::Idle);
        assert_eq!(
            LivenessTag::from(&Liveness::Exited {
                evidence: ExitEvidence {
                    exit_code: Some(0),
                    signal_killed: false,
                    observed_at_ms: 1,
                    source: ExitSource::Probe,
                }
            }),
            LivenessTag::Exited
        );
        assert_eq!(
            LivenessTag::from(&Liveness::Unknown { since_ms: 9 }),
            LivenessTag::Unknown
        );
        assert_eq!(LivenessTag::default(), LivenessTag::Unknown);
    }

    #[test]
    fn death_verdict_serde_round_trip() {
        for (verdict, wire) in [
            (DeathVerdict::Dead, "\"dead\""),
            (DeathVerdict::Alive, "\"alive\""),
            (DeathVerdict::Unknown, "\"unknown\""),
        ] {
            assert_eq!(serde_json::to_string(&verdict).unwrap(), wire);
            assert_eq!(serde_json::from_str::<DeathVerdict>(wire).unwrap(), verdict);
        }
    }

    #[test]
    fn exit_interpretation_db_str() {
        assert_eq!(ExitInterpretation::Completed.as_db_str(), "completed");
        assert_eq!(
            ExitInterpretation::Failed {
                reason: "boom".into()
            }
            .as_db_str(),
            "failed"
        );
        assert_eq!(
            ExitInterpretation::PreserveCard.as_db_str(),
            "preserve_card"
        );
        assert_eq!(
            ExitInterpretation::ResumeEligible.as_db_str(),
            "resume_eligible"
        );
    }
}

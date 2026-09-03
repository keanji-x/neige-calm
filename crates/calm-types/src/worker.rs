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
    /// Process-level evidence says the session is executing. Carries the
    /// in-flight turn when the provider can see one (codex turn cache).
    Alive { active_turn_id: Option<String> },
    /// Alive but no in-flight turn.
    Idle,
    /// The provider observed an exit — evidence attached.
    Exited { evidence: ExitEvidence },
    /// The provider cannot tell (e.g. codex after a daemon restart: the
    /// upstream app-server has no thread-status RPC). `since_ms` is when
    /// certainty was lost, so the reaper can escalate on a deadline.
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

/// Which observer produced a piece of exit evidence (issue #679 §2).
/// All of today's independent exit writers funnel through this vocabulary
/// when PR8 makes `interpret_exit` the single exit authority.
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

/// Raw, uninterpreted exit observation (issue #679 §2). Producers record
/// what they saw; only `WorkerProvider::interpret_exit` decides what it
/// *means* — that asymmetry is what makes the exit authority single.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitEvidence {
    /// Child exit code, when the exit was a normal `exit()` / main return.
    pub exit_code: Option<i32>,
    /// True when the child died to a signal — mutually exclusive with
    /// `exit_code.is_some()` at the writer (mirrors `terminals.signal_killed`).
    pub signal_killed: bool,
    /// Unix-ms timestamp of the observation.
    pub observed_at_ms: TimestampMs,
    /// Which observer saw it.
    pub source: ExitSource,
}

/// `WorkerProvider::interpret_exit` verdict (issue #679 §2) — the single
/// exit authority's output, consumed by the PR8 reaper's CAS commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExitInterpretation {
    /// Session ended having done its job; kernel emits no fallback event.
    Completed,
    /// Session died without producing `TaskCompleted`/`TaskFailed`; the
    /// kernel emits the convergence `TaskFailed` carrying `reason`.
    Failed { reason: String },
    /// Unifies the `worker_spawn_failure_preserved` semantics: the session
    /// is dead but its card/projection must be kept for forensics.
    PreserveCard,
    /// `SessionMode::Resumable` and the thread is still alive — eligible
    /// for `WorkerProvider::resume` instead of failure convergence.
    ResumeEligible,
}

impl ExitInterpretation {
    /// The lowercase discriminator the PR2 `worker_sessions.
    /// exit_interpretation` TEXT column stores (the `Failed.reason` text
    /// goes to the event log, not the state row).
    pub fn as_db_str(&self) -> &'static str {
        match self {
            ExitInterpretation::Completed => "completed",
            ExitInterpretation::Failed { .. } => "failed",
            ExitInterpretation::PreserveCard => "preserve_card",
            ExitInterpretation::ResumeEligible => "resume_eligible",
        }
    }
}

// ---------------------------------------------------------------------------
// WorkerSession entity — issue #679 §1
// ---------------------------------------------------------------------------

/// Provider discriminator persisted on the session row
/// (`worker_sessions.provider TEXT CHECK (provider IN
/// ('codex','claude','terminal'))`, issue #679 §1).
///
/// Distinct from [`crate::runtime::WorkerSessionKind`] (which is a *card-runtime*
/// projection vocabulary with the legacy `shared-spec` arm) and from
/// [`crate::runtime::AgentProvider`] (which has no terminal arm): in the
/// session model the planner is just another worker, so its provider is
/// plain `codex`, and root-ness lives on the track row instead of a special
/// kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerProviderKind {
    Codex,
    Claude,
    Terminal,
}

impl WorkerProviderKind {
    /// The lowercase string the PR2 `worker_sessions.provider` CHECK pins.
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

/// Session state machine column (`worker_sessions.state`, issue #679 §1).
/// Single runtime/session state vocabulary (`worker_sessions.state`, issue #679 §1).
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
    /// States that carry live MCP authority. Keep this in lockstep with the
    /// `session_get_by_active_token_hash` SQL predicate.
    pub fn is_active_authority(self) -> bool {
        matches!(
            self,
            WorkerSessionState::Starting
                | WorkerSessionState::Running
                | WorkerSessionState::Idle
                | WorkerSessionState::TurnPending
        )
    }

    /// Terminal states never transition again (mirrors the runtime-status
    /// matrix golden's terminal-absorption rule).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkerSessionState::Exited
                | WorkerSessionState::Failed
                | WorkerSessionState::Superseded
        )
    }

    /// The snake_case string the PR2 `worker_sessions.state` CHECK pins.
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

/// Persisted liveness tag (`worker_sessions.liveness TEXT NOT NULL DEFAULT
/// 'unknown'`, issue #679 §1). The flattened, evidence-free projection of
/// [`Liveness`] — T2 observation state, never event-emitting.
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
    /// The lowercase string the PR2 `worker_sessions.liveness` column stores.
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

/// The execution-truth entity (issue #679 §1): execution identity (MCP token
/// hash, codex thread, liveness) hangs off the **session**, and the card points
/// at it (`cards.session_id`) instead of owning it.
///
/// Pure struct, field-per-column against the current `worker_sessions` schema.
/// Root-ness is **not** a field — it is derived from `track.root_session_id`
/// (single source of truth, §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerSession {
    pub id: WorkerSessionId,
    pub track_id: TrackId,
    pub provider: WorkerProviderKind,
    pub mode: SessionMode,
    pub contract: WorkerContract,
    /// Delegation tree (≈ ppid). `None` for kernel-dispatched roots.
    pub parent_session_id: Option<WorkerSessionId>,
    /// Result routing: push key→requester, fallback root (issue #679 §4 —
    /// the one real change sub-planners require).
    pub requester_session_id: Option<WorkerSessionId>,
    pub state: WorkerSessionState,
    // --- execution identity (reclaimed from cards) ---
    pub mcp_token_hash: Option<String>,
    pub thread_id: Option<String>,
    pub agent_session_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub terminal_run_id: Option<String>,
    /// Owning card. `Some` for every live/reachable session. Migration 0054
    /// backfilled it from `cards.session_id` for rows created before the
    /// column existed.
    /// `None` ONLY for a pre-existing leaked terminal deferred-planner placeholder
    /// whose card had already been deleted before the backfill. #679 PR9b-0.
    pub card_id: Option<CardId>,
    /// Planner `HarnessSnapshot` moves here as-is (opaque to the kernel).
    pub handle_state_json: Option<Value>,
    // --- liveness (execution-period observation, persisted; T2) ---
    pub liveness: LivenessTag,
    pub liveness_probed_at_ms: Option<TimestampMs>,
    pub exit_code: Option<i32>,
    /// [`ExitInterpretation::as_db_str`] discriminator, once the single
    /// exit authority has ruled (PR8).
    pub exit_interpretation: Option<String>,
    /// The saga that minted this session (`operations.id`). Coordination
    /// breadcrumb, not truth (T4).
    pub spawn_op_id: Option<String>,
    /// Durable codex worker-liveness signal (#741 §1.2; T2, push-fed):
    /// timestamp of the last observed thread activity. Observation-only writes
    /// never bump `updated_at_ms` because projection reads use that timestamp
    /// to choose the winning active session per card. `None` until the activity
    /// feeder first stamps it.
    pub last_activity_ms: Option<TimestampMs>,
    /// Durable codex worker-liveness signal (#741 §1.2; T2, push-fed): the
    /// last observed thread status string (idle|active|waitingOnUserInput|
    /// waitingOnApproval|systemError|notLoaded). `worker_sessions`-only.
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
        // Mirrors the WorkerSessionState terminal set pinned by PR0's
        // runtime_status_matrix golden: exited / failed / superseded
        // absorb, everything else is active.
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

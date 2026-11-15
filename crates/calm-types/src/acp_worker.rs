//! Role-specific durable ACP Worker state; never a Planner HarnessSnapshot or conversation receipt.
use crate::runtime::AgentProvider;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequestedSelection {
    pub provider: AgentProvider,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerTurnPhase {
    Prepared,
    Submitting,
    Settled,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerSelectionSource {
    Explicit,
    ProviderDefaultReported,
    ProviderDefaultUnreported,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct WorkerSelectionSources {
    pub model: WorkerSelectionSource,
    pub effort: WorkerSelectionSource,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadOnlyWorkerTarget {
    pub canonical_path: String,
    pub git_common_dir: String,
    pub head: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerReportOutcome {
    Completed {
        #[ts(type = "unknown")]
        result: Value,
        artifacts: Vec<crate::event::ArtifactRef>,
    },
    Failed {
        reason: String,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerPendingReport {
    pub outcome: WorkerReportOutcome,
    pub commit_message: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerTimeoutCleanup {
    pub task_id: String,
    pub requested_at_ms: i64,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpWorkerState {
    pub version: u8,
    pub attempt_id: String,
    pub requested_selection: WorkerRequestedSelection,
    pub effective_model: Option<String>,
    pub effective_effort: Option<String>,
    pub selection_source: WorkerSelectionSources,
    pub native_session_id: Option<String>,
    /// Captured grant generation, retained after token revocation to fence late cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_generation: Option<String>,
    pub turn_phase: WorkerTurnPhase,
    pub notified_through_seq: i64,
    pub goal_submitted: bool,
    #[serde(default)]
    pub turn_index: u32,
    pub release_intent: bool,
    #[serde(default)]
    pub cleanup_confirmed: bool,
    #[serde(default)]
    pub pending_report: Option<WorkerPendingReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_cleanup: Option<WorkerTimeoutCleanup>,
    pub read_only_target: Option<ReadOnlyWorkerTarget>,
}
impl AcpWorkerState {
    pub fn prepared(attempt_id: String, requested_selection: WorkerRequestedSelection) -> Self {
        Self {
            version: 1,
            attempt_id,
            requested_selection,
            effective_model: None,
            effective_effort: None,
            selection_source: WorkerSelectionSources {
                model: WorkerSelectionSource::ProviderDefaultUnreported,
                effort: WorkerSelectionSource::ProviderDefaultUnreported,
            },
            native_session_id: None,
            credential_generation: None,
            turn_phase: WorkerTurnPhase::Prepared,
            notified_through_seq: 0,
            goal_submitted: false,
            turn_index: 0,
            release_intent: false,
            cleanup_confirmed: false,
            pending_report: None,
            timeout_cleanup: None,
            read_only_target: None,
        }
    }
    /// An ambiguous transport write must never cause the goal to be submitted again.
    pub fn recover(&mut self) {
        if self.turn_phase == WorkerTurnPhase::Submitting {
            self.turn_phase = WorkerTurnPhase::Unknown;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_execution_unknown_never_becomes_prepared_on_recovery() {
        let mut state = AcpWorkerState::prepared(
            "a".into(),
            WorkerRequestedSelection {
                provider: AgentProvider::Antigravity,
                model: None,
                reasoning_effort: None,
            },
        );
        state.turn_phase = WorkerTurnPhase::Submitting;
        state.goal_submitted = true;
        state.recover();
        assert_eq!(state.turn_phase, WorkerTurnPhase::Unknown);
        assert!(state.goal_submitted);
    }
}

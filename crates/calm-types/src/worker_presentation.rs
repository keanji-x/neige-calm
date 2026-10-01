//! Worker card presentation and derived task report snapshots.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// Native-only workers expose status and results without an interactive terminal client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerPresentation {
    NativeOnly,
    InteractiveTui { terminal_id: String },
}

/// Derived from the task row and its committed report event; never a second persisted report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct WorkerSnapshot {
    pub task_id: String,
    pub goal: String,
    pub status: WorkerSnapshotStatus,
    pub report: WorkerSnapshotReport,
}

/// Validated task-status wire label; state transitions remain owned by Truth's TaskStatus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(try_from = "String", into = "String")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[ts(type = "\"pending\" | \"dispatched\" | \"running\" | \"verifying\" | \
            \"done\" | \"failed\" | \"canceled\"")]
pub struct WorkerSnapshotStatus(String);
impl TryFrom<String> for WorkerSnapshotStatus {
    type Error = String;
    fn try_from(label: String) -> Result<Self, Self::Error> {
        match label.as_str() {
            "pending" | "dispatched" | "running" | "verifying" | "done" | "failed" | "canceled" => {
                Ok(Self(label))
            }
            _ => Err(format!("unknown task status: {label}")),
        }
    }
}
impl From<WorkerSnapshotStatus> for String {
    fn from(status: WorkerSnapshotStatus) -> Self {
        status.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerSnapshotReport {
    Pending,
    Reported {
        outcome: WorkerSnapshotOutcome,
        #[ts(type = "unknown")]
        result: serde_json::Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerSnapshotOutcome {
    Completed,
    Failed,
}

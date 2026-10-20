//! Shared gate declarations and wrapper file paths (#2464). The kernel gate (`task-verify`) and
//! the worker-requested run (`task-gate-run`) own their admission, release order and completion;
//! nothing here decides a verdict.

#[cfg(all(test, target_os = "macos"))]
mod unsupported_gate_tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{CalmError, Result};

/// Mirror plan.rs; the adapters re-clamp defensively because the gate ran through `prepare_tx`
/// freezing.
const GATE_TIMEOUT_DEFAULT_SECS: i64 = 1800;
const GATE_TIMEOUT_MAX_SECS: i64 = 7200;

/// The record + go-token write must complete within this or the held group is killed and the op
/// fails `gate-infra`.
pub(crate) const RELEASE_TIMEOUT: Duration = Duration::from_secs(60);

/// Live timeout enforcement stays with the observer; the parked deadline is the backstop for a
/// dead observer.
pub(crate) const PARKED_DEADLINE_SLACK_SECS: i64 = 120;

/// Wire-compatible mirror of plan.rs's validated `gate` shape (stored verbatim in `tasks.gate_json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateSpec {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<i64>,
    pub steps: Vec<GateStep>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateStep {
    pub name: String,
    pub cmd: String,
}

impl GateSpec {
    /// A task row's stored `gate_json`; a row that does not parse is a kernel defect.
    pub(crate) fn parse(task_id: &str, gate_json: &str) -> Result<Self> {
        serde_json::from_str(gate_json)
            .map_err(|e| CalmError::Internal(format!("task {task_id} gate_json: {e}")))
    }

    pub fn timeout_secs_clamped(&self) -> i64 {
        self.timeout_secs
            .unwrap_or(GATE_TIMEOUT_DEFAULT_SECS)
            .clamp(1, GATE_TIMEOUT_MAX_SECS)
    }

    /// When a parked op of this gate is overdue: the live timeout plus the slack the observer
    /// needs to report it.
    pub(crate) fn parked_deadline_ms(&self, now_ms: i64) -> i64 {
        now_ms + (self.timeout_secs_clamped() + PARKED_DEADLINE_SLACK_SECS) * 1000
    }
}

/// `status_detail` is `None` on green, else `gate-red` / `gate-timeout` / `gate-infra` /
/// `gate-target-mismatch` (the fourth value is produced only by a target check: task-verify's
/// `target::finalize` and prepare-time refusal, and the gate run's `finalize_run`). The
/// task-verify target rides on `TaskGateResult`, never here (D10, A32).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateVerdict {
    pub passed: bool,
    #[serde(default)]
    pub status_detail: Option<String>,
    #[serde(default)]
    pub failing_step: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub log_tail: String,
    pub log_path: String,
    pub attempt: i64,
}

/// The four files of one held wrapper, all under `<data_dir>/gate-logs` and named by one stem
/// (`<task>-g<N>` for a kernel gate, `<task>-r<N>` for a run).
#[derive(Clone, Debug)]
pub(crate) struct GatePaths {
    pub script: PathBuf,
    pub log: PathBuf,
    pub exit: PathBuf,
    pub step: PathBuf,
}

impl GatePaths {
    pub(crate) fn new(dir: &Path, stem: &str) -> Self {
        Self {
            script: dir.join(format!("{stem}.sh")),
            log: dir.join(format!("{stem}.log")),
            exit: dir.join(format!("{stem}.exit")),
            step: dir.join(format!("{stem}.step")),
        }
    }

    /// Unlink the stale exit and step files: strictly after the kills, strictly before the spawn.
    pub(crate) async fn unlink_stale(&self, dir: &Path) -> Result<()> {
        tokio::fs::create_dir_all(dir).await?;
        for stale in [
            &self.exit,
            &PathBuf::from(format!("{}.tmp", self.exit.display())),
            &self.step,
        ] {
            match tokio::fs::remove_file(stale).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

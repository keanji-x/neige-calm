//! The gate lifecycle both gate kinds share (#2464): the declared gate, the files one held wrapper
//! is written to and writes, its spawn with the identity triple read back, and the kills. The
//! kernel gate (`task-verify`) and the worker-requested run (`task-gate-run`) each own their
//! admission, release order and completion; nothing here decides a verdict.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::SpawnArtifacts;
use super::gate_process::{GateEvidence, GateFiles, kill, spawn_held};
use crate::error::{CalmError, Result};
use crate::proc_identity::{read_boot_id, read_proc_start_time, signal_process_group};

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

    fn files(&self) -> GateFiles<'_> {
        GateFiles {
            script: &self.script,
            log: &self.log,
            exit: &self.exit,
            step: &self.step,
        }
    }
}

/// Spawn the held wrapper (`gate_process::spawn_held`) and read its identity triple back. The
/// wrapper executes nothing until it is released; an identity that cannot be read kills its group.
pub(crate) async fn spawn_held_identified(
    repo: &dyn crate::db::RouteRepo,
    cwd: &Path,
    steps: &[GateStep],
    paths: &GatePaths,
    op_marker: &str,
) -> Result<(tokio::process::Child, SpawnArtifacts)> {
    let child = spawn_held(repo, cwd, steps, paths.files(), op_marker).await?;
    let pid = child.id().map(|p| p as i32).ok_or_else(|| {
        CalmError::Internal("gate wrapper exited before pid could be read".into())
    })?;
    let identity = read_proc_start_time(pid)
        .ok_or_else(|| CalmError::Internal(format!("gate wrapper pid {pid}: starttime unreadable")))
        .and_then(|start_time| {
            read_boot_id()
                .map(|boot_id| (start_time, boot_id))
                .ok_or_else(|| CalmError::Internal("boot_id unreadable".into()))
        });
    let (start_time, boot_id) = match identity {
        Ok(identity) => identity,
        Err(error) => {
            signal_process_group(pid, libc::SIGKILL);
            return Err(error);
        }
    };
    let artifacts = SpawnArtifacts {
        pid,
        pgid: pid,
        start_time,
        boot_id,
        log_path: Some(paths.log.display().to_string()),
        extra: json!({
            "exit_path": paths.exit.display().to_string(),
            "step_path": paths.step.display().to_string(),
            "script_path": paths.script.display().to_string(),
        }),
    };
    Ok((child, artifacts))
}

/// Kill the recorded gate group iff the identity triple still matches (verify-fail → skip; ESRCH
/// swallowed).
pub(crate) fn kill_recorded_group(pid: i64, start_time: i64, boot_id: &str, pgid: i64) {
    let (Ok(pid), Ok(pgid)) = (i32::try_from(pid), i32::try_from(pgid)) else {
        return;
    };
    let Ok(start_time) = u64::try_from(start_time) else {
        return;
    };
    kill(&SpawnArtifacts {
        pid,
        pgid,
        start_time,
        boot_id: boot_id.to_string(),
        log_path: None,
        extra: Value::Null,
    });
}

pub(crate) fn exit_path_from_artifacts(artifacts: &SpawnArtifacts) -> Option<PathBuf> {
    artifacts
        .extra
        .get("exit_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

/// The evidence a recovery reads for recorded work: its log, its step file and the steps the
/// wrapper ran.
pub(crate) fn evidence_from_artifacts(
    artifacts: &SpawnArtifacts,
    steps: Vec<GateStep>,
) -> GateEvidence {
    GateEvidence {
        log_path: artifacts
            .log_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_default(),
        step_path: artifacts
            .extra
            .get("step_path")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        steps,
    }
}

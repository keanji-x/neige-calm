//! `task-verify` operation — the kernel gate runner (issue #644 PR-C,
//! reformulated on the #653 parked-operations primitive).
//!
//! One operation per gate **attempt**, idempotency key
//! `"{task.id}#g{N}"`. The saga guarantees at-least-once *start*;
//! everything after the spawn is owned by the durable parked op row +
//! the runtime-spawned exit observer:
//!
//! * `prepare_tx` — guarded `gate_attempt` bump (`N-1 → N`, only while
//!   the row is `verifying`) + freezes the gate definition, resolved
//!   cwd (`gate.cwd → task.cwd → waves.cwd`, design §6.4) and attempt
//!   into `tx_output.data`. The gate that runs is the one recorded.
//! * `spawn_side_effect` — kill-prior (own-row artifacts per the #653
//!   §3.2 MUST, the previous attempt's op artifacts, and the tasks-row
//!   pid triple), unlink the stale exit file, spawn the POSIX wrapper
//!   **held** at a stdin handshake (`read -r _go || exit 75`), record
//!   the `(pid, starttime, boot_id)` identity on the tasks row AND as
//!   op spawn artifacts, release the go-token, and park the op with an
//!   exit observer. Every gate process that can execute a step is
//!   recorded before release — there is no fork-window orphan.
//! * The **observer** (runtime-spawned only after the park committed,
//!   #653 §3.1) waits the child (group-killing at `timeout_secs`),
//!   derives the verdict, and lands op completion + the
//!   `verifying → done|failed` task flip + `Event::TaskGateResult` +
//!   the §3 lifecycle promotion in ONE tx — gated on
//!   `ParkedCompletion::Completed` (#653 §3.3 write-gate contract).
//! * `recover_parked` — exit file first (a durable verdict beats
//!   liveness), then mode-dependent: boot reattach for a healthy
//!   running gate, infra-fail for dead work with no verdict, timeout
//!   fail past deadline.
//!
//! Gate red / timeout / infra all land `failed` — "gate didn't prove
//! green" is the invariant (§6.3); the spec re-plans.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::db::sqlite::{
    begin_immediate_tx, events_append_for_operation_tx, task_apply_gate_result_tx,
    task_gate_attempt_bump_tx, task_get_tx,
};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CoveId, WaveId};
use crate::model::{TaskStatus, now_ms};
use crate::proc_identity::{
    read_boot_id, read_proc_start_time, signal_process_group, verify_owned_pid,
};
use crate::wave_lifecycle::auto_transition_if_current_in_tx;

use super::{
    CompensationStateVersioned, CompensationStep, Operation, OperationCompletionBus, OperationKey,
    ParkedCompletion, ParkedOutcome, ParkedRecovery, PhaseTag, ProviderAdapter, RecoveryMode,
    SpawnArtifacts, SpawnCtx, SpawnOutcome, TxOutput, complete_parked_tx,
};

pub const TASK_VERIFY_KIND: &str = "task-verify";

/// Default / cap mirror plan.rs rule 7; the adapter re-clamps
/// defensively because the gate ran through `prepare_tx` freezing.
const GATE_TIMEOUT_DEFAULT_SECS: i64 = 1800;
const GATE_TIMEOUT_MAX_SECS: i64 = 7200;

/// Kernel-side release-handshake timeout (design §6.2 steps 3-4): the
/// record + go-token write must complete within this or the held group
/// is killed and the op fails `gate-infra`.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(60);

/// Slack added to `timeout_secs` for the parked deadline (#653 §6.1
/// step 6): live timeout enforcement stays with the observer; the
/// parked deadline is the backstop for a dead observer.
const PARKED_DEADLINE_SLACK_SECS: i64 = 120;

/// Trailing log bytes copied into `gate_result_json` and the event.
const LOG_TAIL_BYTES: u64 = 8 * 1024;

/// Reattach-observer liveness poll cadence (#653 §6.3 — a non-child
/// cannot be `waitpid`ed; polling + exit-file is the only
/// cross-restart observation).
const REATTACH_POLL: Duration = Duration::from_secs(2);

const TASK_VERIFY_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];

/// Deterministic payload — a pure function of the frozen task row, so
/// a post-crash resubmit always idempotency-matches.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskVerifyOperationPayload {
    pub actor: ActorId,
    pub wave_id: String,
    pub task_id: String,
    pub attempt: i64,
}

/// Wire-compatible mirror of plan.rs's validated `gate` shape (stored
/// verbatim in `tasks.gate_json`).
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
    pub fn timeout_secs_clamped(&self) -> i64 {
        self.timeout_secs
            .unwrap_or(GATE_TIMEOUT_DEFAULT_SECS)
            .clamp(1, GATE_TIMEOUT_MAX_SECS)
    }
}

/// The machine verdict of one gate attempt. `status_detail` is `None`
/// on green, else `gate-red` / `gate-timeout` / `gate-infra` — the
/// task row's `status_detail` vocabulary (§3/§6.3).
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

/// Everything the gate-result tx needs to address the task row and the
/// wave-scoped event.
#[derive(Clone, Debug)]
pub(crate) struct GateResultCtx {
    pub task_id: String,
    pub wave_id: WaveId,
    pub cove_id: CoveId,
}

/// The ONE gate-result body (design §3 / §6.5): guarded
/// `verifying → done|failed` flip + `Event::TaskGateResult` + the
/// lifecycle promotion (`Working → Reviewing` — on ANY verdict, green
/// or red: either way there is now something to review), all appended
/// in the caller's tx. Returns the post-commit broadcast envelopes, or
/// an empty vec when the guard missed (task moved on / superseded
/// attempt) — in which case NOTHING else was written.
pub(crate) async fn apply_gate_result_in_tx(
    tx: &mut super::Tx<'_>,
    rctx: &GateResultCtx,
    verdict: &GateVerdict,
) -> Result<Vec<BroadcastEnvelope>> {
    let rows = task_apply_gate_result_tx(
        tx,
        &rctx.task_id,
        verdict.attempt,
        verdict.passed,
        verdict.status_detail.as_deref(),
        &serde_json::to_string(verdict)?,
        now_ms(),
    )
    .await?;
    if rows == 0 {
        return Ok(Vec::new());
    }
    let scope = EventScope::Wave {
        wave: rctx.wave_id.clone(),
        cove: rctx.cove_id.clone(),
    };
    let mut events = vec![Event::TaskGateResult {
        task_id: rctx.task_id.clone(),
        idempotency_key: rctx.task_id.clone(),
        passed: verdict.passed,
        failing_step: verdict.failing_step.clone(),
        exit_code: verdict.exit_code,
        log_tail: verdict.log_tail.clone(),
        log_path: verdict.log_path.clone(),
        attempt: verdict.attempt,
        agent_message: None,
    }];
    if let Some(auto_events) = auto_transition_if_current_in_tx(
        tx,
        &rctx.wave_id,
        crate::model::WaveLifecycle::Working,
        crate::model::WaveLifecycle::Reviewing,
        &ActorId::KernelDispatcher,
        Some("[auto] gate result recorded".to_string()),
    )
    .await?
    {
        events.extend(auto_events);
    }
    let ids = events_append_for_operation_tx(tx, &ActorId::KernelDispatcher, &scope, None, &events)
        .await?;
    Ok(ids
        .into_iter()
        .zip(events)
        .map(|(id, event)| BroadcastEnvelope {
            id,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::KernelDispatcher,
            scope: scope.clone(),
            event,
        })
        .collect())
}

/// Complete the parked op AND apply the consumer writes in one tx,
/// honoring the #653 §3.3 write gate: on `AlreadyResolved` nothing is
/// written (rollback) — the scheduler's reconcile copies the
/// enforcement outcome to the row instead.
pub(crate) async fn complete_gate_op_with_result(
    pool: &sqlx::SqlitePool,
    completion: &OperationCompletionBus,
    events: &EventBus,
    op_id: &str,
    rctx: &GateResultCtx,
    verdict: &GateVerdict,
) -> Result<()> {
    let outcome = ParkedOutcome::Succeeded {
        result: serde_json::to_value(verdict)?,
    };
    let mut tx = begin_immediate_tx(pool).await?;
    match complete_parked_tx(&mut tx, &op_id.to_string(), &outcome).await? {
        ParkedCompletion::Completed(result) => {
            let envelopes = apply_gate_result_in_tx(&mut tx, rctx, verdict).await?;
            tx.commit().await?;
            completion.complete(result);
            for envelope in envelopes {
                events.emit_envelope(envelope);
            }
            Ok(())
        }
        ParkedCompletion::AlreadyResolved { phase } => {
            tx.rollback().await?;
            tracing::debug!(
                op_id,
                task_id = %rctx.task_id,
                phase = ?phase,
                "gate observer: op already resolved; verdict discarded (enforcement won)"
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Wrapper script
// ---------------------------------------------------------------------------

/// POSIX single-quote escaping: `'` → `'\''`.
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Render the per-attempt POSIX wrapper (design §6.2 step 2 + #653
/// §6.1 step 3). First action is the release handshake — kernel death
/// before release EOFs the pipe and the held child exits 75 having
/// executed nothing. The exit code is written via tmp + `rename(2)` as
/// the last action so a mid-write SIGKILL leaves no file, never a
/// truncated one. The exit path rides the `NEIGE_GATE_EXIT_PATH` env
/// var to avoid path quoting in the script body.
pub(crate) fn render_gate_wrapper(steps: &[GateStep]) -> String {
    let mut script = String::new();
    script.push_str("#!/bin/sh\n");
    script.push_str("# generated by neige-calm task-verify; do not edit\n");
    script.push_str("read -r _go || exit 75\n");
    script.push_str("neige_gate_finish() {\n");
    script.push_str(
        "  printf '%s\\n' \"$1\" > \"$NEIGE_GATE_EXIT_PATH.tmp\" && \
         mv -f -- \"$NEIGE_GATE_EXIT_PATH.tmp\" \"$NEIGE_GATE_EXIT_PATH\"\n",
    );
    script.push_str("  exit \"$1\"\n");
    script.push_str("}\n");
    for step in steps {
        script.push_str(&format!(
            "printf '%s\\n' {}\n",
            sh_single_quote(&format!("::gate-step {}", step.name))
        ));
        script.push_str(&step.cmd);
        script.push('\n');
        script.push_str("neige_gate_rc=$?\n");
        script.push_str(
            "if [ \"$neige_gate_rc\" -ne 0 ]; then neige_gate_finish \"$neige_gate_rc\"; fi\n",
        );
    }
    script.push_str("neige_gate_finish 0\n");
    script
}

// ---------------------------------------------------------------------------
// Verdict derivation
// ---------------------------------------------------------------------------

/// Last `::gate-step <name>` sentinel in the log, if any.
fn last_gate_step_sentinel(log_text: &str) -> Option<String> {
    log_text
        .lines()
        .filter_map(|line| line.strip_prefix("::gate-step "))
        .next_back()
        .map(|s| s.trim().to_string())
}

fn read_log_tail(log_path: &Path) -> (String, Option<String>) {
    let Ok(bytes) = std::fs::read(log_path) else {
        return (String::new(), None);
    };
    let text = String::from_utf8_lossy(&bytes);
    let sentinel = last_gate_step_sentinel(&text);
    let tail_start = bytes.len().saturating_sub(LOG_TAIL_BYTES as usize);
    let tail = String::from_utf8_lossy(&bytes[tail_start..]).to_string();
    (tail, sentinel)
}

/// Parse the wrapper-written exit file. `Ok(None)` = absent;
/// `Err(())` = present but unparseable — a foreign artifact (§6.1 step
/// 3 tmp+rename excludes truncation), fail loudly rather than guess.
fn read_exit_file(exit_path: &Path) -> std::result::Result<Option<i32>, ()> {
    match std::fs::read_to_string(exit_path) {
        Ok(content) => content.trim().parse::<i32>().map(Some).map_err(|_| ()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        // Unreadable-but-present is indistinguishable from foreign.
        Err(_) => Err(()),
    }
}

/// Derive the verdict from a wrapper exit code (the exit file's value,
/// or the wait status of a wrapper that skipped its exit file).
fn verdict_from_exit_code(exit_code: i32, log_path: &Path, attempt: i64) -> GateVerdict {
    let (log_tail, sentinel) = read_log_tail(log_path);
    if exit_code == 0 {
        return GateVerdict {
            passed: true,
            status_detail: None,
            failing_step: None,
            exit_code: Some(0),
            log_tail,
            log_path: log_path.display().to_string(),
            attempt,
        };
    }
    // A wrapper exit with no `::gate-step` sentinel never ran a step
    // (e.g. the handshake `read` hit EOF → exit 75) → infra, not red.
    let status_detail = if sentinel.is_some() {
        "gate-red"
    } else {
        "gate-infra"
    };
    GateVerdict {
        passed: false,
        status_detail: Some(status_detail.into()),
        failing_step: sentinel,
        exit_code: Some(exit_code),
        log_tail,
        log_path: log_path.display().to_string(),
        attempt,
    }
}

fn infra_verdict(reason: &str, log_path: &Path, attempt: i64) -> GateVerdict {
    let (mut log_tail, sentinel) = read_log_tail(log_path);
    if log_tail.is_empty() {
        log_tail = reason.to_string();
    }
    GateVerdict {
        passed: false,
        status_detail: Some("gate-infra".into()),
        failing_step: sentinel,
        exit_code: None,
        log_tail,
        log_path: log_path.display().to_string(),
        attempt,
    }
}

fn timeout_verdict(log_path: &Path, attempt: i64, timeout_secs: i64) -> GateVerdict {
    let (mut log_tail, sentinel) = read_log_tail(log_path);
    if log_tail.is_empty() {
        log_tail = format!("gate timed out after {timeout_secs}s");
    }
    GateVerdict {
        passed: false,
        status_detail: Some("gate-timeout".into()),
        failing_step: sentinel,
        exit_code: None,
        log_tail,
        log_path: log_path.display().to_string(),
        attempt,
    }
}

// ---------------------------------------------------------------------------
// Frozen tx_output.data shape
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FrozenVerify {
    pub task_id: String,
    pub wave_id: String,
    pub cove_id: String,
    pub key: String,
    pub attempt: i64,
    pub cwd: String,
    pub gate: GateSpec,
}

impl FrozenVerify {
    pub(crate) fn from_output(output: &TxOutput) -> Result<Self> {
        serde_json::from_value(output.data.clone()).map_err(|e| {
            CalmError::Internal(format!("task-verify tx_output.data unparseable: {e}"))
        })
    }

    fn result_ctx(&self) -> GateResultCtx {
        GateResultCtx {
            task_id: self.task_id.clone(),
            wave_id: WaveId::from(self.wave_id.clone()),
            cove_id: CoveId::from(self.cove_id.clone()),
        }
    }
}

/// Parse the attempt number out of `"{task.id}#g{N}"`.
pub(crate) fn parse_attempt_key(idempotency_key: &str) -> Option<(&str, i64)> {
    let (task_id, n) = idempotency_key.rsplit_once("#g")?;
    let attempt = n.parse::<i64>().ok()?;
    (attempt >= 1 && !task_id.is_empty()).then_some((task_id, attempt))
}

/// Compose the per-attempt idempotency key.
pub fn gate_attempt_key(task_id: &str, attempt: i64) -> String {
    format!("{task_id}#g{attempt}")
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

pub struct TaskVerifyAdapter {
    /// `<data_dir>/gate-logs` — wrapper scripts, logs, exit files.
    gate_logs_dir: PathBuf,
}

impl TaskVerifyAdapter {
    pub fn new(gate_logs_dir: PathBuf) -> Self {
        Self { gate_logs_dir }
    }

    /// Resolve the gate-logs dir for call sites without a `Config`
    /// (test `AppState::from_parts`, the dispatcher test runtime, the
    /// MCP `plan/<key>/gate.log` view): `NEIGE_GATE_LOGS_DIR` env
    /// override, else `$CALM_DATA_DIR/gate-logs` (the env spelling of
    /// `Config::data_dir`), else the same XDG chain
    /// `Config::data_dir_resolved` uses, joined `gate-logs`. A
    /// `--data-dir` CLI flag without the env var is the one divergence
    /// (the config-ful `AppState::new` site passes the resolved dir
    /// explicitly and is unaffected).
    pub fn default_gate_logs_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("NEIGE_GATE_LOGS_DIR") {
            return PathBuf::from(dir);
        }
        if let Some(dir) = std::env::var_os("CALM_DATA_DIR") {
            return PathBuf::from(dir).join("gate-logs");
        }
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("neige-calm").join("gate-logs")
    }

    fn script_path(&self, task_id: &str, attempt: i64) -> PathBuf {
        self.gate_logs_dir.join(format!("{task_id}-g{attempt}.sh"))
    }

    fn log_path(&self, task_id: &str, attempt: i64) -> PathBuf {
        self.gate_logs_dir.join(format!("{task_id}-g{attempt}.log"))
    }

    fn exit_path(&self, task_id: &str, attempt: i64) -> PathBuf {
        self.gate_logs_dir
            .join(format!("{task_id}-g{attempt}.exit"))
    }
}

/// Kill the recorded gate group iff the identity triple still matches
/// (double-kill-safe: verify-fail → skip; ESRCH swallowed).
fn kill_recorded_group(pid: i64, start_time: i64, boot_id: &str, pgid: i64) {
    let (Ok(pid), Ok(pgid)) = (i32::try_from(pid), i32::try_from(pgid)) else {
        return;
    };
    let Ok(start_time) = u64::try_from(start_time) else {
        return;
    };
    if verify_owned_pid(pid, start_time, boot_id) {
        signal_process_group(pgid, libc::SIGKILL);
    }
}

fn kill_artifacts_group(artifacts: &SpawnArtifacts) {
    if verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id) {
        signal_process_group(artifacts.pgid, libc::SIGKILL);
    }
}

fn exit_path_from_artifacts(artifacts: &SpawnArtifacts) -> Option<PathBuf> {
    artifacts
        .extra
        .get("exit_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn log_path_from_artifacts(artifacts: &SpawnArtifacts) -> PathBuf {
    artifacts
        .log_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[async_trait]
impl ProviderAdapter for TaskVerifyAdapter {
    fn kind(&self) -> &'static str {
        TASK_VERIFY_KIND
    }

    fn phases(&self) -> &'static [PhaseTag] {
        TASK_VERIFY_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: TaskVerifyOperationPayload = serde_json::from_value(input.clone())?;
        if payload.task_id.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "task-verify task_id must not be empty".into(),
            ));
        }
        if payload.attempt < 1 {
            return Err(CalmError::BadRequest(format!(
                "task-verify attempt must be >= 1, got {}",
                payload.attempt
            )));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut super::Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TaskVerifyOperationPayload = serde_json::from_value(input.clone())?;
        let key = op
            .idempotency_key
            .as_deref()
            .and_then(parse_attempt_key)
            .ok_or_else(|| {
                CalmError::BadRequest(format!(
                    "task-verify idempotency key must be \"{{task_id}}#g{{N}}\", got {:?}",
                    op.idempotency_key
                ))
            })?;
        if key != (payload.task_id.as_str(), payload.attempt) {
            return Err(CalmError::BadRequest(
                "task-verify idempotency key does not match payload".into(),
            ));
        }
        let attempt = payload.attempt;

        let task = task_get_tx(tx, &payload.task_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("task {}", payload.task_id)))?;
        if task.status != TaskStatus::Verifying {
            return Err(CalmError::Conflict(format!(
                "task {} is not verifying (status {:?}); gate attempt {attempt} is moot",
                task.id, task.status
            )));
        }
        let gate_json = task
            .gate_json
            .as_deref()
            .ok_or_else(|| CalmError::Conflict(format!("task {} declares no gate", task.id)))?;
        let gate: GateSpec = serde_json::from_str(gate_json)
            .map_err(|e| CalmError::Internal(format!("task {} gate_json: {e}", task.id)))?;

        // §6.4 cwd chain: gate.cwd → task.cwd → waves.cwd.
        let wave: Option<(String, String)> =
            sqlx::query_as("SELECT cwd, cove_id FROM waves WHERE id = ?1")
                .bind(&task.wave_id)
                .fetch_optional(&mut **tx)
                .await?;
        let (wave_cwd, cove_id) =
            wave.ok_or_else(|| CalmError::Conflict(format!("wave {} is gone", task.wave_id)))?;
        let cwd = gate
            .cwd
            .clone()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| task.cwd.clone().filter(|c| !c.trim().is_empty()))
            .unwrap_or(wave_cwd);
        if cwd.trim().is_empty() {
            return Err(CalmError::BadRequest(format!(
                "task {}: no gate cwd resolvable (gate.cwd, task.cwd, waves.cwd all empty)",
                task.id
            )));
        }

        // Guarded attempt bump: exactly one op prepares attempt N.
        let rows = task_gate_attempt_bump_tx(tx, &task.id, attempt, now_ms()).await?;
        if rows == 0 {
            return Err(CalmError::Conflict(format!(
                "task {} gate attempt {attempt} lost the bump (current attempt {} / status {:?})",
                task.id, task.gate_attempt, task.status
            )));
        }

        let frozen = FrozenVerify {
            task_id: task.id.clone(),
            wave_id: task.wave_id.clone(),
            cove_id,
            key: task.key.clone(),
            attempt,
            cwd,
            gate,
        };
        let mut output = TxOutput::new("task", Some(task.id.clone()), json!({}));
        output.data = serde_json::to_value(&frozen)?;
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<super::AppServerInteractOutcome> {
        Ok(super::AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let frozen = FrozenVerify::from_output(output)?;
        let pool = ctx.operation_repo.sqlite_pool();

        // 1. Kill prior (design §6.2 step 1 + #653 §3.2 contract item 2):
        //    (a) this op's own recorded artifacts (same-op re-drive),
        //    (b) the previous attempt's op artifacts,
        //    (c) the tasks-row pid triple.
        if let Some(artifacts) = &op.spawn_artifacts {
            kill_artifacts_group(artifacts);
        }
        if frozen.attempt > 1
            && let Some(prev) = ctx
                .operation_repo
                .find_by_idempotency_key(
                    TASK_VERIFY_KIND,
                    &OperationKey {
                        operation_key: String::new(),
                        idempotency_key: Some(gate_attempt_key(
                            &frozen.task_id,
                            frozen.attempt - 1,
                        )),
                        payload_hash: String::new(),
                    },
                )
                .await?
            && let Some(artifacts) = &prev.spawn_artifacts
        {
            kill_artifacts_group(artifacts);
        }
        let triple: Option<(Option<i64>, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT gate_pid, gate_pid_starttime, gate_pid_boot_id FROM tasks WHERE id = ?1",
        )
        .bind(&frozen.task_id)
        .fetch_optional(&pool)
        .await?;
        if let Some((Some(pid), Some(start_time), Some(boot_id))) = triple {
            kill_recorded_group(pid, start_time, &boot_id, pid);
        }

        // 2. Unlink the stale exit file (strictly after the kills,
        //    strictly before the spawn — #653 §6.1 step 2).
        let exit_path = self.exit_path(&frozen.task_id, frozen.attempt);
        let log_path = self.log_path(&frozen.task_id, frozen.attempt);
        let script_path = self.script_path(&frozen.task_id, frozen.attempt);
        tokio::fs::create_dir_all(&self.gate_logs_dir).await?;
        for stale in [
            &exit_path,
            &PathBuf::from(format!("{}.tmp", exit_path.display())),
        ] {
            match tokio::fs::remove_file(stale).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }

        // 3. Spawn, held at the handshake.
        if !Path::new(&frozen.cwd).is_dir() {
            return Err(CalmError::BadRequest(format!(
                "gate cwd {} does not exist",
                frozen.cwd
            )));
        }
        tokio::fs::write(&script_path, render_gate_wrapper(&frozen.gate.steps)).await?;
        let log_file = std::fs::File::create(&log_path)?;
        let log_file_err = log_file.try_clone()?;
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg(&script_path)
            .current_dir(&frozen.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_file_err))
            // The gate cannot write kernel state (§6.3).
            .env_remove("NEIGE_MCP_SOCKET")
            .env_remove("NEIGE_MCP_TOKEN")
            .env("NEIGE_GATE_EXIT_PATH", &exit_path)
            .kill_on_drop(true);
        if let Value::Object(env) = super::terminal_adapter::terminal_worker_env(ctx.repo.as_ref())
            .await
            .unwrap_or(Value::Null)
        {
            for (k, v) in env {
                if let Value::String(v) = v {
                    cmd.env(k, v);
                }
            }
        }
        // One process group = one kill target for wrapper + current
        // step + descendants.
        // SAFETY: setsid() is async-signal-safe and called in the
        // forked child before exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let pid = child.id().map(|p| p as i32).ok_or_else(|| {
            CalmError::Internal("gate wrapper exited before pid could be read".into())
        })?;
        let pgid = pid; // setsid → session/group leader

        // 4-5. Record then release, under the kernel-side 60s timeout.
        let record_release = async {
            let start_time = read_proc_start_time(pid).ok_or_else(|| {
                CalmError::Internal(format!("gate wrapper pid {pid}: starttime unreadable"))
            })?;
            let boot_id =
                read_boot_id().ok_or_else(|| CalmError::Internal("boot_id unreadable".into()))?;
            // Durable record on the tasks row (guarded), …
            let rows = sqlx::query(
                r#"UPDATE tasks
                   SET gate_pid = ?1, gate_pid_starttime = ?2, gate_pid_boot_id = ?3,
                       updated_at_ms = ?4
                   WHERE id = ?5 AND status = 'verifying' AND gate_attempt = ?6"#,
            )
            .bind(pgid as i64)
            .bind(start_time as i64)
            .bind(&boot_id)
            .bind(now_ms())
            .bind(&frozen.task_id)
            .bind(frozen.attempt)
            .execute(&pool)
            .await?
            .rows_affected();
            if rows == 0 {
                return Err(CalmError::Conflict(format!(
                    "task {} moved on before gate attempt {} was recorded",
                    frozen.task_id, frozen.attempt
                )));
            }
            // … AND the op spawn artifacts (#653 §3.2 hook) — both
            // BEFORE release, so every gate process that can execute a
            // step is recorded.
            let artifacts = SpawnArtifacts {
                pid,
                pgid,
                start_time,
                boot_id,
                log_path: Some(log_path.display().to_string()),
                extra: json!({
                    "exit_path": exit_path.display().to_string(),
                    "script_path": script_path.display().to_string(),
                }),
            };
            ctx.record_spawn_artifacts(op, &artifacts).await?;
            // Release the go-token (newline-terminated — POSIX `read`
            // returns non-zero on EOF-before-newline).
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| CalmError::Internal("gate wrapper stdin handle missing".into()))?;
            stdin
                .write_all(b"go\n")
                .await
                .map_err(|e| CalmError::Internal(format!("gate release write failed: {e}")))?;
            drop(stdin);
            Ok::<SpawnArtifacts, CalmError>(artifacts)
        };
        let artifacts = match tokio::time::timeout(RELEASE_TIMEOUT, record_release).await {
            Ok(Ok(artifacts)) => artifacts,
            Ok(Err(e)) => {
                // Kill the held child; its handshake `read` may also
                // already have EOF'd via the dropped stdin.
                signal_process_group(pgid, libc::SIGKILL);
                return Err(e);
            }
            Err(_) => {
                signal_process_group(pgid, libc::SIGKILL);
                return Err(CalmError::Internal(
                    "gate-infra: record/release did not complete within 60s".into(),
                ));
            }
        };

        // 6. Build the exit observer — the runtime spawns it only
        //    AFTER the park commits (#653 §3.1).
        let timeout_secs = frozen.gate.timeout_secs_clamped();
        let rctx = frozen.result_ctx();
        let attempt = frozen.attempt;
        let op_id = op.id.clone();
        let completion = ctx.completion.clone();
        let events = ctx.events.clone();
        let observer_pool = pool.clone();
        let observer_exit_path = exit_path.clone();
        let observer_log_path = log_path.clone();
        let observer = Box::pin(async move {
            let mut child = child;
            let wait =
                tokio::time::timeout(Duration::from_secs(timeout_secs as u64), child.wait()).await;
            let (status, timed_out) = match wait {
                Ok(status) => (status, false),
                Err(_) => {
                    // Live timeout enforcement: kill the group, then
                    // reap the wrapper.
                    if verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id) {
                        signal_process_group(artifacts.pgid, libc::SIGKILL);
                    }
                    (child.wait().await, true)
                }
            };
            // Prefer a present, parseable exit file over the wait
            // status (#653 §4.4 ordering B / §6.3): the durable
            // verdict wins even when the wrapper died to a signal
            // after renaming it.
            let verdict = match read_exit_file(&observer_exit_path) {
                Ok(Some(code)) => verdict_from_exit_code(code, &observer_log_path, attempt),
                Err(()) => infra_verdict(
                    "gate exit file present but unparseable (foreign artifact)",
                    &observer_log_path,
                    attempt,
                ),
                Ok(None) => match (timed_out, status) {
                    (true, _) => timeout_verdict(&observer_log_path, attempt, timeout_secs),
                    (false, Ok(status)) => match status.code() {
                        Some(code) => verdict_from_exit_code(code, &observer_log_path, attempt),
                        None => infra_verdict(
                            "gate wrapper killed by signal with no exit file",
                            &observer_log_path,
                            attempt,
                        ),
                    },
                    (false, Err(e)) => infra_verdict(
                        &format!("gate wrapper wait failed: {e}"),
                        &observer_log_path,
                        attempt,
                    ),
                },
            };
            if let Err(e) = complete_gate_op_with_result(
                &observer_pool,
                &completion,
                &events,
                &op_id,
                &rctx,
                &verdict,
            )
            .await
            {
                tracing::error!(
                    op_id = %op_id,
                    task_id = %rctx.task_id,
                    error = %e,
                    "gate observer: completion tx failed; sweep/reconcile will recover"
                );
            }
        });

        let deadline_ms = now_ms() + (timeout_secs + PARKED_DEADLINE_SLACK_SECS) * 1000;
        Ok(SpawnOutcome::Parked {
            deadline_ms,
            observer,
        })
    }

    /// #653 §6.3 — exit file first, liveness second.
    async fn recover_parked(
        &self,
        op: &Operation,
        artifacts: &SpawnArtifacts,
        alive: bool,
        mode: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        let frozen = op
            .tx_output
            .as_ref()
            .ok_or_else(|| CalmError::Internal("task-verify op missing tx_output".into()))
            .and_then(FrozenVerify::from_output)?;
        let exit_path = exit_path_from_artifacts(artifacts)
            .unwrap_or_else(|| self.exit_path(&frozen.task_id, frozen.attempt));
        let log_path = log_path_from_artifacts(artifacts);
        match read_exit_file(&exit_path) {
            Ok(Some(code)) => {
                let verdict = verdict_from_exit_code(code, &log_path, frozen.attempt);
                return Ok(ParkedRecovery::Complete(ParkedOutcome::Succeeded {
                    result: serde_json::to_value(&verdict)?,
                }));
            }
            Err(()) => {
                return Ok(ParkedRecovery::Fail {
                    reason: "gate exit file present but unparseable (foreign artifact); gate-infra"
                        .into(),
                });
            }
            Ok(None) => {}
        }
        if !alive {
            return Ok(ParkedRecovery::Fail {
                reason: "gate process dead with no recorded verdict; gate-infra".into(),
            });
        }
        match mode {
            RecoveryMode::Boot => {
                // Re-attach: a healthy running gate survives the
                // kernel restart. A non-child cannot be waitpid'ed —
                // poll the identity triple until it dies, then read
                // the exit file; complete via the same
                // Completed-gated one-tx body.
                let pool = ctx.operation_repo.sqlite_pool();
                let completion = ctx.completion.clone();
                let events = ctx.events.clone();
                let op_id = op.id.clone();
                let rctx = frozen.result_ctx();
                let attempt = frozen.attempt;
                let artifacts = artifacts.clone();
                let log_path = log_path.clone();
                tokio::spawn(async move {
                    loop {
                        if !verify_owned_pid(
                            artifacts.pid,
                            artifacts.start_time,
                            &artifacts.boot_id,
                        ) {
                            break;
                        }
                        tokio::time::sleep(REATTACH_POLL).await;
                    }
                    let verdict = match read_exit_file(&exit_path) {
                        Ok(Some(code)) => verdict_from_exit_code(code, &log_path, attempt),
                        Ok(None) => infra_verdict(
                            "reattached gate exited with no exit file",
                            &log_path,
                            attempt,
                        ),
                        Err(()) => infra_verdict(
                            "gate exit file present but unparseable (foreign artifact)",
                            &log_path,
                            attempt,
                        ),
                    };
                    if let Err(e) = complete_gate_op_with_result(
                        &pool,
                        &completion,
                        &events,
                        &op_id,
                        &rctx,
                        &verdict,
                    )
                    .await
                    {
                        tracing::error!(
                            op_id = %op_id,
                            error = %e,
                            "gate reattach observer: completion tx failed"
                        );
                    }
                });
                Ok(ParkedRecovery::LeaveParked)
            }
            // §4.4 only probes dead work pre-deadline; defensive.
            RecoveryMode::PreDeadlineProbe => Ok(ParkedRecovery::LeaveParked),
            // The caller kills the group next and runs the post-kill
            // re-check; spawning a reattach observer here would watch
            // a corpse and double-report.
            RecoveryMode::PastDeadline => Ok(ParkedRecovery::Fail {
                reason: "gate timeout (parked deadline exceeded)".into(),
            }),
        }
    }

    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let frozen = FrozenVerify::from_output(output)?;
        let artifacts = op
            .spawn_artifacts
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(Value::Null);
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase: _from_phase,
            reason: reason.to_string(),
            steps: vec![
                CompensationStep {
                    op: "kill_gate_group".into(),
                    args: json!({ "artifacts": artifacts, "task_id": frozen.task_id }),
                    completed: false,
                    attempts: 0,
                    last_error: None,
                },
                CompensationStep {
                    op: "fail_task_gate_infra".into(),
                    args: json!({
                        "task_id": frozen.task_id,
                        "wave_id": frozen.wave_id,
                        "cove_id": frozen.cove_id,
                        "attempt": frozen.attempt,
                        "reason": reason,
                    }),
                    completed: false,
                    attempts: 0,
                    last_error: None,
                },
            ],
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        match step.op.as_str() {
            "kill_gate_group" => {
                if let Some(artifacts) = step.args.get("artifacts").filter(|v| !v.is_null()) {
                    let artifacts: SpawnArtifacts = serde_json::from_value(artifacts.clone())?;
                    kill_artifacts_group(&artifacts);
                }
                // Belt-and-suspenders: the tasks-row triple (recorded
                // before release) covers the window where the op-row
                // artifacts never committed.
                if let Some(task_id) = step.args.get("task_id").and_then(Value::as_str) {
                    let pool = ctx.operation_repo.sqlite_pool();
                    let triple: Option<(Option<i64>, Option<i64>, Option<String>)> =
                        sqlx::query_as(
                            "SELECT gate_pid, gate_pid_starttime, gate_pid_boot_id \
                             FROM tasks WHERE id = ?1",
                        )
                        .bind(task_id)
                        .fetch_optional(&pool)
                        .await?;
                    if let Some((Some(pid), Some(start_time), Some(boot_id))) = triple {
                        kill_recorded_group(pid, start_time, &boot_id, pid);
                    }
                }
                Ok(())
            }
            "fail_task_gate_infra" => {
                let task_id = step
                    .args
                    .get("task_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CalmError::Internal("fail_task_gate_infra missing task_id".into())
                    })?;
                let wave_id = step
                    .args
                    .get("wave_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let cove_id = step
                    .args
                    .get("cove_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let attempt = step
                    .args
                    .get("attempt")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let reason = step
                    .args
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("gate-infra");
                let rctx = GateResultCtx {
                    task_id: task_id.to_string(),
                    wave_id: WaveId::from(wave_id.to_string()),
                    cove_id: CoveId::from(cove_id.to_string()),
                };
                let verdict = GateVerdict {
                    passed: false,
                    status_detail: Some("gate-infra".into()),
                    failing_step: None,
                    exit_code: None,
                    log_tail: reason.to_string(),
                    log_path: self.log_path(task_id, attempt).display().to_string(),
                    attempt,
                };
                let pool = ctx.operation_repo.sqlite_pool();
                let mut tx = begin_immediate_tx(&pool).await?;
                let envelopes = apply_gate_result_in_tx(&mut tx, &rctx, &verdict).await?;
                tx.commit().await?;
                for envelope in envelopes {
                    ctx.events.emit_envelope(envelope);
                }
                Ok(())
            }
            other => Err(CalmError::Internal(format!(
                "task-verify unknown compensation step {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_key_round_trip() {
        assert_eq!(gate_attempt_key("w:impl", 3), "w:impl#g3");
        assert_eq!(parse_attempt_key("w:impl#g3"), Some(("w:impl", 3)));
        // Task keys may contain '#g' lookalikes only via the wave id /
        // key alphabet — keys are [a-z0-9._-], so the LAST '#g' is
        // always the attempt separator.
        assert_eq!(parse_attempt_key("w:impl#g0"), None, "attempt >= 1");
        assert_eq!(parse_attempt_key("w:impl"), None);
        assert_eq!(parse_attempt_key("#g2"), None, "empty task id");
        assert_eq!(parse_attempt_key("w:impl#gx"), None);
    }

    #[test]
    fn wrapper_holds_at_handshake_and_writes_sentinels() {
        let steps = vec![
            GateStep {
                name: "fmt".into(),
                cmd: "cargo fmt --check".into(),
            },
            GateStep {
                name: "it's-quoted".into(),
                cmd: "true".into(),
            },
        ];
        let script = render_gate_wrapper(&steps);
        // Handshake is the FIRST action — nothing executes before it.
        let first_action = script
            .lines()
            .find(|l| !l.starts_with('#') && !l.trim().is_empty())
            .unwrap();
        assert_eq!(first_action, "read -r _go || exit 75");
        assert!(script.contains("'::gate-step fmt'"));
        // Single quotes in step names are escaped, not script-breaking.
        assert!(script.contains("'::gate-step it'\\''s-quoted'"));
        assert!(script.contains("cargo fmt --check\n"));
        // Exit file lands via tmp + rename, and the wrapper always
        // finishes through the helper.
        assert!(
            script.contains("mv -f -- \"$NEIGE_GATE_EXIT_PATH.tmp\" \"$NEIGE_GATE_EXIT_PATH\"")
        );
        assert!(script.trim_end().ends_with("neige_gate_finish 0"));
    }

    #[test]
    fn verdict_classification() {
        let dir = std::env::temp_dir().join(format!("gate-verdict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("v.log");

        // Green.
        std::fs::write(&log, "::gate-step fmt\nok\n").unwrap();
        let v = verdict_from_exit_code(0, &log, 1);
        assert!(v.passed);
        assert_eq!(v.status_detail, None);
        assert_eq!(v.failing_step, None);

        // Red with sentinel → gate-red + failing step attribution.
        std::fs::write(&log, "::gate-step fmt\nok\n::gate-step test\nboom\n").unwrap();
        let v = verdict_from_exit_code(101, &log, 2);
        assert!(!v.passed);
        assert_eq!(v.status_detail.as_deref(), Some("gate-red"));
        assert_eq!(v.failing_step.as_deref(), Some("test"));
        assert_eq!(v.exit_code, Some(101));
        assert_eq!(v.attempt, 2);

        // Non-zero with NO sentinel (handshake EOF exit 75) → infra.
        std::fs::write(&log, "").unwrap();
        let v = verdict_from_exit_code(75, &log, 1);
        assert!(!v.passed);
        assert_eq!(v.status_detail.as_deref(), Some("gate-infra"));

        // Timeout verdict.
        let v = timeout_verdict(&log, 1, 7);
        assert_eq!(v.status_detail.as_deref(), Some("gate-timeout"));
        assert!(v.log_tail.contains("timed out after 7s"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exit_file_parse_states() {
        let dir = std::env::temp_dir().join(format!("gate-exit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.exit");
        assert_eq!(read_exit_file(&path), Ok(None), "absent");
        std::fs::write(&path, "3\n").unwrap();
        assert_eq!(read_exit_file(&path), Ok(Some(3)));
        std::fs::write(&path, "not-a-code").unwrap();
        assert_eq!(read_exit_file(&path), Err(()), "foreign artifact");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn log_tail_caps_at_8kib_and_finds_last_sentinel() {
        let dir = std::env::temp_dir().join(format!("gate-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("big.log");
        let mut content = String::from("::gate-step first\n");
        content.push_str(&"x".repeat(20 * 1024));
        content.push_str("\n::gate-step last\ntail-end\n");
        std::fs::write(&log, &content).unwrap();
        let (tail, sentinel) = read_log_tail(&log);
        assert!(tail.len() <= LOG_TAIL_BYTES as usize);
        assert!(tail.ends_with("tail-end\n"));
        assert_eq!(sentinel.as_deref(), Some("last"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gate_timeout_clamps() {
        let mut gate = GateSpec {
            cwd: None,
            timeout_secs: None,
            steps: vec![],
        };
        assert_eq!(gate.timeout_secs_clamped(), GATE_TIMEOUT_DEFAULT_SECS);
        gate.timeout_secs = Some(999_999);
        assert_eq!(gate.timeout_secs_clamped(), GATE_TIMEOUT_MAX_SECS);
        gate.timeout_secs = Some(0);
        assert_eq!(gate.timeout_secs_clamped(), 1);
    }
}

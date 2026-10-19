//! `task-verify` operation — the kernel gate runner. One operation per gate attempt (`"{task.id}#g{N}"`); the wrapper is held at a stdin
//! handshake until park and durable authorization. Executor settlement supplies the verdict; exit files are informational.

mod display;
mod execution;
mod preparation;
pub(crate) mod regate;
mod reuse;
pub(crate) mod target;

use super::gate_lifecycle::GatePaths;
use super::gate_process::*;
use std::path::{Path, PathBuf};

pub use super::gate_lifecycle::{GateSpec, GateStep, GateVerdict};
use target::FrozenTarget;
pub use target::{SAMPLE_TIMEOUT, TaskGateResult};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::sqlite::{
    append_decision_events_in_tx, begin_immediate_tx, task_apply_gate_result_tx,
    task_gate_attempt_bump_tx, task_get_tx,
};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, AreaId, TrackId};
use crate::model::{TaskStatus, now_ms};

use super::{
    CompensationStateVersioned, CompensationStep, Operation, ParkedRecovery, PhaseTag,
    ProviderAdapter, RecoveryMode, SpawnCtx, SpawnOutcome, TxOutput,
};

pub const TASK_VERIFY_KIND: &str = "task-verify";

const TASK_VERIFY_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    // A target refused in `prepare_tx` spawns nothing: `Ready(NoOp)` lands here (#1727 S4 D3).
    PhaseTag::SpawnSucceeded,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];

/// A pure function of the frozen task row, so a post-crash resubmit always idempotency-matches.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskVerifyOperationPayload {
    pub actor: ActorId,
    pub track_id: String,
    pub task_id: String,
    pub attempt: i64,
}

/// The gate steps a codex or claude task declares, in run order, for its worker's prompt (#2404).
/// `None` when the task declares no gate or has no row.
pub(crate) async fn declared_gate_steps_tx(
    tx: &mut super::Tx<'_>,
    task_id: &str,
) -> Result<Option<Vec<GateStep>>> {
    let Some(gate_json) = task_get_tx(tx, task_id)
        .await?
        .and_then(|task| task.gate_json)
    else {
        return Ok(None);
    };
    Ok(Some(GateSpec::parse(task_id, &gate_json)?.steps))
}

#[derive(Clone, Debug)]
pub(crate) struct GateResultCtx {
    pub task_id: String,
    pub track_id: TrackId,
    pub area_id: AreaId,
}

/// The ONE gate-result body: guarded `verifying → done|failed` flip + `Event::TaskGateResult` + the `Working → Reviewing` promotion on ANY verdict.
/// Returns an empty vec when the guard missed (task moved on / superseded attempt), in which case NOTHING else was written.
pub(crate) async fn apply_gate_result_in_tx(
    tx: &mut super::Tx<'_>,
    rctx: &GateResultCtx,
    result: &TaskGateResult,
) -> Result<Vec<BroadcastEnvelope>> {
    apply_gate_result_with_guard_in_tx(tx, rctx, result, result.verdict.attempt).await
}

/// [`apply_gate_result_in_tx`] with an explicit row-guard attempt: the reconcile arm flips a row still at `verdict.attempt - 1`, the shape left when `prepare_tx` failed BEFORE the guarded bump.
pub(crate) async fn apply_gate_result_with_guard_in_tx(
    tx: &mut super::Tx<'_>,
    rctx: &GateResultCtx,
    result: &TaskGateResult,
    guard_attempt: i64,
) -> Result<Vec<BroadcastEnvelope>> {
    let verdict = &result.verdict;
    let mut recorded = result.clone();
    if recorded.cwd.is_none() {
        // The frozen cwd, when the producer did not carry it (the reconcile arm P10 has no
        // freeze; a prepare-time refusal's own freeze is not yet persisted — empty is expected).
        recorded.cwd = sqlx::query_scalar::<_, Option<String>>(
            "SELECT json_extract(tx_output_json, '$.data.cwd') FROM operations \
             WHERE kind = 'task-verify' AND idempotency_key = ?1",
        )
        .bind(gate_attempt_key(&rctx.task_id, verdict.attempt))
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
    }
    let rows = task_apply_gate_result_tx(
        tx,
        &rctx.task_id,
        guard_attempt,
        verdict.passed,
        verdict.status_detail.as_deref(),
        &serde_json::to_string(&recorded)?,
        now_ms(),
    )
    .await?;
    if rows == 0 {
        return Ok(Vec::new());
    }
    let scope = EventScope::Track {
        track: rctx.track_id.clone(),
        area: rctx.area_id.clone(),
    };
    let events = vec![Event::TaskGateResult {
        task_id: rctx.task_id.clone(),
        idempotency_key: rctx.task_id.clone(),
        passed: verdict.passed,
        failing_step: verdict.failing_step.clone(),
        exit_code: verdict.exit_code,
        log_tail: verdict.log_tail.clone(),
        log_path: verdict.log_path.clone(),
        attempt: verdict.attempt,
        agent_message: None,
        status_detail: verdict.status_detail.clone(),
        target: Some(recorded.target.clone()),
    }];
    let ids =
        append_decision_events_in_tx(tx, &ActorId::KernelDispatcher, &scope, None, &events).await?;
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FrozenVerify {
    pub task_id: String,
    pub track_id: String,
    pub area_id: String,
    pub key: String,
    pub attempt: i64,
    pub cwd: String,
    pub gate: GateSpec,
    /// Absent on an op frozen before slice 4: `Unbound { LegacyFrozen }` (D12 (c)).
    #[serde(default)]
    pub target: FrozenTarget,
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
            track_id: TrackId::from(self.track_id.clone()),
            area_id: AreaId::from(self.area_id.clone()),
        }
    }
}

/// Parse the attempt number out of `"{task.id}#g{N}"`.
pub(crate) fn parse_attempt_key(idempotency_key: &str) -> Option<(&str, i64)> {
    let (task_id, n) = idempotency_key.rsplit_once("#g")?;
    let attempt = n.parse::<i64>().ok()?;
    (attempt >= 1 && !task_id.is_empty()).then_some((task_id, attempt))
}

/// Compose the per-attempt idempotency key. A row's `gate_attempt` is the highest number it has
/// reserved: a re-run (`regate`, #2405) reserves one that no op runs, so attempts can skip one.
pub fn gate_attempt_key(task_id: &str, attempt: i64) -> String {
    format!("{task_id}#g{attempt}")
}

#[derive(Clone)]
pub struct TaskVerifyAdapter {
    /// `<data_dir>/gate-logs` — wrapper scripts, logs, exit files, step files.
    gate_logs_dir: PathBuf,
    #[cfg(test)]
    pub(crate) before_release:
        Option<std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>>,
    #[cfg(any(test, feature = "fixtures"))]
    before_completion:
        Option<std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>>,
}

impl TaskVerifyAdapter {
    pub fn new(gate_logs_dir: PathBuf) -> Self {
        Self {
            gate_logs_dir,
            #[cfg(test)]
            before_release: None,
            #[cfg(any(test, feature = "fixtures"))]
            before_completion: None,
        }
    }

    /// Fixture-only pause in the live observer after the kernel verdict, before completion commits.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn with_before_completion(
        mut self,
        hook: std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>,
    ) -> Self {
        self.before_completion = Some(hook);
        self
    }

    /// For the GENUINELY config-less call sites only; every config-ful site passes the resolved dir explicitly.
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

    fn paths(&self, task_id: &str, attempt: i64) -> GatePaths {
        GatePaths::new(&self.gate_logs_dir, &format!("{task_id}-g{attempt}"))
    }

    fn log_path(&self, task_id: &str, attempt: i64) -> PathBuf {
        self.paths(task_id, attempt).log
    }
}

#[async_trait]
impl ProviderAdapter for TaskVerifyAdapter {
    async fn preparation_track(
        &self,
        input: &Value,
        _ctx: &crate::operation::SpawnCtx,
    ) -> Result<Option<String>> {
        Ok(Some(
            serde_json::from_value::<TaskVerifyOperationPayload>(input.clone())?.track_id,
        ))
    }

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

    async fn prepare_external(
        &self,
        input: &Value,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn super::PreparedTx>>> {
        preparation::collect(self, input, op, ctx).await
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut super::Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        // Preserve the common task admission fence even for unsupported direct preparation.
        let payload: TaskVerifyOperationPayload = serde_json::from_value(input.clone())?;
        super::refuse_if_context_stale(tx, Some(&payload.task_id)).await?;
        Err(CalmError::Internal(
            "task verification requires external preparation".into(),
        ))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<super::AppServerInteractOutcome> {
        Ok(super::AppServerInteractOutcome::NotApplicable)
    }

    fn owns_parked_resource(&self) -> Option<&'static str> {
        Some(super::gate_execution::KEY)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        self.spawn_execution(output, op, ctx).await
    }

    async fn recover_owned_parked(
        &self,
        op: &Operation,
        mode: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        self.recover_execution(op, mode, ctx).await
    }

    async fn complete_owned_parked_tx(
        &self,
        tx: &mut super::Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<BroadcastEnvelope>> {
        super::gate_execution::retire(tx, op).await?;
        // The outcome is calculated from this same claim snapshot in recover_execution.
        // complete_parked_tx has already persisted it in the completion transaction.
        let output: String =
            sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
                .bind(&op.id)
                .fetch_one(&mut **tx)
                .await?;
        let output: TxOutput = serde_json::from_str(&output)?;
        let verdict: TaskGateResult = serde_json::from_value(output.result)?;
        let frozen = FrozenVerify::from_output(super::required_output(op)?)?;
        apply_gate_result_in_tx(tx, &frozen.result_ctx(), &verdict).await
    }

    fn owned_parked_completed(&self, op: &Operation, ctx: &SpawnCtx) {
        if let Ok(record) = super::gate_execution::record(op) {
            ctx.execution.drive([record.execution_id]);
        }
    }

    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        if op.spawn_artifacts.is_some() && output.data.get(super::gate_execution::KEY).is_none() {
            return Err(execution::legacy_error(op));
        }
        let frozen = FrozenVerify::from_output(output)?;
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase: _from_phase,
            reason: reason.to_string(),
            steps: vec![CompensationStep {
                op: "fail_task_gate_infra".into(),
                args: json!({
                    "task_id": frozen.task_id,
                    "track_id": frozen.track_id,
                    "area_id": frozen.area_id,
                    "attempt": frozen.attempt,
                    "reason": reason,
                    "target": target::target_json(&target::compensation_target(
                        &frozen.target,
                        reason,
                    )),
                }),
                completed: false,
                attempts: 0,
                last_error: None,
            }],
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if _op.spawn_artifacts.is_some() && output.data.get(super::gate_execution::KEY).is_none() {
            return Err(execution::legacy_error(_op));
        }
        match step.op.as_str() {
            "kill_gate_group" => Err(execution::legacy_error(_op)),
            "fail_task_gate_infra" => {
                let task_id = step
                    .args
                    .get("task_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CalmError::Internal("fail_task_gate_infra missing task_id".into())
                    })?;
                let track_id = step
                    .args
                    .get("track_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let area_id = step
                    .args
                    .get("area_id")
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
                    .unwrap_or(target::GATE_INFRA);
                let rctx = GateResultCtx {
                    task_id: task_id.to_string(),
                    track_id: TrackId::from(track_id.to_string()),
                    area_id: AreaId::from(area_id.to_string()),
                };
                // P8: the target planned with the step; a state planned before slice 4 has none
                // and falls back to the op's freeze (which reads as `LegacyFrozen` when absent).
                let target = match step.args.get("target") {
                    Some(target) => serde_json::from_value(target.clone())?,
                    None => target::compensation_target(
                        &FrozenVerify::from_output(output)?.target,
                        reason,
                    ),
                };
                let verdict = TaskGateResult {
                    verdict: GateVerdict {
                        passed: false,
                        status_detail: Some(target::GATE_INFRA.into()),
                        failing_step: None,
                        exit_code: None,
                        log_tail: reason.to_string(),
                        log_path: self.log_path(task_id, attempt).display().to_string(),
                        attempt,
                    },
                    cwd: None,
                    target,
                };
                let pool = ctx.operation_repo.sqlite_db();
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
mod verdict_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_key_round_trip() {
        assert_eq!(gate_attempt_key("w:impl", 3), "w:impl#g3");
        assert_eq!(parse_attempt_key("w:impl#g3"), Some(("w:impl", 3)));
        // Keys are [a-z0-9._-], so the LAST '#g' is always the attempt separator.
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
        let first_action = script
            .lines()
            .find(|l| !l.starts_with('#') && !l.trim().is_empty())
            .unwrap();
        assert_eq!(first_action, "read -r _go || exit 75");
        assert!(script.contains("'::gate-step fmt'"));
        assert!(script.contains("'::gate-step it'\\''s-quoted'"));
        assert!(script.contains("(\ncargo fmt --check\n)\n"));
        // The env var is unset BEFORE any step runs — step children must not inherit the verdict-file path.
        let unset_pos = script
            .find("unset NEIGE_GATE_EXIT_PATH")
            .expect("unset line");
        let first_step_pos = script.find("'::gate-step fmt'").expect("first step");
        assert!(unset_pos < first_step_pos, "unset precedes every step");
        let unset_step_pos = script
            .find("unset NEIGE_GATE_STEP_PATH")
            .expect("unset step line");
        let first_record_pos = script
            .find("printf '%s\\n' 1 > \"$neige_gate_step_path\"")
            .expect("first step record");
        assert!(unset_step_pos < first_record_pos && first_record_pos < first_step_pos);
        assert!(
            script.contains("mv -f -- \"$neige_gate_exit_path.tmp\" \"$neige_gate_exit_path\"")
        );
        assert!(script.trim_end().ends_with("neige_gate_finish 0"));
    }

    #[test]
    fn gate_timeout_clamps() {
        let mut gate = GateSpec {
            cwd: None,
            timeout_secs: None,
            steps: vec![],
        };
        assert_eq!(gate.timeout_secs_clamped(), 1800);
        gate.timeout_secs = Some(999_999);
        assert_eq!(gate.timeout_secs_clamped(), 7200);
        gate.timeout_secs = Some(0);
        assert_eq!(gate.timeout_secs_clamped(), 1);
    }
}

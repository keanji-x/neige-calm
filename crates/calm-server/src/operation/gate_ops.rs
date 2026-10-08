//! The gate operations of one attempt, both kinds (#2464 D7): the kernel's gates (`task-verify`,
//! keyed `<attempt>#g<N>`) and the worker's runs (`task-gate-run`, keyed `<attempt>#r<N>`). One
//! reading decides whether a gate process of the attempt may still be at work in its checkout:
//! a new run, a regate and the delivery all ask it.

use super::gate_process::marked_group_stopped;
use super::task_gate_run::{TASK_GATE_RUN_KIND, parse_run_key};
use super::task_verify_adapter::{TASK_VERIFY_KIND, parse_attempt_key};
use super::{OperationOutcome, PhaseTag, SpawnArtifacts, Tx};
use crate::error::Result;

/// One gate operation of an attempt: its key (also its processes' `NEIGE_GATE_OP` marker), its
/// phase and its last recorded process group.
#[derive(Clone, Debug)]
pub(crate) struct GateOp {
    pub key: String,
    pub kind: GateOpKind,
    /// The gate attempt (`#g<N>`) or the run (`#r<N>`) number.
    pub number: i64,
    pub phase: PhaseTag,
    pub artifacts: Option<SpawnArtifacts>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateOpKind {
    Verify,
    Run,
}

impl GateOp {
    /// How a refusal names the op: `attempt <N>` for a kernel gate, `run r<N>` for a run.
    pub(crate) fn label(&self) -> String {
        match self.kind {
            GateOpKind::Verify => format!("attempt {}", self.number),
            GateOpKind::Run => format!("run r{}", self.number),
        }
    }
}

/// What the gate operations of one attempt allow.
#[derive(Clone, Debug)]
pub(crate) enum AttemptGateOps {
    /// Every op is `succeeded` or `failed` and its recorded group is proven stopped.
    Clear,
    /// An op has not finished (`pending` through `parked`, or `compensating`): it may still start
    /// or run processes. It wins over [`Self::Unproven`].
    Unfinished(GateOp),
    /// An op is `stuck`, or finished with a recorded group its marker cannot prove stopped. It
    /// never finishes, so nothing waits on it.
    Unproven(GateOp),
}

/// Every gate operation of `task_id`, kernel gates first, each kind by number.
pub(crate) async fn gate_ops_of_attempt_tx(tx: &mut Tx<'_>, task_id: &str) -> Result<Vec<GateOp>> {
    // Keys in [`<task>#g`, `<task>#h`) and [`<task>#r`, `<task>#s`) are exactly the keys of the
    // task's ops of each kind; the parsers re-check each.
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT kind, idempotency_key, phase, spawn_artifacts_json FROM operations \
         WHERE (kind = ?1 AND idempotency_key >= ?2 AND idempotency_key < ?3) \
            OR (kind = ?4 AND idempotency_key >= ?5 AND idempotency_key < ?6)",
    )
    .bind(TASK_VERIFY_KIND)
    .bind(format!("{task_id}#g"))
    .bind(format!("{task_id}#h"))
    .bind(TASK_GATE_RUN_KIND)
    .bind(format!("{task_id}#r"))
    .bind(format!("{task_id}#s"))
    .fetch_all(&mut **tx)
    .await?;
    let mut ops = Vec::new();
    for (kind, key, phase, artifacts) in rows {
        let parsed = if kind == TASK_VERIFY_KIND {
            parse_attempt_key(&key).map(|(task, n)| (task, GateOpKind::Verify, n))
        } else {
            parse_run_key(&key).map(|(task, n)| (task, GateOpKind::Run, n))
        };
        let Some((op_task, kind, number)) = parsed else {
            continue;
        };
        if op_task != task_id {
            continue;
        }
        let artifacts = artifacts
            .as_deref()
            .map(serde_json::from_str::<SpawnArtifacts>)
            .transpose()?;
        ops.push(GateOp {
            key: key.clone(),
            kind,
            number,
            phase: PhaseTag::from_db_str(&phase)?,
            artifacts,
        });
    }
    ops.sort_by_key(|op| (op.kind == GateOpKind::Run, op.number));
    Ok(ops)
}

/// Classify `ops`. A caller that reads before its own op row exists (run admission, regate, the
/// delivery) passes every op; reuse leaves out the op it is preparing ([`attempt_gate_ops`]).
pub(crate) fn classify_gate_ops(ops: &[GateOp]) -> AttemptGateOps {
    let mut unproven = None;
    for op in ops {
        match op.phase {
            PhaseTag::Succeeded | PhaseTag::Failed => {
                let stopped = op.artifacts.as_ref().is_none_or(|artifacts| {
                    matches!(marked_group_stopped(artifacts, &op.key), Ok(true))
                });
                if !stopped && unproven.is_none() {
                    unproven = Some(op.clone());
                }
            }
            PhaseTag::Stuck => {
                if unproven.is_none() {
                    unproven = Some(op.clone());
                }
            }
            _ => return AttemptGateOps::Unfinished(op.clone()),
        }
    }
    match unproven {
        Some(op) => AttemptGateOps::Unproven(op),
        None => AttemptGateOps::Clear,
    }
}

/// [`gate_ops_of_attempt_tx`] then [`classify_gate_ops`], without the op keyed `exclude`: reuse
/// reads inside the `prepare_tx` of the `#g1` op, which is still `pending` there (J1).
pub(crate) async fn attempt_gate_ops(
    tx: &mut Tx<'_>,
    task_id: &str,
    exclude: Option<&str>,
) -> Result<AttemptGateOps> {
    let mut ops = gate_ops_of_attempt_tx(tx, task_id).await?;
    ops.retain(|op| Some(op.key.as_str()) != exclude);
    Ok(classify_gate_ops(&ops))
}

/// The outcome of a finished gate op, read in `tx`; `None` while it has not finished.
pub(crate) async fn gate_op_outcome_tx(
    tx: &mut Tx<'_>,
    op: &GateOp,
) -> Result<Option<OperationOutcome>> {
    let row = sqlx::query("SELECT * FROM operations WHERE kind = ?1 AND idempotency_key = ?2")
        .bind(op.op_kind())
        .bind(&op.key)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let op = super::repo_sqlite::operation_from_row(&row)?;
    Ok(super::operation_result_from(&op)?.map(|result| result.outcome))
}

impl GateOp {
    /// The operation kind the op's row is filed under.
    pub(crate) fn op_kind(&self) -> &'static str {
        match self.kind {
            GateOpKind::Verify => TASK_VERIFY_KIND,
            GateOpKind::Run => TASK_GATE_RUN_KIND,
        }
    }
}

impl super::OperationRuntime {
    /// [`attempt_gate_ops`] in its own short transaction, for a caller outside one (the delivery,
    /// #2464 D8).
    pub(crate) async fn attempt_gate_ops(&self, task_id: &str) -> Result<AttemptGateOps> {
        let pool = self.repo.sqlite_pool();
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let ops = attempt_gate_ops(&mut tx, task_id, None).await;
        tx.rollback().await?;
        ops
    }
}

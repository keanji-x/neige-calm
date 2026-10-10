//! Who may run, and the one transaction that admits a run (#2464 D3, D5, D10). The tool handler
//! runs [`admit_run_tx`] in its `BEGIN IMMEDIATE` transaction: the guards, the join of an
//! unfinished run and the insert of the new run's op row commit together, so a second caller sees
//! the first caller's row and joins it, and a refusal writes no row.

use calm_types::task_execution::TaskAccess;

use super::{GATE_RUNS_PER_ATTEMPT, TASK_GATE_RUN_KIND, TaskGateRunPayload, gate_run_key};
use crate::db::sqlite::{
    WorkerBinding, WorkerOf, attempt_binding_tx, task_attempt_current_tx, task_get_tx,
    worker_binding_tx,
};
use crate::error::{CalmError, Result};
use crate::git_candidate::commit_message::CommitMessage;
use crate::model::{TaskStatus, new_id};
use crate::operation::gate_lifecycle::GateSpec;
use crate::operation::gate_ops::{
    AttemptGateOps, GateOpKind, classify_gate_ops, gate_ops_of_attempt_tx,
};
use crate::operation::repo_sqlite::insert_pending_row;
use crate::operation::workspace_lease::facts::latest_workspace_lease_for_attempt_tx;
use crate::operation::workspace_lease::{DeliveryPolicy, worker_branch_tx};
use crate::operation::{OperationKey, Tx};
use crate::routes::idempotency_key::stable_payload_hash;

/// The checkout a run commits and runs in: the attempt's kernel-delivery lease, its base and
/// branch, and the frozen gate.
pub(super) struct RunTarget {
    pub cwd: String,
    pub branch: String,
    pub base_sha: String,
    pub canonical_path: String,
    pub git_common_dir: String,
    pub gate: GateSpec,
}

/// D10, read in the caller's transaction: `attempt_id` is the current, `running` execution of its
/// key, run by `card_id` on `track_id`; it declares a gate and holds a kernel-delivery lease. The
/// command set is the frozen `gate_json`: the worker chooses when, never what.
pub(super) async fn run_target_tx(
    tx: &mut Tx<'_>,
    attempt_id: &str,
    card_id: &str,
    track_id: &str,
) -> Result<RunTarget> {
    let not_yours = || {
        CalmError::BadRequest(format!(
            "a gate run needs the running attempt you were handed; {attempt_id} is not one"
        ))
    };
    let task = task_get_tx(tx, attempt_id).await?.ok_or_else(not_yours)?;
    // #2493: the attempt is Live and running in a session of `card_id`.
    let live_running = matches!(
        worker_binding_tx(tx, WorkerOf::Attempt(attempt_id)).await?,
        WorkerBinding::Live {
            status: TaskStatus::Running,
            ..
        }
    );
    let bound_card = attempt_binding_tx(tx, attempt_id)
        .await?
        .and_then(|binding| binding.card_id);
    if task.track_id != track_id || !live_running || bound_card.as_deref() != Some(card_id) {
        return Err(not_yours());
    }
    let current = task_attempt_current_tx(tx, &task.track_id, &task.key).await?;
    if task.status != TaskStatus::Running
        || current.as_ref().map(|c| c.attempt_id.as_str()) != Some(attempt_id)
    {
        return Err(not_yours());
    }
    let gate_json = task
        .gate_json
        .as_deref()
        .ok_or_else(|| CalmError::BadRequest(format!("task `{}` declares no gate", task.key)))?;
    let gate = GateSpec::parse(&task.id, gate_json)?;
    let no_kernel_commit = || {
        CalmError::BadRequest(format!(
            "task `{}` has no kernel commit; its gate runs after you report done",
            task.key
        ))
    };
    if task.access == TaskAccess::ReadOnly {
        return Err(no_kernel_commit());
    }
    let lease = latest_workspace_lease_for_attempt_tx(tx, attempt_id)
        .await?
        .filter(|lease| lease.delivery_policy == Some(DeliveryPolicy::Kernel))
        .ok_or_else(no_kernel_commit)?;
    let base = lease.base.as_ref().ok_or_else(no_kernel_commit)?;
    let utf8 = |path: &std::path::Path| {
        path.to_str().map(str::to_owned).ok_or_else(|| {
            CalmError::Internal(format!(
                "lease {} path {} is not UTF-8",
                lease.lease_id,
                path.display()
            ))
        })
    };
    Ok(RunTarget {
        canonical_path: utf8(&base.canonical_path)?,
        git_common_dir: utf8(&base.git_common_dir)?,
        base_sha: base.base_sha.clone(),
        branch: worker_branch_tx(tx, &task.track_id).await?,
        cwd: lease.path.clone(),
        gate,
    })
}

/// What the admission decided: a new run whose op row it inserted, or the unfinished run the call
/// joins (its own `commit_message` is dropped: that run's checkpoint used the first call's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    New { key: String, run: i64, used: i64 },
    Joined { key: String, run: i64, used: i64 },
}

/// D3/D5/D10 in one transaction. Refusals: not the caller's running attempt (the calling worker
/// session's Live binding, #2493), a stale context, no gate, no kernel commit (`BadRequest` /
/// `Conflict` as [`run_target_tx`] and the stale fence say); an earlier gate op whose processes are
/// not proven stopped; the cap. None writes a row.
pub(crate) async fn admit_run_tx(
    tx: &mut Tx<'_>,
    attempt_id: &str,
    card_id: &str,
    session_id: &str,
    track_id: &str,
    commit_message: Option<CommitMessage>,
) -> Result<Admitted> {
    let runs_it = matches!(
        worker_binding_tx(tx, WorkerOf::Session(session_id)).await?,
        WorkerBinding::Live { attempt_id: bound, status: TaskStatus::Running, .. } if bound == attempt_id
    );
    if !runs_it {
        return Err(CalmError::BadRequest(format!(
            "a gate run needs the running attempt you were handed; {attempt_id} is not one"
        )));
    }
    run_target_tx(tx, attempt_id, card_id, track_id).await?;
    crate::operation::refuse_if_context_stale(tx, Some(attempt_id)).await?;
    let ops = gate_ops_of_attempt_tx(tx, attempt_id).await?;
    let used = ops.iter().filter(|op| op.kind == GateOpKind::Run).count() as i64;
    match classify_gate_ops(&ops) {
        AttemptGateOps::Clear => {}
        AttemptGateOps::Unfinished(op) if op.kind == GateOpKind::Run => {
            return Ok(Admitted::Joined {
                key: op.key,
                run: op.number,
                used,
            });
        }
        AttemptGateOps::Unfinished(op) => {
            return Err(CalmError::Conflict(format!(
                "{} of this attempt's gate is unfinished; report done or fail",
                op.label()
            )));
        }
        AttemptGateOps::Unproven(op) => {
            let name = match op.kind {
                GateOpKind::Run => format!("run `r{}`", op.number),
                GateOpKind::Verify => format!("gate {}", op.label()),
            };
            return Err(CalmError::Conflict(format!(
                "{name}'s processes could not be proven stopped; report done or fail"
            )));
        }
    }
    if used >= GATE_RUNS_PER_ATTEMPT {
        return Err(CalmError::Conflict(format!(
            "attempt used {used} of {GATE_RUNS_PER_ATTEMPT} gate runs; report done and the kernel's gate decides"
        )));
    }
    let run = used + 1;
    let payload = TaskGateRunPayload {
        track_id: track_id.to_string(),
        task_id: attempt_id.to_string(),
        card_id: card_id.to_string(),
        run,
        message: match commit_message {
            Some(message) => message.as_str().to_string(),
            None => format!("neige: attempt {attempt_id} gate run {run}"),
        },
    };
    let key = gate_run_key(attempt_id, run);
    let payload = serde_json::to_value(&payload)?;
    let operation_key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(key.clone()),
        payload_hash: stable_payload_hash(&payload)?,
    };
    insert_pending_row(&new_id(), TASK_GATE_RUN_KIND, &operation_key, &payload)?
        .execute(&mut **tx)
        .await?;
    Ok(Admitted::New {
        key,
        run,
        used: run,
    })
}

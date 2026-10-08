//! #2405: re-run a failed gate on the same candidate. The attempt's own row goes back to
//! `verifying` — no new attempt, worker, lease or delivery — and the scheduler's ordinary gate
//! drive runs the frozen `gate_json` against the attempt's candidate again.
//!
//! Admission and the transition are one transaction (the caller's `BEGIN IMMEDIATE`): every guard
//! below is read in it, so the row cannot move between the check and the write.
//!
//! The row's `gate_attempt` becomes `M + 1`, where `M` is the highest gate attempt the task has
//! used (the row's, its verdict's, and every task-verify op's). `M + 1` is reserved, never run: no
//! op `#g{M+1}` exists, so the scheduler submits `#g{M+2}` and that op's `prepare_tx` bump moves
//! the row onto it. Reserving `M` instead would let the scheduler find the finished op `#gM` and
//! copy its old verdict straight back. The visible gate run numbers therefore skip one.
//!
//! The re-run reads the attempt's frozen gate and candidate only; context changes since the
//! attempt ended are not re-checked (its `task_ref_index` rows were dropped when it ended).

use super::target::{
    GATE_INFRA, GATE_RED, GATE_TARGET_MISMATCH, GATE_TIMEOUT, TaskGateResult, VerifyIdentity,
    verify_target_identity,
};
use crate::db::sqlite::{
    CheckoutOccupancy, checkout_occupancy, status_detail_class, task_attempt_current_tx,
    task_get_tx, task_regate_tx, track_get_tx,
};
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::ids::TrackId;
use crate::model::{TaskStatus, now_ms};
use crate::operation::gate_ops::{
    AttemptGateOps, GateOp, GateOpKind, classify_gate_ops, gate_ops_of_attempt_tx,
};
use crate::operation::{PhaseTag, Tx};

/// The `status_detail` classes a gate verdict writes on failure; a row failed with any other
/// class (a worker failure, `delivery-failed`) never ran its gate on a candidate.
const GATE_FAILURE_CLASSES: [&str; 4] = [GATE_RED, GATE_TIMEOUT, GATE_INFRA, GATE_TARGET_MISMATCH];

/// What a re-run reserved, for the tool's receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Regated {
    pub key: String,
    /// The attempt of the verdict being re-run.
    pub previous_gate_attempt: i64,
    /// The number the row now holds without running it.
    pub reserved_gate_attempt: i64,
}

impl Regated {
    /// The gate attempt the scheduler will run.
    pub fn next_gate_attempt(&self) -> i64 {
        self.reserved_gate_attempt + 1
    }
}

/// Admit and apply one re-run of `attempt_id`'s gate in `tx`, for the Planner of `track_id`.
/// Returns the receipt and the `task.regate_requested` event the caller appends in the same
/// transaction. Every refusal is `NotFound` (not this Track's attempt) or `Conflict` (why not now).
pub(crate) async fn regate_in_tx(
    tx: &mut Tx<'_>,
    track_id: &TrackId,
    attempt_id: &str,
    agent_message: String,
) -> Result<(Regated, Event)> {
    let refuse = |why: String| CalmError::Conflict(format!("regate {attempt_id} refused: {why}"));
    let task = task_get_tx(tx, attempt_id)
        .await?
        .filter(|task| task.track_id == track_id.as_str())
        .ok_or_else(|| {
            CalmError::NotFound(format!(
                "attempt_id {attempt_id} is not a task attempt of this track; regate refused"
            ))
        })?;
    let current = task_attempt_current_tx(tx, track_id.as_str(), &task.key).await?;
    if current.as_ref().map(|c| c.attempt_id.as_str()) != Some(task.id.as_str()) {
        return Err(refuse(format!(
            "it is not the current execution of task {}",
            task.key
        )));
    }
    if !track_get_tx(tx, track_id).await?.is_open() {
        return Err(refuse("the track is closed".into()));
    }
    if task.status != TaskStatus::Failed {
        return Err(refuse(format!(
            "task {} is {}; only a failed gate can be re-run",
            task.key,
            task.status.wire_label()
        )));
    }
    if task.gate_json.is_none() {
        return Err(refuse(format!("task {} declares no gate", task.key)));
    }
    let class = task.status_detail.as_deref().map(status_detail_class);
    if !class.is_some_and(|class| GATE_FAILURE_CLASSES.contains(&class)) {
        return Err(refuse(format!(
            "task {} failed before its gate judged a candidate ({})",
            task.key,
            class.unwrap_or("no status_detail")
        )));
    }
    let previous: TaskGateResult = task
        .gate_result_json
        .as_deref()
        .ok_or_else(|| refuse(format!("task {} has no gate verdict", task.key)))
        .and_then(|raw| {
            serde_json::from_str(raw)
                .map_err(|e| CalmError::Internal(format!("task {} gate_result_json: {e}", task.id)))
        })?;
    if task.context_stale_at_ms.is_some() {
        return Err(refuse(format!(
            "task {}'s context went stale; declare a new task",
            task.key
        )));
    }

    let delivery_created_at_ms = match verify_target_identity(tx, &task).await? {
        VerifyIdentity::Bound {
            candidate: Some(_),
            delivery: Some(delivery),
            ..
        } => delivery.created_at_ms,
        VerifyIdentity::Bound {
            candidate: Some(candidate),
            delivery: None,
            ..
        } => {
            return Err(CalmError::Internal(format!(
                "candidate {} has no delivery row",
                candidate.candidate_id
            )));
        }
        VerifyIdentity::Bound {
            candidate: None, ..
        }
        | VerifyIdentity::Unbound { .. } => {
            return Err(refuse(format!(
                "task {} has no kernel-delivered candidate to verify again",
                task.key
            )));
        }
    };

    // Every gate op of the attempt (kernel gates and the worker's runs) is over, and each op's last
    // recorded process group is proven stopped, judged within that pgid and by that op's
    // `NEIGE_GATE_OP` marker: a terminal phase alone does not prove it (a descendant that hid its
    // environ survives a recovered gate whose cleanup failed). A descendant that left the group or
    // dropped the marker is not seen (#2437).
    let ops = gate_ops_of_attempt_tx(tx, &task.id).await?;
    match classify_gate_ops(&ops) {
        AttemptGateOps::Clear => {}
        AttemptGateOps::Unfinished(op) => return Err(refuse(unfinished(&op))),
        AttemptGateOps::Unproven(op) if op.phase == PhaseTag::Stuck => {
            return Err(refuse(unfinished(&op)));
        }
        AttemptGateOps::Unproven(op) => {
            return Err(refuse(format!(
                "the previous gate's processes ({}) could not be proven stopped; \
                 the kernel will not run a second gate beside them; declare a new task instead",
                op.label()
            )));
        }
    }
    let highest = ops
        .iter()
        .filter(|op| op.kind == GateOpKind::Verify)
        .map(|op| op.number)
        .fold(task.gate_attempt.max(previous.verdict.attempt), i64::max);

    if checkout_occupancy(tx, track_id.as_str(), &task.id).await? != CheckoutOccupancy::Free {
        return Err(refuse(
            "the track checkout is in use; re-run once no task runs or delivers".into(),
        ));
    }
    let later_delivery: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_git_deliveries \
         WHERE track_id = ?1 AND producer_attempt_id <> ?2 AND created_at_ms > ?3)",
    )
    .bind(track_id.as_str())
    .bind(&task.id)
    .bind(delivery_created_at_ms)
    .fetch_one(&mut **tx)
    .await?;
    if later_delivery {
        return Err(refuse(format!(
            "a later task delivered into the checkout after task {}; its candidate is no longer checked out",
            task.key
        )));
    }

    let regated = Regated {
        key: task.key.clone(),
        previous_gate_attempt: previous.verdict.attempt,
        reserved_gate_attempt: highest + 1,
    };
    let rows = task_regate_tx(
        tx,
        &task.id,
        track_id.as_str(),
        task.gate_attempt,
        regated.reserved_gate_attempt,
        now_ms(),
    )
    .await?;
    if rows == 0 {
        return Err(refuse(format!("task {} changed concurrently", task.key)));
    }
    let event = Event::TaskRegateRequested {
        attempt_id: task.id.clone(),
        key: task.key.clone(),
        previous_gate_attempt: regated.previous_gate_attempt,
        reserved_gate_attempt: regated.reserved_gate_attempt,
        agent_message,
    };
    Ok((regated, event))
}

fn unfinished(op: &GateOp) -> String {
    format!(
        "a gate op of this task is unfinished or stuck ({} is {}); \
         the kernel cannot prove no gate process is running; declare a new task instead",
        op.label(),
        op.phase.as_str()
    )
}

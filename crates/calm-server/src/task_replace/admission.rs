//! Steps 2–4 of a replacement (design §4.4), inside the report transaction: the attempt check and
//! every refusal (2, 2a) before the first write, then the stop (3) and the carry source (4).

use calm_types::report_blocks::KIND_TASK;
use calm_types::report_blocks::tasks::{PLANNER_DECLARATION_AUTHOR, task_block_is_live};
use calm_types::task_recovery::TASK_IN_TRACK_ROUTE;
use calm_types::track_report::ReportBlock;
use serde_json::{Value, json};

use super::receipt::{self, CarryNone, CarrySource, Stop};
use super::refusal::Refusal;
use super::{CarryMode, ReplaceArgs};
use crate::db::sqlite::{
    status_detail_with_reason, task_attempt_current_tx, task_cancel_pending_with_detail_tx,
    task_cancel_running_tx, task_get_tx,
};
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::model::{Task, TaskStatus, now_ms};
use crate::operation::Tx;
use crate::scheduler::{WorkerCleanupReason, mark_running_timeout_cleanup_tx};

/// `status_detail` class of a predecessor a replacement canceled (`superseded: <successor key>`).
pub(crate) const SUPERSEDED: &str = "superseded";

/// A replacement every refusal has passed; nothing is written yet.
pub(crate) struct Admitted {
    pub predecessor: Task,
    pub successor_key: String,
    /// Where the successor block goes: right after the predecessor's.
    pub position: usize,
    pub successor_payload: Value,
}

fn status_str(status: TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The predecessor's live task block: the first live block declaring `key`.
fn predecessor_block<'a>(blocks: &'a [ReportBlock], key: &str) -> Option<(usize, &'a ReportBlock)> {
    blocks
        .iter()
        .enumerate()
        .find(|(_, block)| task_block_is_live(block) && block.payload["key"] == key)
}

/// `<root>.<n>`: a predecessor that is itself a successor is `<root>.<m>` by construction and
/// continues at `m + 1`; any other key roots a new lineage at `.2`.
async fn successor_key_tx(tx: &mut Tx<'_>, track_id: &str, key: &str) -> Result<String> {
    if receipt::by_successor_tx(tx, track_id, key).await?.is_some()
        && let Some((root, n)) = key.rsplit_once('.')
        && let Ok(n) = n.parse::<u32>()
    {
        return Ok(format!("{root}.{}", n + 1));
    }
    Ok(format!("{key}.2"))
}

/// Every task still to finish that depends on `key`: execution rows not yet terminal, and live
/// report declarations naming `key` whose own key has no terminal execution — an unready or
/// ceiling-held declaration has no row yet but will wait on `key` once it starts.
async fn unfinished_dependents_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    blocks: &[ReportBlock],
    key: &str,
) -> Result<Vec<String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, depends_on_json FROM current_tasks WHERE track_id = ?1 \
         AND status IN ('pending','dispatched','running','verifying') ORDER BY key",
    )
    .bind(track_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut dependents: std::collections::BTreeSet<String> = rows
        .into_iter()
        .filter(|(_, depends)| {
            serde_json::from_str::<Vec<String>>(depends)
                .is_ok_and(|deps| deps.iter().any(|d| d == key))
        })
        .map(|(key, _)| key)
        .collect();
    for block in blocks.iter().filter(|block| {
        task_block_is_live(block)
            && block.payload["depends_on"]
                .as_array()
                .is_some_and(|deps| deps.iter().any(|d| d == key))
    }) {
        let Some(dependent) = block.payload["key"].as_str() else {
            continue;
        };
        let finished = match task_attempt_current_tx(tx, track_id, dependent).await? {
            Some(allocation) => {
                task_get_tx(tx, &allocation.attempt_id)
                    .await?
                    .is_some_and(|task| {
                        matches!(
                            task.status,
                            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled
                        )
                    })
            }
            None => false,
        };
        if !finished {
            dependents.insert(dependent.to_string());
        }
    }
    Ok(dependents.into_iter().collect())
}

/// Steps 2 and 2a: the expected attempt is current and every refusal is decided, before any write.
pub(crate) async fn admit_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    blocks: &[ReportBlock],
    args: &ReplaceArgs,
) -> Result<Admitted> {
    let current = task_attempt_current_tx(tx, track_id, &args.key).await?;
    if current.as_ref().map(|a| a.attempt_id.as_str()) != Some(args.expected_attempt_id.as_str()) {
        let facts = current.map_or_else(
            || format!("task {} has no attempt", args.key),
            |a| format!("current attempt {}", a.attempt_id),
        );
        return Err(Refusal::StaleAttempt.refuse(&facts));
    }
    let predecessor = task_get_tx(tx, &args.expected_attempt_id)
        .await?
        .ok_or_else(|| {
            Refusal::StaleAttempt.refuse(&format!(
                "attempt {} has no task row",
                args.expected_attempt_id
            ))
        })?;

    let track =
        crate::track_lifecycle::track_get_tx(tx, &TrackId::from(track_id.to_string())).await?;
    if track.lifecycle.is_terminal() {
        return Err(Refusal::TrackTerminal.refuse(track.lifecycle.as_db_str()));
    }
    // The declared route (the one predicate), and the execution fact that an isolated worker
    // operation already ran for this attempt.
    if !super::route::on_route(&predecessor, &track)
        || crate::isolated_codex::lookup::is_isolated_task_tx(tx, &predecessor.id).await?
    {
        return Err(Refusal::UnsupportedRoute.refuse(""));
    }
    let policy: Option<String> =
        sqlx::query_scalar("SELECT automation_policy FROM tracks WHERE id = ?1")
            .bind(track_id)
            .fetch_one(&mut **tx)
            .await?;
    if policy.as_deref() == Some("declare-and-wait") {
        return Err(Refusal::RequiresUserRelease.refuse(""));
    }
    if let Some(existing) = receipt::by_predecessor_tx(tx, &predecessor.id).await? {
        return Err(
            Refusal::AlreadyReplaced.refuse(&format!("successor {}", existing.successor_key))
        );
    }
    match predecessor.status {
        TaskStatus::Dispatched => return Err(Refusal::PredecessorDispatching.refuse("dispatched")),
        TaskStatus::Running if predecessor.worker_card_id.is_none() => {
            return Err(Refusal::PredecessorDispatching.refuse("running without a worker card"));
        }
        TaskStatus::Verifying => return Err(Refusal::PredecessorVerifying.refuse("verifying")),
        _ => {}
    }
    let delivery_pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_git_deliveries \
         WHERE producer_attempt_id = ?1 AND settlement IS NULL)",
    )
    .bind(&predecessor.id)
    .fetch_one(&mut **tx)
    .await?;
    if delivery_pending {
        return Err(Refusal::CandidatePending.refuse(""));
    }
    let dependents = unfinished_dependents_tx(tx, track_id, blocks, &predecessor.key).await?;
    if !dependents.is_empty() {
        return Err(Refusal::PendingDependents.refuse(&dependents.join(", ")));
    }
    let Some((index, block)) = predecessor_block(blocks, &predecessor.key) else {
        return Err(Refusal::PredecessorUndeclared.refuse(""));
    };
    let successor_key = successor_key_tx(tx, track_id, &predecessor.key).await?;
    if !calm_types::report_blocks::tasks::key_is_valid(&successor_key) {
        return Err(Refusal::DerivedKeyTooLong.refuse(&successor_key));
    }
    let declared = blocks
        .iter()
        // Any task block, tombstones included: a tombstoned key blocks its redeclaration.
        .any(|block| block.kind == KIND_TASK && block.payload["key"] == successor_key.as_str());
    if declared
        || task_attempt_current_tx(tx, track_id, &successor_key)
            .await?
            .is_some()
        || receipt::by_successor_tx(tx, track_id, &successor_key)
            .await?
            .is_some()
    {
        return Err(Refusal::DerivedKeyTaken.refuse(&successor_key));
    }
    let successor_payload = successor_payload(&block.payload, &successor_key, args)?;
    // The block may have been edited since the predecessor started: the successor copies the
    // block, so its own route is what must be replaceable, before anything is stopped.
    let spawn = successor_payload["spawn"]
        .as_str()
        .unwrap_or(TASK_IN_TRACK_ROUTE);
    if !super::route::route_is_replaceable(
        track.workspace.kind,
        successor_payload["kind"].as_str().unwrap_or_default(),
        spawn,
        &successor_payload["context"],
    ) {
        return Err(Refusal::UnsupportedRoute.refuse("the predecessor's block names another route"));
    }
    Ok(Admitted {
        predecessor,
        successor_key,
        position: index + 1,
        successor_payload,
    })
}

/// The predecessor's block payload with the successor's identity and the request's contract:
/// `gate`, `no_gate_reason`, `kind`, `depends_on`, `priority` and `spawn` are copied untouched;
/// `goal`, `acceptance` and `context` are the request's; Planner-declared and ready, with no
/// User release (each new declaration's release is the User's decision, as for a repair).
fn successor_payload(predecessor: &Value, key: &str, args: &ReplaceArgs) -> Result<Value> {
    let mut payload = predecessor.clone();
    let object = payload.as_object_mut().ok_or_else(|| {
        CalmError::Internal("predecessor task block payload is not an object".into())
    })?;
    object.remove("released_by_user");
    object.insert("key".into(), json!(key));
    object.insert("declared_by".into(), json!(PLANNER_DECLARATION_AUTHOR));
    object.insert("ready".into(), json!(true));
    object.insert("goal".into(), json!(args.goal));
    object.insert("acceptance".into(), json!(args.acceptance));
    object.insert("context".into(), args.successor_context());
    Ok(payload)
}

/// Step 3: stop the predecessor. A `running` one is canceled with the Planner cancel's CAS and its
/// worker marked for the reap; a `pending` one is canceled; a terminal one is left alone. A CAS
/// that moves nothing refuses the whole request with the row's current status.
pub(crate) async fn stop_tx(tx: &mut Tx<'_>, admitted: &Admitted) -> Result<(TaskStatus, Stop)> {
    let predecessor = &admitted.predecessor;
    let detail = status_detail_with_reason(SUPERSEDED, &admitted.successor_key);
    let now = now_ms();
    let rows = match predecessor.status {
        TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled => {
            return Ok((predecessor.status, Stop::AlreadyTerminal));
        }
        TaskStatus::Pending => {
            task_cancel_pending_with_detail_tx(tx, &predecessor.id, &detail, now).await?
        }
        TaskStatus::Running => {
            let card_id = predecessor.worker_card_id.as_deref().ok_or_else(|| {
                CalmError::Internal("admitted running predecessor has no worker card".into())
            })?;
            let rows = task_cancel_running_tx(tx, &predecessor.id, card_id, &detail, now).await?;
            if rows != 0
                && mark_running_timeout_cleanup_tx(
                    tx,
                    card_id,
                    &predecessor.id,
                    now,
                    WorkerCleanupReason::Superseded,
                )
                .await?
                    == 0
            {
                tracing::warn!(
                    task_id = %predecessor.id,
                    card_id,
                    "task_replace: no live worker session to mark; the superseded worker is not reaped"
                );
            }
            rows
        }
        TaskStatus::Dispatched | TaskStatus::Verifying => {
            return Err(CalmError::Internal(
                "admission let a dispatched or verifying predecessor through".into(),
            ));
        }
    };
    if rows == 0 {
        let status = task_get_tx(tx, &predecessor.id)
            .await?
            .map_or(predecessor.status, |row| row.status);
        return Err(Refusal::PredecessorChanged.refuse(&status_str(status)));
    }
    Ok((predecessor.status, Stop::CanceledNow))
}

/// Step 4: the candidate the successor carries — the predecessor's own settled candidate whatever
/// its task status, else the source of the predecessor's own replacement, else none.
pub(crate) async fn carry_source_tx(
    tx: &mut Tx<'_>,
    predecessor: &Task,
    mode: CarryMode,
) -> Result<CarrySource> {
    if mode == CarryMode::None {
        return Ok(CarrySource::None(CarryNone::RequestedNone));
    }
    if let Some(candidate) =
        crate::git_candidate::candidate::candidate_for_attempt_tx(tx, &predecessor.id).await?
    {
        return Ok(CarrySource::From {
            source_attempt_id: predecessor.id.clone(),
            source_candidate_id: candidate.candidate_id,
        });
    }
    Ok(
        match receipt::by_successor_tx(tx, &predecessor.track_id, &predecessor.key).await? {
            Some(own) => match own.carry {
                CarrySource::From { .. } => own.carry,
                CarrySource::None(_) => CarrySource::None(CarryNone::NoCandidate),
            },
            None => CarrySource::None(CarryNone::NoCandidate),
        },
    )
}

//! The server close owner. Callers authorize one target; this transaction owns its subtree.
use crate::db::sqlite::{task_cancel_tx, task_get_tx, track_get_tx, track_update_tx};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, TrackId};
use crate::model::{TaskStatus, Track, TrackPatch, now_ms};
use crate::operation::Tx;
use calm_truth::db::sqlite::{
    MAX_TRACK_TREE_DEPTH, MAX_TREE_TASK_BUDGET, TRACK_BOUNDED_PATH_SQL, TRACK_BOUNDED_SUBTREE_SQL,
    TRACK_ROOT_DEPTH_SQL,
};

pub(crate) struct ClosedSubtree {
    pub track: Track,
    pub executions: Vec<workspace_runtime::execution::ExecutionId>,
    pub events: Vec<(ActorId, EventScope, Event)>,
    /// Cleanup is a committed effect even when the Track was already closed.
    pub cleanup_requested: bool,
}

/// No application or provider policy belongs in the generic truth row writer.
/// BEGIN IMMEDIATE serializes this membership snapshot with child creation and task outcomes.
pub(crate) async fn close_subtree_tx(
    tx: &mut Tx<'_>,
    target_id: &TrackId,
    actor: ActorId,
    message: Option<String>,
) -> Result<ClosedSubtree> {
    let target = track_get_tx(tx, target_id).await?;
    let roots: Vec<(String, i64)> = sqlx::query_as(TRACK_ROOT_DEPTH_SQL)
        .bind(target_id.as_str())
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_all(&mut **tx)
        .await?;
    let [(_, root_depth)] = roots.as_slice() else {
        return Err(CalmError::Conflict("close-subtree-invalid-tree".into()));
    };
    if *root_depth > MAX_TRACK_TREE_DEPTH {
        return Err(CalmError::Conflict("close-subtree-invalid-tree".into()));
    }
    // Include the sentinel level: a truncated walk must never be reported as a complete close.
    let members: Vec<(String, i64)> = sqlx::query_as(TRACK_BOUNDED_SUBTREE_SQL)
        .bind(target_id.as_str())
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .bind(MAX_TREE_TASK_BUDGET + 1)
        .fetch_all(&mut **tx)
        .await?;
    if members.len() as i64 > MAX_TREE_TASK_BUDGET
        || members
            .iter()
            .any(|(_, depth)| root_depth + depth > MAX_TRACK_TREE_DEPTH)
    {
        return Err(CalmError::Conflict("close-subtree-invalid-tree".into()));
    }
    let ancestry: Vec<(String, i64)> = sqlx::query_as(TRACK_BOUNDED_PATH_SQL)
        .bind(target_id.as_str())
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_all(&mut **tx)
        .await?;
    for (ancestor, _) in ancestry {
        if track_get_tx(tx, &ancestor.into()).await?.area_id != target.area_id {
            return Err(CalmError::Conflict("close-subtree-invalid-tree".into()));
        }
    }
    let mut tracks = Vec::with_capacity(members.len());
    for (id, _) in &members {
        let track = track_get_tx(tx, &id.clone().into()).await?;
        if track.area_id != target.area_id {
            return Err(CalmError::Conflict("close-subtree-invalid-tree".into()));
        }
        let forge =
            crate::operation::workspace_lease::track_has_active_forge_action(&mut **tx, id).await?;
        if forge {
            return Err(CalmError::Conflict(
                "target has an in-flight forge-action".into(),
            ));
        }
        tracks.push(track);
    }

    let mut events = Vec::new();
    let mut executions = Vec::new();
    let mut cleanup_requested = false;
    // Cancel every execution still live, including a damaged historical binding, never a terminal one.
    for track in &tracks {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE track_id=?1 AND status IN ('pending','dispatched','running','verifying') ORDER BY id",
        ).bind(track.id.as_str()).fetch_all(&mut **tx).await?;
        let mut keys = Vec::new();
        for id in ids {
            let task = task_get_tx(tx, &id)
                .await?
                .ok_or_else(|| CalmError::Internal("close task disappeared".into()))?;
            if task.status == TaskStatus::Pending {
                task_cancel_tx(tx, &id, now_ms()).await?;
            } else if task.spawn == calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE {
                executions.extend(cancel_child_assignment_tx(tx, &id, &members).await?);
            } else {
                crate::scheduler::task_cancel::require_worker_binding_in_tx(tx, &task).await?;
                let (_, released) =
                    crate::scheduler::task_cancel::cancel_running_in_tx(tx, &task, &task.key)
                        .await?;
                events.extend(released);
            }
            keys.push(task.key);
        }
        cleanup_requested |= mark_ended_workers_tx(tx, track, &mut events).await?;
        if !keys.is_empty() {
            events.push((
                actor.clone(),
                scope(track),
                Event::PlanUpdated {
                    track_id: track.id.clone(),
                    changed_keys: keys,
                    agent_message: message.clone(),
                },
            ));
        }
    }
    // Closing a child directly cancels only its exact current parent assignment, not that parent.
    for (member, _) in &members {
        let bindings: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE child_track_id=?1 AND status IN ('dispatched','running','verifying')",
        ).bind(member).fetch_all(&mut **tx).await?;
        for id in bindings {
            let task = task_get_tx(tx, &id)
                .await?
                .ok_or_else(|| CalmError::Internal("close assignment disappeared".into()))?;
            executions.extend(cancel_child_assignment_tx(tx, &id, &members).await?);
            let parent = track_get_tx(tx, &task.track_id.clone().into()).await?;
            events.push((
                actor.clone(),
                scope(&parent),
                Event::PlanUpdated {
                    track_id: parent.id.clone(),
                    changed_keys: vec![task.key],
                    agent_message: message.clone(),
                },
            ));
        }
    }
    for track in tracks {
        if track.closed_at.is_none() {
            let closed = track_update_tx(
                tx,
                track.id.as_str(),
                TrackPatch {
                    closed: Some(true),
                    ..Default::default()
                },
            )
            .await?;
            events.push((
                actor.clone(),
                scope(&closed),
                Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                    closed,
                    message.clone(),
                )),
            ));
        }
        // Separate from handle_state_json: live snapshot persistence cannot erase the obligation.
        let planners: Vec<(String, String)> = sqlx::query_as(
            "SELECT ws.id,c.id FROM cards c \
             JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=c.track_id \
             WHERE c.track_id=?1 AND c.role='planner' AND ws.contract='planner' \
               AND (ws.state IN ('starting','running','idle','turn_pending') \
                    OR (ws.state='failed' AND ws.completed_at_ms IS NULL))",
        ).bind(track.id.as_str()).fetch_all(&mut **tx).await?;
        for (id, card) in planners {
            let runtime = crate::db::sqlite::session_projection_by_id_tx(tx, &id)
                .await?
                .ok_or_else(|| CalmError::Conflict("closed Planner binding unavailable".into()))?;
            crate::harness::backend::factory::PlannerFactory::require_closed_planner_binding_tx(
                tx, &runtime,
            )
            .await?;
            cleanup_requested |= sqlx::query(
                "INSERT INTO track_close_cleanups(worker_session_id,card_id,track_id,requested_at_ms) \
                 VALUES(?1,?2,?3,?4) ON CONFLICT(worker_session_id) DO NOTHING",
            )
                .bind(id).bind(card).bind(track.id.as_str()).bind(now_ms()).execute(&mut **tx).await?.rows_affected() > 0;
        }
    }
    Ok(ClosedSubtree {
        track: track_get_tx(tx, target_id).await?,
        executions,
        events,
        cleanup_requested,
    })
}

fn scope(track: &Track) -> EventScope {
    EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    }
}

type ChildAssignmentBinding = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

async fn cancel_child_assignment_tx(
    tx: &mut Tx<'_>,
    id: &str,
    members: &[(String, i64)],
) -> Result<Vec<workspace_runtime::execution::ExecutionId>> {
    let binding: Option<ChildAssignmentBinding> = sqlx::query_as(
        "SELECT t.spawn,t.track_id,t.child_track_id,child.parent_track_id,t.status,t.worker_card_id FROM tasks t \
         LEFT JOIN tracks child ON child.id=t.child_track_id WHERE t.id=?1",
    ).bind(id).fetch_optional(&mut **tx).await?;
    let Some((spawn, parent, child, actual_parent, status, worker)) = binding else {
        return Err(CalmError::Conflict("close-subtree-unbound-child".into()));
    };
    let safe = match child {
        Some(child) => {
            actual_parent.as_deref() == Some(parent.as_str())
                && members.iter().any(|(id, _)| id == &child)
        }
        None => status == "dispatched" && worker.is_none(),
    };
    if spawn != calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE || worker.is_some() || !safe {
        return Err(CalmError::Conflict("close-subtree-foreign-child".into()));
    }
    calm_truth::db::sqlite::progress_obligation::progress_obligation_release_tx(
        tx,
        id,
        calm_truth::db::sqlite::task_progress::CHILD_BOOTSTRAP_PURPOSE,
    )
    .await?;
    sqlx::query("UPDATE tasks \
        SET status='canceled',status_detail='track-closed',running_deadline_ms=NULL,updated_at_ms=?2,finished_at_ms=?2 \
        WHERE id=?1 AND status IN ('pending','dispatched','running','verifying')")
        .bind(id).bind(now_ms()).execute(&mut **tx).await?;
    crate::operation::task_verify_adapter::cancel_child_gate_tx(tx, id).await
}

/// Completion may win before close. Keep its terminal outcome, but persist the same exact
/// cleanup obligation. A live carrier with no attributable attempt refuses the entire close.
async fn mark_ended_workers_tx(
    tx: &mut Tx<'_>,
    track: &Track,
    events: &mut Vec<(ActorId, EventScope, Event)>,
) -> Result<bool> {
    let sessions: Vec<(String,String,Option<String>)> = sqlx::query_as(
        "SELECT ws.id,c.id,json_extract(ws.handle_state_json,'$.timeout_cleanup.task_id') \
         FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=c.track_id \
         WHERE c.track_id=?1 AND c.role='worker' \
           AND ws.state IN ('starting','running','idle','turn_pending')",
    ).bind(track.id.as_str()).fetch_all(&mut **tx).await?;
    let mut marked = false;
    for (session_id, card, marker) in sessions {
        let session = crate::db::sqlite::session_get_tx(tx, &session_id.into())
            .await?
            .ok_or_else(|| CalmError::Conflict("close-subtree-unbound-worker".into()))?;
        let task_id = calm_truth::worker_binding::session_attempt_id_tx(tx, &session, &card)
            .await?
            .ok_or_else(|| CalmError::Conflict("close-subtree-unbound-worker".into()))?;
        let task = task_get_tx(tx, &task_id)
            .await?
            .ok_or_else(|| CalmError::Conflict("close-subtree-unbound-worker".into()))?;
        if !task.status.is_terminal()
            || task.track_id != track.id.as_str()
            || task.worker_card_id.as_deref() != Some(card.as_str())
            || !crate::scheduler::task_has_running_liveness_deadline(&task)
        {
            return Err(CalmError::Conflict("close-subtree-unbound-worker".into()));
        }
        crate::scheduler::task_cancel::require_worker_binding_in_tx(tx, &task).await?;
        if marker.as_deref() != Some(task_id.as_str()) {
            let mark = crate::scheduler::mark_running_timeout_cleanup_tx(
                tx,
                &card,
                &task_id,
                now_ms(),
                crate::scheduler::WorkerCleanupReason::PlannerCanceled,
            )
            .await?;
            marked |= mark.marked > 0;
            events.extend(mark.released);
        }
    }
    Ok(marked)
}

/// Used by creation and start owners inside their own admission transaction.
pub(crate) async fn require_open_tx(tx: &mut Tx<'_>, track: &str) -> Result<()> {
    let open: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE id=?1 AND closed_at IS NULL)")
            .bind(track)
            .fetch_one(&mut **tx)
            .await?;
    if !open {
        return Err(CalmError::Conflict("track-closed".into()));
    }
    Ok(())
}

pub(crate) async fn require_planner_start<'e>(
    executor: impl sqlx::SqliteExecutor<'e>,
    card: &str,
) -> Result<()> {
    let refused: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards c JOIN tracks t ON t.id=c.track_id \
         WHERE c.id=?1 AND c.role='planner' AND (t.closed_at IS NOT NULL \
           OR EXISTS(SELECT 1 FROM track_close_cleanups cleanup WHERE cleanup.card_id=c.id) \
           OR (t.parent_track_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM current_tasks assignment \
             WHERE assignment.child_track_id=t.id AND assignment.track_id=t.parent_track_id \
               AND assignment.status IN ('dispatched','running')))))",
    )
    .bind(card)
    .fetch_one(executor)
    .await?;
    if refused {
        return Err(CalmError::Conflict("track-closed".into()));
    }
    Ok(())
}

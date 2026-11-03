//! Task-owned reporting of a durably observed failure, not a recovery controller.
use super::progress_obligation::{
    ProgressObligation, progress_obligation_notify_tx, progress_obligation_release_tx,
    progress_obligation_require_tx,
};
use crate::error::Result;
use crate::planner_binding::PlannerBinding;
use calm_types::{model::CardRole, task_recovery::TASK_CHILD_TRACK_ROUTE};
use sqlx::{Row, Sqlite, SqliteConnection, Transaction};

pub const CHILD_PLANNER_FAILURE_PURPOSE: &str = "child-planner-failed";

pub struct ChildPlannerFailure {
    pub execution_id: String,
    pub track_id: String,
    pub area_id: String,
    pub task_id: String,
    pub task_key: String,
    pub child_track_id: String,
    pub worker_session_id: String,
    pub failure_epoch: i64,
}

/// Start from current owned work, including pre-upgrade failures, not enrolled rows.
/// Root tenure, exact card binding and the shared typed contract must all agree.
async fn current_failures(connection: &mut SqliteConnection) -> Result<Vec<ChildPlannerFailure>> {
    let rows = sqlx::query(
        "SELECT ws.id,ws.track_id,ws.mode,ws.provider,ws.contract,ws.failure_epoch, \
         t.id AS task_id,t.key AS task_key,t.track_id AS parent_track_id,\
         p.area_id AS parent_area_id,c.kind AS card_kind,c.payload AS card_payload \
         FROM current_tasks t JOIN tracks p ON p.id=t.track_id AND p.closed_at IS NULL \
         JOIN tracks child ON child.id=t.child_track_id AND child.closed_at IS NULL \
           AND child.parent_track_id=t.track_id \
         JOIN worker_sessions ws ON ws.id=child.root_session_id AND ws.track_id=child.id \
         JOIN cards c ON c.id=ws.card_id AND c.track_id=child.id AND c.session_id=ws.id \
         WHERE t.spawn=?1 AND t.status='running' AND c.role='planner' AND ws.state='failed' \
           AND (SELECT count(*) FROM cards WHERE track_id=child.id AND role='planner')=1",
    )
    .bind(TASK_CHILD_TRACK_ROUTE)
    .fetch_all(&mut *connection)
    .await?;
    let mut targets = Vec::new();
    for row in rows {
        let mode = super::session_row::worker_session_parse("mode", row.try_get("mode")?)?;
        let provider =
            super::session_row::worker_session_parse("provider", row.try_get("provider")?)?;
        let contract =
            super::session_row::worker_session_parse("contract", row.try_get("contract")?)?;
        let kind: String = row.try_get("card_kind")?;
        let payload: String = row.try_get("card_payload")?;
        let Some(binding) =
            PlannerBinding::from_shape(&kind, CardRole::Planner, &serde_json::from_str(&payload)?)
        else {
            continue;
        };
        if !binding.matches_session_contract_parts(mode, provider, contract) {
            continue;
        }
        let task_id: String = row.try_get("task_id")?;
        let worker_session_id: String = row.try_get("id")?;
        let failure_epoch: i64 = row.try_get("failure_epoch")?;
        targets.push(ChildPlannerFailure {
            execution_id: serde_json::to_string(&(&task_id, &worker_session_id, failure_epoch))?,
            task_id,
            task_key: row.try_get("task_key")?,
            track_id: row.try_get("parent_track_id")?,
            area_id: row.try_get("parent_area_id")?,
            child_track_id: row.try_get("track_id")?,
            worker_session_id,
            failure_epoch,
        });
    }
    Ok(targets)
}

pub async fn task_reconcile_child_failure_obligations_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<()> {
    let targets = current_failures(tx).await?;
    for target in &targets {
        progress_obligation_require_tx(
            tx,
            &ProgressObligation {
                execution_id: target.execution_id.clone(),
                purpose: CHILD_PLANNER_FAILURE_PURPOSE.into(),
                // A confirmed failure is already due; this is not an inactivity clock.
                deadline_at_ms: 0,
            },
        )
        .await?;
    }
    // Closure or a temporarily unavailable binding is dormant, not a new episode.
    // Release only after the original task/session/failure identity is retired.
    let obsolete: Vec<String> = sqlx::query_scalar(
        "SELECT o.execution_id FROM progress_obligations o WHERE o.purpose=?1 AND o.state!='released' \
         AND NOT EXISTS(SELECT 1 FROM current_tasks t JOIN worker_sessions ws \
           ON ws.id=json_extract(CASE WHEN o.purpose=?1 THEN o.execution_id END,'$[1]') \
           WHERE t.id=json_extract(CASE WHEN o.purpose=?1 THEN o.execution_id END,'$[0]') AND t.spawn=?2 AND t.status='running' \
             AND ws.state='failed' AND ws.failure_epoch=json_extract(CASE WHEN o.purpose=?1 THEN o.execution_id END,'$[2]'))"
    ).bind(CHILD_PLANNER_FAILURE_PURPOSE).bind(TASK_CHILD_TRACK_ROUTE).fetch_all(&mut **tx).await?;
    for execution_id in obsolete {
        progress_obligation_release_tx(tx, &execution_id, CHILD_PLANNER_FAILURE_PURPOSE).await?;
    }
    Ok(())
}

/// One eligibility snapshot in the event transaction; each CAS commits with its event.
pub async fn task_claim_child_failure_notices_tx(
    tx: &mut Transaction<'_, Sqlite>,
    now: i64,
) -> Result<Vec<ChildPlannerFailure>> {
    let mut claimed = Vec::new();
    for target in current_failures(tx).await? {
        let obligation = ProgressObligation {
            execution_id: target.execution_id.clone(),
            purpose: CHILD_PLANNER_FAILURE_PURPOSE.into(),
            deadline_at_ms: 0,
        };
        if progress_obligation_notify_tx(tx, &obligation, now).await? {
            claimed.push(target);
        }
    }
    Ok(claimed)
}

/// Active retries cease on supersession/recovery. An already committed event is
/// still a truthful historical observation and keeps the existing replay contract.
pub async fn task_pending_child_failure_notices(
    db: &super::SqliteDb,
) -> Result<Vec<super::task_progress::ParentProgressNotice>> {
    let mut connection = db.read_pool().acquire().await?;
    let targets: std::collections::HashMap<_, _> = current_failures(&mut connection)
        .await?
        .into_iter()
        .map(|target| (target.execution_id, target.track_id))
        .collect();
    let rows: Vec<(String,i64)> = sqlx::query_as(
        "SELECT o.execution_id,e.id FROM progress_obligations o \
         JOIN current_tasks t ON t.id=json_extract(CASE WHEN o.purpose=?1 THEN o.execution_id END,'$[0]') \
         JOIN events e ON e.scope_track=t.track_id AND e.kind='track.wake_requested' AND json_extract(e.payload,'$.source')='progress-obligation' \
           AND json_extract(e.payload,'$.key')=o.execution_id||':'||o.purpose \
         WHERE o.purpose=?1 AND o.state='notified' ORDER BY e.id"
    ).bind(CHILD_PLANNER_FAILURE_PURPOSE).fetch_all(&mut *connection).await?;
    Ok(rows
        .into_iter()
        .filter_map(|(execution_id, event_id)| {
            targets
                .get(&execution_id)
                .map(|track_id| super::task_progress::ParentProgressNotice {
                    track_id: track_id.clone(),
                    event_id,
                })
        })
        .collect())
}

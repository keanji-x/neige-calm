//! Kernel-owned references shared by child success and handback.
use crate::error::{CalmError, Result};
use crate::operation::Tx;
use serde_json::{Value, json};

pub(crate) async fn result_ref_tx(
    tx: &mut Tx<'_>,
    child_track_id: &str,
    blocked_on: Option<&str>,
) -> Result<Value> {
    let (card_id, updated_at, payload, body_crdt): (String, i64, String, Option<Vec<u8>>) =
        sqlx::query_as("SELECT id,updated_at,payload,body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(child_track_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("child track {child_track_id} has no report")))?;
    let report = crate::track_report_read::report_doc_snapshot(
        &card_id,
        updated_at,
        serde_json::from_str(&payload)?,
        body_crdt.as_deref(),
    )?;
    let candidate =
        crate::git_candidate::candidate::latest_candidate_for_track_tx(tx, child_track_id).await?;
    let mut result = json!({
        "source": "child-track",
        "outcome": if blocked_on.is_some() { "handed_back" } else { "met" },
        "child_track_id": child_track_id,
        "report": {"card_id":card_id,"rev":report.doc_rev},
        "candidate": candidate,
    });
    if let Some(blocked_on) = blocked_on {
        result["blocked_on"] = json!(blocked_on);
    }
    Ok(result)
}

/// Final child-success compare-and-set. The snapshot that selected the
/// success arm is advisory; this statement is the authority and rechecks that
/// the child is closed and quiescent in the writer transaction.
pub(crate) async fn guarded_child_success_flip_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    child_track_id: &str,
    target_status: &str,
    now: i64,
    finished_at_ms: Option<i64>,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE tasks SET status=?1,status_detail=NULL,worker_card_id=NULL,\
                running_deadline_ms=NULL,updated_at_ms=?2,finished_at_ms=?3 \
           WHERE id=?4 AND child_track_id=?5 \
             AND status IN ('dispatched','running') \
             AND EXISTS(SELECT 1 FROM tracks child \
                WHERE child.id=?5 AND child.closed_at IS NOT NULL) \
             AND NOT EXISTS(SELECT 1 FROM current_tasks ct WHERE ct.track_id=?5 \
                AND ct.status IN ('pending','dispatched','running','verifying'))",
    )
    .bind(target_status)
    .bind(now)
    .bind(finished_at_ms)
    .bind(task_id)
    .bind(child_track_id)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// One success writer for explicit child reports and scheduler recovery. The caller owns the
/// surrounding transaction, including any child close, so failed CAS or event writes roll back.
pub(crate) async fn success_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    child_track_id: &str,
    gated: bool,
) -> Result<Option<crate::event::Event>> {
    let now = crate::model::now_ms();
    let status = if gated { "verifying" } else { "done" };
    if guarded_child_success_flip_tx(
        tx,
        task_id,
        child_track_id,
        status,
        now,
        (!gated).then_some(now),
    )
    .await?
        != 1
    {
        return Ok(None);
    }
    Ok(Some(crate::event::Event::TaskCompleted {
        notification: if gated {
            calm_types::event::PlannerNotification::Suppress
        } else {
            calm_types::event::PlannerNotification::Notify
        },
        idempotency_key: task_id.into(),
        result: result_ref_tx(tx, child_track_id, None).await?,
        artifacts: vec![],
        agent_message: None,
    }))
}

//! One terminal task completion authority for Legacy exits and owned Pipe settlement.
use super::*;
use crate::db::sqlite::{append_decision_event_in_tx, track_find_tx};
use crate::event::{BroadcastEnvelope, SYNC_EVENT_VERSION};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn complete_terminal_task_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    track_id_str: &str,
    worker_card_id: &str,
    exit_code: Option<i32>,
    signal_killed: bool,
    pty_output: &str,
    pty_output_truncated: bool,
) -> Result<Vec<BroadcastEnvelope>> {
    let Some(track) = track_find_tx(tx, track_id_str).await? else {
        return Ok(Vec::new());
    };
    let scope = EventScope::Track {
        track: track.id,
        area: track.area_id,
    };
    let success = !signal_killed && exit_code == Some(0);
    let task_id = task_id.to_string();
    let now = now_ms();
    // The payload `idempotency_key` is mutable via `PATCH /api/cards/{id}`, so it is NOT
    // proof of ownership: only the card the worker-spawn op actually created may flip an
    // UNSTAMPED row; a forged-payload card fails both sides → 0 rows → no event.
    let owns_key =
        crate::db::sqlite::worker_op_targets_card_tx(tx, &task_id, worker_card_id).await?;
    let reporter = TaskReporter::Card {
        card_id: worker_card_id,
        owns_key,
    };
    // A gated terminal task's clean exit is still a self-report: the row goes to `verifying`.
    let (rows, event) = if success {
        let flip =
            task_report_success_from_worker_tx(tx, &task_id, track_id_str, reporter, now).await?;
        (
            if flip == SuccessReportFlip::None {
                0
            } else {
                1
            },
            Event::TaskCompleted {
                notification: if flip == SuccessReportFlip::Done {
                    calm_types::event::PlannerNotification::Notify
                } else {
                    calm_types::event::PlannerNotification::Suppress
                },
                idempotency_key: task_id.clone(),
                result: json!({
                    "exit_code": 0,
                    "source": "terminal-exit",
                    "pty_output": pty_output,
                    "pty_output_truncated": pty_output_truncated,
                }),
                artifacts: Vec::new(),
                agent_message: None,
            },
        )
    } else {
        let reason = if signal_killed {
            "terminal worker killed by signal".to_string()
        } else {
            match exit_code {
                Some(-1) => {
                    "terminal worker exited while the kernel was down (outcome unknown)".to_string()
                }
                Some(code) => format!("terminal worker exited with code {code}"),
                None => "terminal worker exited without an exit code".to_string(),
            }
        };
        (
            // The same interpreted reason the event carries lands on the row.
            task_fail_from_worker_tx(
                tx,
                &task_id,
                track_id_str,
                reporter,
                &status_detail_with_reason("worker-reported", &reason),
                now,
            )
            .await?,
            Event::TaskFailed {
                notification: calm_types::event::PlannerNotification::Notify,
                idempotency_key: task_id.clone(),
                reason,
                details: Some(json!({
                    "exit_code": exit_code,
                    "signal_killed": signal_killed,
                    "source": "terminal-exit",
                    "pty_output": pty_output,
                    "pty_output_truncated": pty_output_truncated,
                })),
                agent_message: None,
            },
        )
    };
    if rows == 0 {
        // First writer already won (live hook vs sweep, or a
        // belt-and-suspenders worker self-report) — no row
        // change, no event.
        return Ok(Vec::new());
    }
    let event_id =
        append_decision_event_in_tx(tx, &ActorId::KernelDispatcher, &scope, None, &event).await?;
    Ok(vec![BroadcastEnvelope {
        event_version: SYNC_EVENT_VERSION,
        id: event_id,
        actor: ActorId::KernelDispatcher,
        scope,
        event,
    }])
}

pub(super) async fn terminal_task_uses_pipe(repo: &dyn Repo, task_id: &str) -> bool {
    let Some(pool) = repo.sqlite_db() else {
        return true;
    };
    let result: std::result::Result<Option<bool>, _> = sqlx::query_scalar(
        "SELECT json_type(tx_output_json,'$.data.terminal_worker_pipe') IS NOT NULL \
         FROM operations WHERE kind='terminal-worker' AND idempotency_key=?1",
    )
    .bind(task_id)
    .fetch_optional(pool.read_pool())
    .await;
    result.map(|value| value.unwrap_or(false)).unwrap_or(true)
}

impl Scheduler {
    pub(super) async fn acknowledge_pipe_parked(
        &self,
        runtime: &OperationRuntime,
        task: &Task,
    ) -> Result<bool> {
        if task.kind != TaskKind::Terminal {
            return Ok(false);
        }
        let Some(op) = runtime
            .find_by_kind_and_idempotency("terminal-worker", &task.id)
            .await?
        else {
            return Ok(false);
        };
        if op.phase == crate::operation::Phase::Parked
            && op
                .tx_output
                .as_ref()
                .is_some_and(crate::operation::terminal_adapter::pipe_worker::is_pipe)
        {
            self.mark_running(&task.id, op.target_id.as_deref()).await?;
            return Ok(true);
        }
        Ok(false)
    }
}

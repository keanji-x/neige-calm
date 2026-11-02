//! Durable inspection requests. Elapsed time is never task settlement authority.
use super::*;
use crate::db::sqlite::{task_current_get_tx, track_get_tx};

pub(crate) const STALL_AFTER_MS: i64 = 15 * 60 * 1000;
pub(crate) const REVIEW_AFTER_MS: i64 = 15 * 60 * 1000;
pub(crate) const WAKE_SOURCE: &str = "task-stall";

#[derive(sqlx::FromRow)]
pub(crate) struct Notice {
    pub task_id: String,
    pub phase: String,
    pub phase_updated_at_ms: i64,
    pub gate_attempt: i64,
    pub notice_id: String,
    pub next_review_at_ms: i64,
    pub decision_json: Option<String>,
}

pub(crate) fn watched(task: &Task) -> bool {
    matches!(
        task.status,
        TaskStatus::Pending | TaskStatus::Dispatched | TaskStatus::Verifying
    ) || (task.status == TaskStatus::Running && !task_has_running_liveness_deadline(task))
}

pub(crate) fn phase(task: &Task) -> String {
    serde_json::to_value(task.status)
        .expect("task status")
        .as_str()
        .expect("status string")
        .to_owned()
}

impl Notice {
    pub(crate) fn matches(&self, task: &Task) -> bool {
        self.task_id == task.id
            && self.phase == phase(task)
            && self.phase_updated_at_ms == task.updated_at_ms
            && self.gate_attempt == task.gate_attempt
    }
    pub(crate) fn response(&self, decision: &str) -> Value {
        json!({"attempt_id":self.task_id,"notice_id":self.notice_id,"decision":decision,
            "review_at": if decision == "terminate" { Value::Null } else { json!(crate::time_format::at(self.next_review_at_ms)) }})
    }
}

pub(crate) async fn notice_tx(tx: &mut Tx<'_>, task: &str) -> Result<Option<Notice>> {
    Ok(sqlx::query_as(
        "SELECT task_id,phase,phase_updated_at_ms,gate_attempt,notice_id,
                     next_review_at_ms,decision_json
              FROM task_followups WHERE task_id=?1",
    )
    .bind(task)
    .fetch_optional(&mut **tx)
    .await?)
}

pub(crate) async fn current_tx(tx: &mut Tx<'_>, track: &str, attempt: &str) -> Result<Task> {
    let task = task_get_tx(tx, attempt)
        .await?
        .ok_or_else(|| CalmError::NotFound("task attempt".into()))?;
    if task.track_id != track
        || task_current_get_tx(tx, track, &task.key)
            .await?
            .is_none_or(|current| current.id != attempt)
    {
        return Err(CalmError::Conflict("current task attempt changed".into()));
    }
    if !track_get_tx(tx, &TrackId::from(track)).await?.is_open() {
        return Err(CalmError::Conflict("track is closed".into()));
    }
    Ok(task)
}

impl Scheduler {
    pub(super) async fn inspect_stalls(&self) {
        let tasks = match self.repo.tasks_nonterminal().await {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::warn!(%error, "task stall scan failed");
                return;
            }
        };
        for task in tasks {
            if !watched(&task) || now_ms().saturating_sub(task.updated_at_ms) < STALL_AFTER_MS {
                continue;
            }
            if let Err(error) = self.inspect_stall(task).await
                && !is_race_lost(&error)
            {
                tracing::warn!(%error, "task stall observation failed; next sweep retries");
            }
        }
    }

    async fn inspect_stall(&self, snapshot: Task) -> Result<()> {
        write_with_actor_events_typed(self.repo.as_ref(), None, &self.events, &self.write, move |tx| {
            Box::pin(async move {
                // A stale scan never wakes a successor or a closed Track.
                let task = match current_tx(tx, &snapshot.track_id, &snapshot.id).await {
                    Ok(task) => task,
                    Err(CalmError::Conflict(_) | CalmError::NotFound(_)) => return Err(race_lost_err()),
                    Err(error) => return Err(error),
                };
                let now = now_ms();
                if !watched(&task) || now.saturating_sub(task.updated_at_ms) < STALL_AFTER_MS { return Err(race_lost_err()); }
                let prior = notice_tx(tx, &task.id).await?;
                if prior.as_ref().is_some_and(|notice| notice.matches(&task) && now < notice.next_review_at_ms) { return Err(race_lost_err()); }
                let reason = match task.status {
                    TaskStatus::Pending => {
                        let siblings = tasks_by_track_tx(tx, &task.track_id).await?;
                        let occupancy = crate::db::sqlite::checkout_occupancy(tx, &task.track_id, &task.id).await?;
                        match crate::db::sqlite::checkout_admission(&siblings, occupancy).into_iter().find(|(row, _)| row.id == task.id) {
                            None => "waiting_dependencies",
                            Some((_, Some(crate::db::sqlite::CheckoutWait::InUse))) => "checkout_in_use",
                            Some((_, Some(crate::db::sqlite::CheckoutWait::WriterAhead))) => "writer_ahead",
                            Some((_, None)) => "awaiting_admission",
                        }
                    }
                    TaskStatus::Dispatched => "awaiting_spawn_outcome",
                    TaskStatus::Verifying => "awaiting_gate_settlement",
                    TaskStatus::Running => "awaiting_task_outcome",
                    _ => return Err(race_lost_err()),
                };
                let notice = new_id();
                let next = now.saturating_add(REVIEW_AFTER_MS);
                sqlx::query("INSERT INTO task_followups(
                    task_id,phase,phase_updated_at_ms,gate_attempt,notice_id,next_review_at_ms,decision_json
                 ) VALUES(?1,?2,?3,?4,?5,?6,NULL)
                 ON CONFLICT(task_id) DO UPDATE SET phase=excluded.phase,
                    phase_updated_at_ms=excluded.phase_updated_at_ms,gate_attempt=excluded.gate_attempt,
                    notice_id=excluded.notice_id,next_review_at_ms=excluded.next_review_at_ms,decision_json=NULL")
                    .bind(&task.id).bind(phase(&task)).bind(task.updated_at_ms).bind(task.gate_attempt).bind(&notice).bind(next).execute(&mut **tx).await?;
                let fact = json!({"task_key":task.key,"attempt_id":task.id,"notice_id":notice,
                    "phase":task.status,"gate_attempt":task.gate_attempt,"reason":reason,
                    "elapsed_seconds":now.saturating_sub(task.updated_at_ms)/1000,
                    "cancel_available":task.status == TaskStatus::Pending,
                    "previous_decision":prior.filter(|n| n.matches(&task))
                        .and_then(|n| n.decision_json)
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())});
                let text = include_str!("../../prompts/task_stall_notice.md")
                    .trim_end().replace("{fact}", &fact.to_string());
                let track = track_get_tx(tx, &TrackId::from(task.track_id)).await?;
                Ok(((), vec![(ActorId::KernelDispatcher, EventScope::Track { track:track.id.clone(), area:track.area_id }, Event::TrackWakeRequested {track_id:track.id,source:WAKE_SOURCE.into(),key:notice,text})]))
            })
        }).await.map(|_| ())
    }
}

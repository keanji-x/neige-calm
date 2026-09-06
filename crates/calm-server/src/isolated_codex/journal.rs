//! Same Operation lease and one private compare-and-save record, including cleanup.
use super::record::{Admission, ProviderRecord, RunRecord, needs_start_permission};
use super::{OPERATION_KIND, WorkerPayload};
use crate::db::{RouteRepo, write_with_events_typed};
use crate::dedicated_codex::{self, Checkpoint, RequestPhase, SessionRecord};
use crate::error::{CalmError, Result};
use crate::operation::{Operation, Tx, TxOutput};
use std::sync::Arc;

pub(crate) async fn load_tx(tx: &mut Tx<'_>, op_id: &str) -> Result<RunRecord> {
    let row: Option<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT kind,payload_json,target_type,target_id,tx_output_json FROM operations WHERE id=?1",
    )
    .bind(op_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (kind, payload, target_type, target_id, output) =
        row.ok_or_else(|| CalmError::Conflict("isolated Operation missing".into()))?;
    if kind != OPERATION_KIND {
        return Err(CalmError::Conflict(
            "Operation does not own an isolated endpoint".into(),
        ));
    }
    let request: WorkerPayload = serde_json::from_str(&payload)?;
    let output: TxOutput = serde_json::from_str(
        &output
            .ok_or_else(|| CalmError::Conflict("isolated preparation receipt missing".into()))?,
    )?;
    let record = RunRecord::from_output(&output)?;
    let identity = &record.request.identity;
    if identity.run_id != op_id
        || identity.attempt_id != request.task_id
        || record.track_id != request.track_id
        || request.idempotency_key != request.task_id
        || request.actor != crate::ids::ActorId::KernelDispatcher
        || target_type != "card"
        || target_id.as_deref() != Some(&identity.card_id)
    {
        return Err(CalmError::Conflict(
            "isolated endpoint binding changed".into(),
        ));
    }
    Ok(record)
}

pub(crate) async fn require_owner_tx(tx: &mut Tx<'_>, op: &Operation) -> Result<()> {
    let owner = op
        .lease_owner
        .as_deref()
        .ok_or_else(|| CalmError::Conflict("isolated checkpoint requires owned lease".into()))?;
    let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND kind=?2 AND lease_owner=?3 AND phase IN ('spawn_started','parked','compensating'))")
        .bind(&op.id).bind(OPERATION_KIND).bind(owner).fetch_one(&mut **tx).await?;
    if !owned {
        return Err(CalmError::Conflict(
            "isolated Operation lease or phase changed".into(),
        ));
    }
    Ok(())
}

async fn replace_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    current: &RunRecord,
    next: &RunRecord,
) -> Result<()> {
    require_owner_tx(tx, op).await?;
    if load_tx(tx, &op.id).await? != *current
        || current.request != next.request
        || current.track_id != next.track_id
        || current.native_token != next.native_token
        || (current.admission == Admission::Closed && next.admission != Admission::Closed)
    {
        return Err(CalmError::Conflict(
            "stale or changed isolated execution checkpoint".into(),
        ));
    }
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.isolated_execution',json(?1)),lease_until_ms=?2,updated_at_ms=?3 WHERE id=?4 AND lease_owner=?5")
        .bind(serde_json::to_string(next)?).bind(crate::model::now_ms().saturating_add(60_000))
        .bind(crate::model::now_ms()).bind(&op.id).bind(&op.lease_owner).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn prepared_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    session: SessionRecord,
) -> Result<()> {
    let current = load_tx(tx, &op.id).await?;
    if current.provider != ProviderRecord::Unprepared || session.endpoint.request != current.request
    {
        return Err(CalmError::Conflict(
            "prepared isolated endpoint request changed".into(),
        ));
    }
    let mut next = current.clone();
    next.provider = ProviderRecord::Prepared(Box::new(session));
    replace_tx(tx, op, &current, &next).await
}

pub(crate) struct OperationCheckpoint {
    pub repo: Arc<dyn RouteRepo>,
    pub operation: Operation,
    pub events: crate::event::EventBus,
    pub write: crate::state::WriteContext,
    pub task_timeout_ms: i64,
}
#[async_trait::async_trait]
impl Checkpoint for OperationCheckpoint {
    async fn save(
        &self,
        expected: &SessionRecord,
        next: &SessionRecord,
    ) -> dedicated_codex::Result<()> {
        let expected = expected.clone();
        let next = next.clone();
        let op = self.operation.clone();
        let timeout_ms = self.task_timeout_ms;
        write_with_events_typed(
            self.repo.as_ref(),
            crate::ids::ActorId::KernelDispatcher,
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let intent = needs_start_permission(&expected, &next)?;
                    let current = load_tx(tx, &op.id).await?;
                    if current.session()? != &expected {
                        return Err(CalmError::Conflict(
                            "stale isolated controller checkpoint".into(),
                        ));
                    }
                    if intent {
                        if current.admission != Admission::Open {
                            return Err(CalmError::Conflict("isolated admission is closed".into()));
                        }
                        super::admission::validate_start_tx(tx, &op).await?;
                    }
                    let mut updated = current.clone();
                    updated.provider = ProviderRecord::Prepared(Box::new(next.clone()));
                    replace_tx(tx, &op, &current, &updated).await?;
                    let events = if expected.phase != next.phase {
                        bind_acknowledgement_tx(tx, &current, &next, timeout_ms).await?
                    } else {
                        Vec::new()
                    };
                    Ok(((), events))
                })
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| dedicated_codex::Error::Conflict(error.to_string()))
    }
}

async fn bind_acknowledgement_tx(
    tx: &mut Tx<'_>,
    record: &RunRecord,
    next: &SessionRecord,
    timeout_ms: i64,
) -> Result<Vec<(crate::event::EventScope, crate::event::Event)>> {
    use crate::session_projection_repo::{AgentProvider, ThreadAttribution};
    let (thread_id, turn_id) = match &next.phase {
        RequestPhase::ThreadReady { thread_id } => (thread_id, None),
        RequestPhase::TurnActive {
            thread_id, turn_id, ..
        } => (thread_id, Some(turn_id.clone())),
        _ => return Ok(Vec::new()),
    };
    let id = &record.request.identity.session_id;
    let Some(session) = crate::db::sqlite::session_projection_by_id_tx(tx, id).await? else {
        return Ok(Vec::new());
    };
    let spawn_op_id: Option<String> =
        sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    if spawn_op_id.as_deref() != Some(&record.request.identity.run_id)
        || session.card_id != record.request.identity.card_id
    {
        return Err(CalmError::Conflict(
            "isolated acknowledgement session binding changed".into(),
        ));
    }
    crate::db::sqlite::session_bind_attribution_tx(
        tx,
        id,
        ThreadAttribution {
            worker_session_id: id.clone(),
            provider: AgentProvider::Codex,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: turn_id,
        },
    )
    .await?;
    if session.status.is_terminal() {
        return Ok(Vec::new());
    }
    let status = if matches!(next.phase, RequestPhase::TurnActive { .. }) {
        crate::scheduler::mark_acknowledged_running_tx(
            tx,
            &record.request.identity.attempt_id,
            Some(&record.request.identity.card_id),
            timeout_ms,
        )
        .await?;
        crate::session_projection_repo::WorkerSessionState::Running
    } else {
        session.status
    };
    crate::db::sqlite::session_set_status_tx(tx, id, status).await?;
    let area_id: Option<String> = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
        .bind(&record.track_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(area_id) = area_id else {
        return Ok(Vec::new());
    };
    Ok(vec![(
        crate::event::EventScope::Card {
            card: record.request.identity.card_id.clone().into(),
            track: record.track_id.clone().into(),
            area: area_id.into(),
        },
        crate::event::Event::WorkerSessionStarted {
            worker_session_id: id.clone(),
            card_id: record.request.identity.card_id.clone(),
            kind: crate::session_projection_repo::WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status,
        },
    )])
}

pub(crate) async fn close_tx(tx: &mut Tx<'_>, op: &Operation) -> Result<RunRecord> {
    let current = load_tx(tx, &op.id).await?;
    let mut next = current.clone();
    next.admission = Admission::Closed;
    replace_tx(tx, op, &current, &next).await?;
    Ok(next)
}

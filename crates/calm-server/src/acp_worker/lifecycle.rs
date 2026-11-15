//! Native round settlement owns task release. A report alone never proves the agent stopped.
use super::{CancelRequest, admission::Admission};
use crate::{
    acp_session::host::AcpHost,
    db::{Repo, write_in_tx_typed},
    error::{CalmError, Result},
    event::{Event, EventScope},
    ids::ActorId,
    model::{TaskStatus, now_ms},
    operation::{
        SpawnCtx, Tx,
        workspace_lease::{
            EndedAttempt, ReleaseDelivery, release_ended_attempt_lease_for_card_tx,
            release_workspace_lease_for_card_tx,
        },
    },
    state::WriteContext,
};
use calm_truth::db::sqlite::provider_execution as claims;
use calm_types::{
    acp_worker::{AcpWorkerState, WorkerPendingReport, WorkerReportOutcome, WorkerTurnPhase},
    event::PlannerNotification,
};
use std::{sync::atomic::Ordering, time::Duration};

/// Captured by the native driver, consumed only by the ACP execution owner.
pub struct WorkerEnd {
    pub(crate) admission: Admission,
    pub(crate) ctx: SpawnCtx,
    pub(crate) known: bool,
    pub(crate) cleanup_confirmed: bool,
}
fn write_context() -> WriteContext {
    WriteContext::new(
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    )
}

use crate::task_control::write_worker_events;

pub(crate) async fn pending_finalize_tx(tx: &mut Tx<'_>, attempt: &str) -> Result<bool> {
    let row:Option<(String,Option<String>)>=sqlx::query_as("SELECT w.id,w.handle_state_json FROM tasks t JOIN cards c ON c.id=t.worker_card_id JOIN worker_sessions w ON w.id=c.session_id WHERE t.id=?1 AND json_extract(w.handle_state_json,'$.requested_selection.provider') IS NOT NULL").bind(attempt).fetch_optional(&mut **tx).await?;
    let Some((_, Some(json))) = row else {
        return Ok(false);
    };
    let state: AcpWorkerState = serde_json::from_str(&json)?;
    Ok(state.attempt_id == attempt
        && (state.release_intent
            || state.turn_phase == WorkerTurnPhase::Unknown && !state.cleanup_confirmed))
}

pub(crate) async fn defer_report_tx(
    tx: &mut Tx<'_>,
    session: &str,
    attempt: &str,
    report: &crate::decision_sink::WorkerTaskReport,
) -> Result<bool> {
    let applicable:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM worker_sessions WHERE id=?1 AND json_extract(handle_state_json,'$.requested_selection.provider') IS NOT NULL)").bind(session).fetch_one(&mut **tx).await?;
    if !applicable {
        return Ok(false);
    };
    let state = crate::db::sqlite::acp_worker_state_tx(tx, session, attempt).await?;
    if state.release_intent {
        return Err(CalmError::Conflict(
            "ACP Worker already has a pending report".into(),
        ));
    }
    let mut next = state.clone();
    next.release_intent = true;
    next.pending_report = Some(match report {
        crate::decision_sink::WorkerTaskReport::Completed {
            result,
            artifacts,
            commit_message,
            ..
        } => WorkerPendingReport {
            outcome: WorkerReportOutcome::Completed {
                result: result.clone(),
                artifacts: artifacts.clone(),
            },
            commit_message: commit_message
                .as_ref()
                .map(|message| message.as_str().into()),
        },
        crate::decision_sink::WorkerTaskReport::Failed { reason, .. } => WorkerPendingReport {
            outcome: WorkerReportOutcome::Failed {
                reason: reason.clone(),
            },
            commit_message: None,
        },
    });
    let turn: Option<String> =
        sqlx::query_scalar("SELECT active_turn_id FROM worker_sessions WHERE id=?1")
            .bind(session)
            .fetch_one(&mut **tx)
            .await?;
    if !crate::db::sqlite::acp_worker_state_cas_tx(
        tx,
        session,
        attempt,
        turn.as_deref(),
        &state,
        &next,
    )
    .await?
    {
        return Err(CalmError::Conflict(
            "ACP Worker report lost its turn".into(),
        ));
    }
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
        .bind(session)
        .execute(&mut **tx)
        .await?;
    Ok(true)
}

async fn finalize_tx(
    tx: &mut Tx<'_>,
    session: &str,
    state: &mut AcpWorkerState,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    if !state.cleanup_confirmed {
        return Ok(vec![]);
    }
    let Some(report) = state.pending_report.clone() else {
        return Ok(vec![]);
    };
    let row = crate::db::sqlite::session_get_tx(tx, &session.to_owned().into())
        .await?
        .ok_or_else(|| CalmError::Conflict("ACP Worker disappeared before finalize".into()))?;
    let card = row
        .card_id
        .ok_or_else(|| CalmError::Conflict("ACP Worker has no card".into()))?;
    let task = crate::db::sqlite::task_get_tx(tx, &state.attempt_id)
        .await?
        .ok_or_else(|| CalmError::Conflict("ACP Worker task disappeared before finalize".into()))?;
    let delivery = match &report.outcome {
        WorkerReportOutcome::Completed { .. } => match report.commit_message {
            Some(text) => ReleaseDelivery::CommitWorkerMessage(
                crate::git_candidate::commit_message::CommitMessage::parse(&text)
                    .map_err(CalmError::BadRequest)?,
            ),
            None => {
                ReleaseDelivery::Commit(crate::git_candidate::delivery::AttemptOutcome::Completed)
            }
        },
        WorkerReportOutcome::Failed { .. } => {
            ReleaseDelivery::Commit(crate::git_candidate::delivery::AttemptOutcome::Failed)
        }
    };
    let mut events = release_workspace_lease_for_card_tx(tx, card.as_str(), delivery).await?;
    let suppress = task.status == TaskStatus::Verifying
        || matches!(report.outcome, WorkerReportOutcome::Completed { .. })
            && crate::git_candidate::delivery::delivery_latest_for_attempt_tx(tx, &task.id)
                .await?
                .is_some();
    let area = sqlx::query_scalar::<_, String>("SELECT area_id FROM tracks WHERE id=?1")
        .bind(&task.track_id)
        .fetch_one(&mut **tx)
        .await?;
    events.push((
        ActorId::Kernel,
        EventScope::Card {
            card,
            track: task.track_id.into(),
            area: area.into(),
        },
        Event::TaskReportSettled {
            idempotency_key: task.id,
            notification: if suppress {
                PlannerNotification::Suppress
            } else {
                PlannerNotification::Notify
            },
            outcome: report.outcome,
        },
    ));
    state.release_intent = false;
    state.pending_report = None;
    Ok(events)
}

/// Under the same writer that admits messages, either take the next queued notification or
/// close admission and record the existing worker failure. No message can enter the closing gap.
pub(crate) async fn round_action(
    ctx: &SpawnCtx,
    admission: &Admission,
    known: bool,
) -> Result<bool> {
    let owner = admission.clone();
    write_worker_events(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &write_context(),
        move |tx| {
            Box::pin(async move {
                let state = owner.check_tx(tx, true).await?;
                let messages =
                    crate::db::sqlite::planner_messages_for_attempt_tx(tx, &owner.attempt).await?;
                if known
                    && !state.release_intent
                    && state.timeout_cleanup.is_none()
                    && owner.check_tx(tx, false).await.is_ok()
                    && messages
                        .iter()
                        .any(|message| message.seq > state.notified_through_seq)
                {
                    return Ok((true, vec![]));
                }
                let task = crate::db::sqlite::task_get_tx(tx, &owner.attempt)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Conflict("Worker task missing at round end".into())
                    })?;
                let mut events = vec![];
                if !state.release_intent
                    && matches!(task.status, TaskStatus::Dispatched | TaskStatus::Running)
                {
                    let reason = if known {
                        "ACP Worker turn ended without a task report or queued Planner notification"
                    } else {
                        "ACP Worker turn outcome is unknown"
                    };
                    let class = if known {
                        "worker-turn-ended"
                    } else {
                        "worker-session-unknown"
                    };
                    crate::db::sqlite::task_fail_from_worker_tx(
                        tx,
                        &owner.attempt,
                        &owner.track,
                        crate::db::sqlite::TaskReporter::Card {
                            card_id: &owner.card,
                            owns_key: true,
                        },
                        &crate::db::sqlite::status_detail_with_reason(class, reason),
                        now_ms(),
                    )
                    .await?;
                    let mut next = state.clone();
                    next.release_intent = true;
                    next.pending_report = Some(WorkerPendingReport {
                        outcome: WorkerReportOutcome::Failed {
                            reason: reason.into(),
                        },
                        commit_message: None,
                    });
                    if !crate::db::sqlite::acp_worker_state_cas_tx(
                        tx,
                        &owner.worker,
                        &owner.attempt,
                        owner.turn.as_deref(),
                        &state,
                        &next,
                    )
                    .await?
                    {
                        return Err(CalmError::Conflict(
                            "ACP Worker round end lost ownership".into(),
                        ));
                    }
                    let area: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
                        .bind(&owner.track)
                        .fetch_one(&mut **tx)
                        .await?;
                    events.push((
                        ActorId::Kernel,
                        EventScope::Card {
                            card: owner.card.clone().into(),
                            track: owner.track.clone().into(),
                            area: area.into(),
                        },
                        crate::decision_sink::WorkerTaskReport::Failed {
                            attempt_id: owner.attempt.clone(),
                            reason: reason.into(),
                        }
                        .event(PlannerNotification::Suppress),
                    ));
                }
                sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
                    .bind(&owner.worker)
                    .execute(&mut **tx)
                    .await?;
                Ok((false, events))
            })
        },
    )
    .await
    .map(|(next, _)| next)
}

pub(crate) async fn complete(
    session: &str,
    record: &crate::provider_execution::Record,
    end: WorkerEnd,
) -> Result<()> {
    let claims::Purpose::WorkerTurn {
        attempt_id,
        turn_id,
        native_session_id,
    } = &record.purpose
    else {
        return Err(CalmError::Conflict(
            "wrong Worker completion purpose".into(),
        ));
    };
    if session != end.admission.worker
        || attempt_id != &end.admission.attempt
        || end.admission.turn.as_ref() != Some(turn_id)
        || end.admission.native.as_ref() != Some(native_session_id)
    {
        return Err(CalmError::Conflict(
            "ACP Worker completion identity changed".into(),
        ));
    }
    let admission = end.admission.clone();
    let record = record.clone();
    write_worker_events(end.ctx.repo.as_ref(),None,&end.ctx.events,&write_context(),move|tx|Box::pin(async move {
        let Some(claim)=claims::load(&mut **tx,&admission.worker).await?.filter(|claim|claim.record()==&record) else{return Ok(((),vec![]))};
        let state=admission.check_tx(tx,true).await?;let mut next=state.clone();
        next.turn_phase=if end.known{WorkerTurnPhase::Settled}else{WorkerTurnPhase::Unknown};next.cleanup_confirmed=end.cleanup_confirmed;
        if !end.cleanup_confirmed && (next.release_intent || !end.known) && next.timeout_cleanup.is_none() {
            next.timeout_cleanup=Some(calm_types::acp_worker::WorkerTimeoutCleanup{task_id:admission.attempt.clone(),requested_at_ms:now_ms(),reason:"ACP Worker native cleanup pending".into()});
        }
        let events=finalize_tx(tx,&admission.worker,&mut next).await?;
        if !crate::db::sqlite::acp_worker_state_cas_tx(tx,&admission.worker,&admission.attempt,admission.turn.as_deref(),&state,&next).await?{return Err(CalmError::Conflict("ACP Worker settlement lost its turn".into()))}
        if end.cleanup_confirmed || end.known && !next.release_intent && next.timeout_cleanup.is_none() {
            let claims::Cas::Applied(())=claims::clear_on_completion(tx,&admission.worker,&claim).await? else{return Err(CalmError::Conflict("ACP Worker settlement lost its claim".into()))};
            sqlx::query("UPDATE worker_sessions SET active_turn_id=NULL, state=CASE WHEN ?2 THEN 'exited' ELSE 'running' END, updated_at_ms=?3 WHERE id=?1 AND active_turn_id=?4").bind(&admission.worker).bind(end.cleanup_confirmed).bind(now_ms()).bind(admission.turn.as_deref()).execute(&mut **tx).await?;
        }
        Ok(((),events))
    })).await.map(|_: ((),Vec<i64>)|())
}

pub(crate) async fn stop_session(
    host: &AcpHost,
    workers: &super::Workers,
    session: &str,
    expected_turn: Option<&str>,
) -> Result<()> {
    if let Some(control) = workers.get(session) {
        if expected_turn.is_some_and(|turn| control.turn != turn) {
            return Err(CalmError::Conflict(
                "ACP Worker cancellation belongs to a different turn".into(),
            ));
        }
        if *control.settled.borrow() {
            return Ok(());
        }
        control.authorized.store(false, Ordering::Release);
        control.admission.check(true).await?;
        control.admission.revoke().await?;
        let mut settled = control.settled.clone();
        if !*settled.borrow() && control.cancel.send(CancelRequest::Requested).is_err() {
            return finish_orphan(host, workers, control).await;
        }
        let wait = tokio::time::timeout(Duration::from_secs(12), async {
            while !*settled.borrow() {
                if settled.changed().await.is_err() {
                    return false;
                }
            }
            true
        })
        .await
        .map_err(|_| CalmError::Conflict("ACP Worker stop did not settle".into()))?;
        if !wait {
            return finish_orphan(host, workers, control).await;
        }
        return Ok(());
    }
    let session = session.to_owned();
    let clean = write_in_tx_typed(host.completion.repo()?.as_ref(), move |tx| {
        Box::pin(async move {
            let json: Option<String> =
                sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                    .bind(&session)
                    .fetch_one(&mut **tx)
                    .await?;
            Ok(json
                .map(|json| serde_json::from_str::<AcpWorkerState>(&json))
                .transpose()?
                .is_some_and(|state| state.cleanup_confirmed))
        })
    })
    .await?;
    if clean {
        Ok(())
    } else {
        Err(CalmError::Conflict(
            "ACP Worker has no confirmed round cleanup".into(),
        ))
    }
}

/// A retained round whose driver exited still owns its exact cleanup witness and claim.
/// Reconciliation may close that attachment; it never adopts a successor or kills a sibling.
async fn finish_orphan(
    host: &AcpHost,
    workers: &super::Workers,
    control: std::sync::Arc<super::Control>,
) -> Result<()> {
    crate::acp_session::server::detach_worker_with_witness(
        host,
        &control.admission.worker,
        Some((&control.native, &control.cleanup)),
    )
    .await?;
    if !control.cleanup.confirmed()
        || !(control.round_ended.load(Ordering::Acquire)
            || control.cleanup.generation.stopped.load(Ordering::Acquire))
    {
        return Err(CalmError::Conflict(
            "ACP Worker orphan cleanup is unconfirmed".into(),
        ));
    }
    let owner = control.admission.clone();
    let claim=write_in_tx_typed(control.ctx.repo.as_ref(),move|tx|Box::pin(async move {
        let state=owner.check_tx(tx,true).await?;
        let claim=claims::load(&mut **tx,&owner.worker).await?;
        if state.cleanup_confirmed {return Ok(None)}
        Ok(claim.filter(|claim|matches!(&claim.record().purpose,claims::Purpose::WorkerTurn{turn_id,attempt_id,native_session_id} if Some(turn_id)==owner.turn.as_ref() && attempt_id==&owner.attempt && Some(native_session_id)==owner.native.as_ref())))
    })).await?;
    if let Some(claim) = claim {
        round_action(&control.ctx, &control.admission, false).await?;
        host.provider_execution
            .owner
            .complete(
                &control.admission.worker,
                claim.record(),
                crate::provider_execution::CompletionEvidence::Unconfirmed,
                Some(crate::provider_execution::KnownEnd::AcpWorkerTurn(
                    Box::new(WorkerEnd {
                        admission: control.admission.clone(),
                        ctx: control.ctx.clone(),
                        known: false,
                        cleanup_confirmed: true,
                    }),
                )),
            )
            .await?;
    }
    fail_and_finalize(
        &control.ctx,
        &control.admission.worker,
        control.admission.credential.clone(),
        "ACP Worker native cleanup settled after driver exit",
    )
    .await?;
    let mut workers = workers.0.lock().expect("ACP Workers");
    if workers
        .get(&control.admission.worker)
        .is_some_and(|current| std::sync::Arc::ptr_eq(current, &control))
    {
        workers.remove(&control.admission.worker);
    }
    Ok(())
}

pub(crate) async fn fail_and_finalize(
    ctx: &SpawnCtx,
    session: &str,
    expected_credential: Option<String>,
    reason: &str,
) -> Result<()> {
    let session = session.to_owned();
    let reason = reason.to_owned();
    write_worker_events(ctx.repo.as_ref(),None,&ctx.events,&write_context(),move|tx|Box::pin(async move {
        let row=crate::db::sqlite::session_get_tx(tx,&session.clone().into()).await?.ok_or_else(||CalmError::Conflict("ACP Worker missing".into()))?;
        let mut state: AcpWorkerState=serde_json::from_value(row.handle_state_json.ok_or_else(||CalmError::Conflict("ACP Worker state missing".into()))?)?;
        if expected_credential.is_some() && state.credential_generation != expected_credential { return Err(CalmError::Conflict("ACP Worker cleanup belongs to a retired generation".into())); }
        if !state.cleanup_confirmed {return Err(CalmError::Conflict("ACP Worker failure cleanup remains unconfirmed".into()))}
        let mut events=vec![];
        if state.pending_report.is_none() {
            let task=crate::db::sqlite::task_get_tx(tx,&state.attempt_id).await?.ok_or_else(||CalmError::Conflict("ACP Worker task missing".into()))?;
            if matches!(task.status,TaskStatus::Dispatched|TaskStatus::Running){
                crate::db::sqlite::task_fail_from_worker_tx(tx,&task.id,&task.track_id,crate::db::sqlite::TaskReporter::Card{card_id:row.card_id.as_ref().ok_or_else(||CalmError::Conflict("Worker card missing".into()))?.as_str(),owns_key:true},&crate::db::sqlite::status_detail_with_reason("worker-turn-ended",&reason),now_ms()).await?;
                let area:String=sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1").bind(&task.track_id).fetch_one(&mut **tx).await?;
                events.push((ActorId::Kernel,EventScope::Card{card:row.card_id.clone().expect("checked Worker card"),track:task.track_id.clone().into(),area:area.into()},crate::decision_sink::WorkerTaskReport::Failed{attempt_id:task.id.clone(),reason:reason.clone()}.event(PlannerNotification::Suppress)));
                state.pending_report=Some(WorkerPendingReport{outcome:WorkerReportOutcome::Failed{reason:reason.clone()},commit_message:None});state.release_intent=true;
            } else {
                let events=release_ended_attempt_lease_for_card_tx(tx,row.card_id.as_ref().ok_or_else(||CalmError::Conflict("Worker card missing".into()))?.as_str(),EndedAttempt::Exactly(&task.id),ReleaseDelivery::CommitAsTaskEnded).await?;
                // Native settlement can release before the scheduler's retry. Retire only
                // this attempt's marker in the same transaction, even if its lease is gone.
                sqlx::query(
                    "UPDATE worker_sessions SET handle_state_json = json_remove(handle_state_json,
                        '$.timeout_cleanup'), updated_at_ms = ?3
                     WHERE id = ?1 AND json_extract(handle_state_json, '$.timeout_cleanup.task_id') = ?2",
                )
                .bind(&session)
                .bind(&task.id)
                .bind(now_ms())
                .execute(&mut **tx)
                .await?;
                return Ok(((),events));
            }
        }
        events.extend(finalize_tx(tx,&session,&mut state).await?);
        sqlx::query("UPDATE worker_sessions SET handle_state_json=?2,mcp_token_hash=NULL,state='failed',active_turn_id=NULL WHERE id=?1").bind(&session).bind(serde_json::to_string(&state)?).execute(&mut **tx).await?;
        Ok(((),events))
    })).await.map(|_:((),Vec<i64>)|())
}

pub(crate) async fn recover_after_boot(repo: &dyn Repo, confirmed: bool) -> Result<()> {
    // Startup's dispatcher replays these durable events after the listener starts.
    write_worker_events(repo,None,&crate::event::EventBus::new(),&write_context(),move|tx|Box::pin(async move {
        let rows:Vec<(String,String)>=sqlx::query_as("SELECT id,handle_state_json FROM worker_sessions WHERE json_extract(handle_state_json,'$.requested_selection.provider') IS NOT NULL").fetch_all(&mut **tx).await?;
        let mut events=vec![];
        for (session,json) in rows {
            let mut state:AcpWorkerState=serde_json::from_str(&json)?;let was_unsettled=!state.cleanup_confirmed;state.recover();
            if confirmed {state.cleanup_confirmed=true;}
            if confirmed && state.goal_submitted && state.pending_report.is_none()
                && let Some(task)=crate::db::sqlite::task_get_tx(tx,&state.attempt_id).await?
                && matches!(task.status,TaskStatus::Dispatched|TaskStatus::Running) {
                let card=task.worker_card_id.as_deref().ok_or_else(||CalmError::Conflict("Recovered ACP Worker has no card".into()))?;
                let reason="ACP Worker native round was interrupted by kernel restart; business goal is not replayed";
                crate::db::sqlite::task_fail_from_worker_tx(tx,&task.id,&task.track_id,crate::db::sqlite::TaskReporter::Card{card_id:card,owns_key:true},&crate::db::sqlite::status_detail_with_reason("worker-session-unknown",reason),now_ms()).await?;
                state.pending_report=Some(WorkerPendingReport{outcome:WorkerReportOutcome::Failed{reason:reason.into()},commit_message:None});state.release_intent=true;
                let area:String=sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1").bind(&task.track_id).fetch_one(&mut **tx).await?;
                events.push((ActorId::Kernel,EventScope::Card{card:card.into(),track:task.track_id.into(),area:area.into()},crate::decision_sink::WorkerTaskReport::Failed{attempt_id:task.id,reason:reason.into()}.event(PlannerNotification::Suppress)));
            }
            events.extend(finalize_tx(tx,&session,&mut state).await?);
            sqlx::query("UPDATE worker_sessions SET handle_state_json=?2,mcp_token_hash=NULL,active_turn_id=CASE WHEN ?3 THEN NULL ELSE active_turn_id END,state=CASE WHEN ?3 AND ?5 THEN 'failed' ELSE state END WHERE id=?1 AND handle_state_json=?4").bind(&session).bind(serde_json::to_string(&state)?).bind(confirmed).bind(json).bind(state.goal_submitted && was_unsettled).execute(&mut **tx).await?;
            if confirmed && let Some(claim)=claims::load(&mut **tx,&session).await?
                && matches!(claim.record().purpose,claims::Purpose::WorkerTurn{..}) {
                let _=claims::clear_on_completion(tx,&session,&claim).await?;
            }
        }
        Ok(((),events))
    })).await.map(|_:((),Vec<i64>)|())
}

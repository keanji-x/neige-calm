//! Executor/Validator policy over the shared ACP attachment and serial native round driver.
use super::{
    CancelRequest, Control,
    admission::Admission,
    lifecycle::{self, WorkerEnd},
    permission::WorkerPermissions,
};
use crate::{
    acp_session::host::AcpHost,
    acp_session::server,
    db::write_in_tx_typed,
    error::{CalmError, Result},
    operation::SpawnCtx,
    provider_execution::{CompletionEvidence, KnownEnd, Purpose},
};
use calm_truth::db::sqlite::provider_execution as claims;
use calm_types::acp_worker::WorkerTurnPhase;
use provider::acp::{
    permission::PermissionHandler,
    protocol,
    translate::{TurnContext, TurnTranslator},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

use super::attachment::{Attachment, attach};

pub(crate) async fn start(
    host: Arc<AcpHost>,
    workers: Arc<super::Workers>,
    mut admission: Admission,
    prompt: String,
    ctx: SpawnCtx,
) -> Result<()> {
    let state = admission.check(false).await?;
    if state.goal_submitted
        || matches!(
            state.turn_phase,
            WorkerTurnPhase::Submitting | WorkerTurnPhase::Unknown
        )
    {
        return Err(CalmError::Conflict(
            "ACP Worker submission outcome is unknown; goal replay refused".into(),
        ));
    }
    let attachment = attach(&host, &mut admission, &ctx).await?;
    admission.native = Some(attachment.native.clone());
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("acp-worker-before-initial-round", &admission.track).await;
    let (ready, started) = oneshot::channel();
    tokio::spawn(async move {
        let cleanup = attachment.cleanup.clone();
        let native = attachment.native.clone();
        let provider = attachment.provider;
        if let Err(error) = drive(
            host.clone(),
            workers.clone(),
            admission.clone(),
            attachment,
            prompt,
            ctx.clone(),
            ready,
        )
        .await
        {
            admission.authorized.store(false, Ordering::Release);
            let _ = admission.revoke().await;
            let clean = server::detach_worker_with_witness(
                &host,
                &admission.worker,
                provider,
                Some((&native, &cleanup)),
            )
            .await
            .is_ok()
                && cleanup.confirmed()
                && (cleanup.generation.stopped.load(Ordering::Acquire)
                    || workers
                        .get(&admission.worker)
                        .is_some_and(|control| control.round_ended.load(Ordering::Acquire))
                    || admission.check(true).await.is_ok_and(|state| {
                        matches!(
                            state.turn_phase,
                            WorkerTurnPhase::Prepared | WorkerTurnPhase::Settled
                        )
                    }));
            if clean {
                let captured = workers
                    .get(&admission.worker)
                    .filter(|control| Arc::ptr_eq(&control.authorized, &admission.authorized));
                let worker = admission.worker.clone();
                let claimed = write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move { Ok(claims::load(&mut **tx, &worker).await?) })
                })
                .await;
                if let Ok(Some(claim)) = &claimed
                    && let Some(control) = captured
                    && let Purpose::WorkerTurn {
                        turn_id,
                        attempt_id,
                        native_session_id,
                    } = &claim.record().purpose
                    && turn_id == &control.turn
                    && attempt_id == &admission.attempt
                    && native_session_id == &native
                {
                    let owner = control.admission.clone();
                    let repaired = async {
                        lifecycle::round_action(&ctx, &owner, false).await?;
                        host.provider_execution
                            .owner
                            .complete(
                                &owner.worker,
                                claim.record(),
                                CompletionEvidence::Unconfirmed,
                                Some(KnownEnd::AcpWorkerTurn(Box::new(WorkerEnd {
                                    admission: owner.clone(),
                                    ctx: ctx.clone(),
                                    known: false,
                                    cleanup_confirmed: true,
                                }))),
                            )
                            .await?;
                        lifecycle::fail_and_finalize(
                            &ctx,
                            &owner.worker,
                            owner.credential.clone(),
                            "ACP Worker driver interrupted",
                        )
                        .await
                    }
                    .await;
                    if let Err(repair) = repaired {
                        tracing::error!(%repair,"ACP Worker owner completion remains fenced");
                    }
                }
                if matches!(claimed, Ok(None)) {
                    let owner = admission.clone();
                    let repaired = write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                        Box::pin(async move {
                            let state = owner.check_tx(tx, true).await?;
                            let active: Option<String> = sqlx::query_scalar(
                                "SELECT active_turn_id FROM worker_sessions WHERE id=?1",
                            )
                            .bind(&owner.worker)
                            .fetch_one(&mut **tx)
                            .await?;
                            if active.is_some()
                                || claims::load(&mut **tx, &owner.worker).await?.is_some()
                                || !matches!(
                                    state.turn_phase,
                                    WorkerTurnPhase::Prepared | WorkerTurnPhase::Settled
                                )
                            {
                                return Err(CalmError::Conflict(
                                    "ACP Worker gap cleanup lost ownership".into(),
                                ));
                            }
                            let mut next = state.clone();
                            next.cleanup_confirmed = true;
                            if !crate::db::sqlite::acp_worker_state_cas_tx(
                                tx,
                                &owner.worker,
                                &owner.attempt,
                                None,
                                &state,
                                &next,
                            )
                            .await?
                            {
                                return Err(CalmError::Conflict(
                                    "ACP Worker gap cleanup CAS lost ownership".into(),
                                ));
                            }
                            Ok(())
                        })
                    })
                    .await;
                    if repaired.is_ok() {
                        let _ = lifecycle::fail_and_finalize(
                            &ctx,
                            &admission.worker,
                            admission.credential.clone(),
                            "ACP Worker stopped between settled rounds",
                        )
                        .await;
                    }
                }
            }
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point("acp-worker-driver-error-settled", &admission.track)
                .await;
            tracing::error!(session=%admission.worker,%error,cleanup_confirmed=clean,"ACP Worker driver ended");
        }
    });
    tokio::time::timeout(Duration::from_secs(30), started)
        .await
        .map_err(|_| {
            CalmError::Conflict("ACP Worker startup did not reach a confirmed prompt write".into())
        })?
        .map_err(|_| CalmError::Conflict("ACP Worker startup ended before prompt handoff".into()))?
        .map_err(CalmError::Conflict)
}

fn wire_error(error: provider::acp::Error) -> CalmError {
    CalmError::Conflict(error.to_string())
}

async fn drive(
    host: Arc<AcpHost>,
    workers: Arc<super::Workers>,
    base: Admission,
    attachment: Attachment,
    prompt: String,
    ctx: SpawnCtx,
    ready: oneshot::Sender<std::result::Result<(), String>>,
) -> Result<()> {
    let mut first = true;
    let mut ready = Some(ready);
    loop {
        let mut admission = base.clone();
        admission.turn = Some(uuid::Uuid::new_v4().to_string());
        let owner = admission.clone();
        let initial = first;
        let prompt = prompt.clone();
        let (claim,text,notified,access)=write_in_tx_typed(admission.repo.as_ref(),move|tx|Box::pin(async move {
            let mut preparing=owner.clone();preparing.turn=None;
            let state=preparing.check_tx(tx,false).await?;
            if !matches!(state.turn_phase,WorkerTurnPhase::Prepared|WorkerTurnPhase::Settled) || initial && state.goal_submitted{return Err(CalmError::Conflict("ACP Worker business prompt already submitted".into()))}
            let task=crate::db::sqlite::task_get_tx(tx,&owner.attempt).await?.ok_or_else(||CalmError::Conflict("Worker task missing".into()))?;
            let messages=crate::db::sqlite::planner_messages_for_attempt_tx(tx,&owner.attempt).await?;
            let pending:Vec<_>=messages.into_iter().filter(|message|message.seq>state.notified_through_seq).collect();
            if !initial && pending.is_empty(){return Err(CalmError::Conflict("ACP Worker has no queued notification".into()))}
            let mut text=if initial {prompt}else{String::new()};let mut notified=state.notified_through_seq;
            for message in pending {notified=notified.max(message.seq);text.push_str(&format!("\n\n{}",crate::terminal_interaction::message_header_from(&owner.attempt,&message.message_id,message.source.unwrap_or_default())));}
            let mut next=state.clone();next.turn_phase=WorkerTurnPhase::Submitting;next.goal_submitted=true;next.turn_index=next.turn_index.checked_add(1).ok_or_else(||CalmError::Conflict("ACP Worker turn counter exhausted".into()))?;
            if !crate::db::sqlite::acp_worker_state_cas_tx(tx,&owner.worker,&owner.attempt,None,&state,&next).await?{return Err(CalmError::Conflict("ACP Worker round admission lost ownership".into()))}
            let turn=owner.turn.as_deref().expect("Worker turn");
            let claims::Cas::Applied(claim)=claims::admit(tx,&owner.worker,turn.into(),Purpose::WorkerTurn{attempt_id:owner.attempt.clone(),turn_id:turn.into(),native_session_id:owner.native.clone().expect("Worker native")},claims::InitialRelease::NotRequired).await? else{return Err(CalmError::Conflict("ACP Worker already has a retained execution".into()))};
            sqlx::query("UPDATE worker_sessions SET active_turn_id=?2,state='running' WHERE id=?1 AND active_turn_id IS NULL").bind(&owner.worker).bind(turn).execute(&mut **tx).await?;
            Ok((claim,text,notified,task.access))
        })).await?;
        first = false;
        let (cancel, mut cancelled) = watch::channel(CancelRequest::Running);
        let (settled_tx, settled) = watch::channel(false);
        let control = Arc::new(Control {
            turn: admission.turn.clone().expect("turn"),
            cancel: cancel.clone(),
            settled,
            authorized: admission.authorized.clone(),
            admission: admission.clone(),
            ctx: ctx.clone(),
            native: attachment.native.clone(),
            provider: attachment.provider,
            cleanup: attachment.cleanup.clone(),
            round_ended: Arc::new(AtomicBool::new(false)),
        });
        {
            let mut controls = workers.0.lock().expect("ACP Workers");
            if controls
                .insert(admission.worker.clone(), control.clone())
                .is_some()
            {
                return Err(CalmError::Conflict(
                    "ACP Worker driver overlaps another round".into(),
                ));
            }
        }
        let mut incoming = attachment
            .client
            .route_session(&attachment.native)
            .map_err(wire_error)?;
        let mut overflow = incoming.overflow();
        let permissions = WorkerPermissions::new(
            attachment.client.clone(),
            access,
            admission.cwd.clone(),
            admission.clone(),
        );
        let mut translator = TurnTranslator::new(TurnContext {
            thread_id: admission.worker.clone(),
            turn_id: admission.turn.clone().expect("turn"),
            native_session_id: attachment.native.clone(),
            usage: attachment.usage.clone(),
            background_tool_completions: attachment.background_tool_completions,
        });
        let (flow_tx, flow_rx) = mpsc::channel(512);
        let flow = tokio::spawn(super::flow::capture(
            ctx.clone(),
            admission.clone(),
            flow_rx,
        ));
        let emit = |event| {
            if flow_tx.try_send(event).is_err() {
                admission.authorized.store(false, Ordering::Release);
                let _ = cancel.send(CancelRequest::Requested);
            }
        };
        emit(translator.started());
        let (pending_tx, pending_rx) = oneshot::channel();
        let submitted = protocol::prompt(
            &attachment.client,
            &attachment.capabilities,
            &attachment.native,
            &[protocol::ContentBlock::text(text)],
            &admission.gate(false),
        )
        .await;
        if submitted.is_ok() {
            let owner = admission.clone();
            write_in_tx_typed(admission.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    let state = owner.check_tx(tx, true).await?;
                    let mut next = state.clone();
                    next.notified_through_seq = next.notified_through_seq.max(notified);
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
                            "ACP Worker notification checkpoint lost ownership".into(),
                        ));
                    }
                    Ok(())
                })
            })
            .await?;
        }
        if let Some(ready) = ready.take() {
            let _ = ready.send(if submitted.is_ok() {
                Ok(())
            } else {
                Err("ACP Worker initial prompt write failed".into())
            });
        }
        let _ = pending_tx.send(submitted);
        let mut retained = vec![];
        let outcome = {
            let cancel_gate = admission.gate(true);
            let collected = crate::acp_session::driver::collect_turn(
                crate::acp_session::driver::NativeTurnIdentity {
                    native: &attachment.native,
                },
                &emit,
                &cancel_gate,
                incoming.receiver_mut(),
                &attachment.commands,
                &mut translator,
                pending_rx,
                &mut cancelled,
                &permissions,
                &mut retained,
            );
            // Keep the protocol and its authority watcher polled concurrently. A protocol
            // permission gate may hold the sole writer connection across an SQL await;
            // awaiting another admission inside a winning timer branch would stop polling it.
            let ownership = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    if admission.check(false).await.is_err() {
                        admission.authorized.store(false, Ordering::Release);
                        let _ = cancel.send(CancelRequest::Requested);
                        std::future::pending::<()>().await;
                    }
                }
            };
            tokio::pin!(collected, ownership);
            tokio::select! {
                result=&mut collected=>result,
                _=&mut ownership=>unreachable!("authority watcher only fences"),
                _=overflow.changed()=>Err(CalmError::Conflict("ACP Worker activity queue overflowed".into())),
            }
        };
        permissions.close().await;
        if let Ok(events) = &outcome {
            for event in events {
                emit(event.clone());
            }
        }
        drop(flow_tx);
        let captured = flow
            .await
            .map_err(|error| CalmError::Conflict(error.to_string()))?;
        let known = outcome.is_ok() && captured.is_ok();
        control.round_ended.store(known, Ordering::Release);
        #[cfg(feature = "fixtures")]
        if known {
            crate::test_seams::pause_point("acp-worker-round-ended", &admission.track).await;
        }
        if !known {
            attachment.client.retire_shared_when_idle();
        }
        let continue_round = lifecycle::round_action(&ctx, &admission, known).await?;
        let cleanup = if continue_round {
            false
        } else {
            admission.authorized.store(false, Ordering::Release);
            server::detach_worker_with_witness(
                &host,
                &admission.worker,
                attachment.provider,
                Some((&attachment.native, &attachment.cleanup)),
            )
            .await
            .is_ok()
                && attachment.cleanup.confirmed()
                && (known
                    || attachment
                        .cleanup
                        .generation
                        .stopped
                        .load(Ordering::Acquire))
        };
        host.provider_execution
            .owner
            .complete(
                &admission.worker,
                claim.record(),
                CompletionEvidence::Unconfirmed,
                Some(KnownEnd::AcpWorkerTurn(Box::new(WorkerEnd {
                    admission: admission.clone(),
                    ctx: ctx.clone(),
                    known,
                    cleanup_confirmed: cleanup,
                }))),
            )
            .await?;
        if cleanup {
            let _ = settled_tx.send(true);
        }
        if cleanup || continue_round {
            let mut controls = workers.0.lock().expect("ACP Workers");
            if controls
                .get(&admission.worker)
                .is_some_and(|control| Some(&control.turn) == admission.turn.as_ref())
            {
                controls.remove(&admission.worker);
            }
        }
        if continue_round {
            continue;
        }
        if cleanup {
            lifecycle::fail_and_finalize(
                &ctx,
                &admission.worker,
                admission.credential.clone(),
                if known {
                    "ACP Worker turn ended without a task report or queued Planner notification"
                } else {
                    "ACP Worker turn outcome is unknown"
                },
            )
            .await?;
            return Ok(());
        }
        return Err(CalmError::Conflict(
            "ACP Worker cleanup is unconfirmed; retaining lease and execution fence".into(),
        ));
    }
}

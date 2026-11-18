//! One autonomous turn; native completion without a task report is an immediate failure.
use super::*;
use crate::harness::backend::worker_turn::WorkerTurn;
use crate::harness::live_replies::LiveReplyWriter;
use crate::session_projection_repo::{ThreadAttribution, WorkerSessionState};
use provider::{
    InputItem,
    control::{CorrelationKey, InputRequest, TurnStartOutcome},
    events::{ItemPhase, PlannerEventKind},
};

struct WorkerTurnRunner {
    adapter: AgentWorkerAdapter,
    ctx: SpawnCtx,
    attempt: String,
    card: String,
    worker: String,
    track: String,
    thread: String,
    provider: AgentProvider,
    turn: WorkerTurn,
    live: LiveReplyWriter,
}

pub(super) async fn spawn(
    adapter: &AgentWorkerAdapter,
    output: &TxOutput,
    op: &Operation,
    ctx: &SpawnCtx,
) -> Result<()> {
    if adapter
        .harness
        .has_task_worker(&output.output_string("worker_session_id", "agent-worker")?)
    {
        return Ok(());
    }
    let track = output.output_string("track_id", "agent-worker")?;
    let fence = crate::provider_execution::TrackFenceHandoff::new(
        crate::harness::run_loop::issuance::TrackAdmissionFence::try_for_worker(
            &track,
            &ctx.track_delete_locks,
        )?,
    );
    let p: Payload = serde_json::from_value(op.payload.clone())?;
    super::super::workspace_lease::worker::verify_recorded_head(output, "agent-worker")?;
    super::super::workspace_lease::worker::verify_worker_checkout(output, "agent-worker")?;
    super::super::admit_task_side_effect(ctx.repo.as_ref(), &p.idempotency_key).await?;
    let saved = ctx
        .repo
        .session_projection_by_id(&output.output_string("worker_session_id", "agent-worker")?)
        .await?;
    if saved.is_some_and(|r| r.thread_id.is_some() || r.session_id.is_some()) {
        return Err(CalmError::Conflict(
            "agent worker native execution cannot be replayed after restart".into(),
        ));
    }
    let card = output.output_string("card_id", "agent-worker")?;
    let worker = output.output_string("worker_session_id", "agent-worker")?;
    let track = output.output_string("track_id", "agent-worker")?;
    let cwd = output.output_string("cwd", "agent-worker")?;
    let prompt = output.output_string("prompt", "agent-worker")?;
    let provider: AgentProvider = serde_json::from_value(output.data["provider"].clone())?;
    let stored: StoredSelection = serde_json::from_value(output.data["selection"].clone())?;
    let selection = TurnModelSelection {
        model: stored.model,
        effort: stored.effort,
    };
    let instructions = crate::harness::instructions::profile_instructions(
        adapter.repo.as_ref(),
        &adapter.wiring.plugin,
        &track,
        &card,
        &PlannerBinding {
            profile: HarnessProfile::TaskWorker,
            provider: provider.clone(),
        },
    )
    .await?;
    let (turn, thread) = if provider == AgentProvider::Codex {
        let token = {
            let card = card.clone();
            let worker = worker.clone();
            write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    crate::mcp_server::wiring::mint_and_persist_card_token(tx, &card, &worker).await
                })
            })
            .await?
        };

        let daemon = adapter.backends.for_session(&worker).await?;
        let socket = adapter.socket.clone().ok_or_else(|| {
            CalmError::ServiceUnavailable("worker MCP listener unavailable".into())
        })?;
        let (turn, minted) = WorkerTurn::open_codex(
            daemon.clone(),
            &card,
            &track,
            &worker,
            cwd.clone(),
            instructions,
            socket,
            token,
        )
        .await?;
        (turn, minted.thread_id)
    } else {
        let thread = new_id();
        let turn = WorkerTurn::open_acp(
            crate::acp_planner::session::SessionParams {
                engine: adapter.wiring.acp.execution.engine().cloned(),
                host: adapter.wiring.acp.clone(),
                provider: provider.clone(),
                repo: adapter.repo.clone(),
                seals: adapter.selection.codex.thread_seals().clone(),
                worker_session_id: worker.clone(),
                card_id: card.clone(),
                track_id: track.clone(),
                cwd: cwd.clone().into(),
                instructions,
            },
            thread.clone(),
        );
        (turn, thread)
    };
    let mut events = turn.subscribe_events();
    {
        let card = card.clone();
        let worker = worker.clone();
        let thread = thread.clone();
        let cwd = cwd.clone();
        let attempt = p.idempotency_key.clone();
        let cap = adapter.liveness.cap_ms();
        let attribution_provider = provider.clone();
        write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
            Box::pin(async move {
                crate::db::sqlite::session_bind_attribution_tx(
                    tx,
                    &worker,
                    ThreadAttribution {
                        worker_session_id: worker.clone(),
                        provider: attribution_provider,
                        thread_id: Some(thread),
                        session_id: None,
                        active_turn_id: None,
                    },
                )
                .await?;
                crate::harness::wiring::record_bound_session_cwd_tx(tx, &card, &worker, &cwd)
                    .await?;
                crate::scheduler::mark_acknowledged_running_tx(tx, &attempt, Some(&card), cap)
                    .await?;
                crate::db::sqlite::session_set_status_tx(tx, &worker, WorkerSessionState::Running)
                    .await?;
                Ok(())
            })
        })
        .await?;
    }
    let claim = adapter
        .harness
        .live_replies()
        .claim(&card.clone().into(), &worker);
    claim.install();
    let mut cancel = adapter.harness.install_task_worker(worker.clone())?;
    let runner = WorkerTurnRunner {
        adapter: adapter.clone(),
        ctx: ctx.clone(),
        attempt: p.idempotency_key,
        card,
        worker,
        track,
        thread,
        provider,
        turn,
        live: claim.writer(),
    };
    // The autonomous task input has no human queue entry.
    let segments = vec![crate::model::HarnessInputSegment {
        presentation: crate::model::HarnessInputPresentation::System,
        text: prompt.clone(),
        attachments: Vec::new(),
        origin: None,
    }];
    runner.store("item/completed",json!({"threadId":runner.thread,"_projection":true,"item":{
        "id":runner.attempt,"clientId":runner.attempt,"type":"userMessage","content":[{"type":"text","text":prompt}]
    }}),Some(serde_json::to_string(&segments)?)).await?;
    super::super::admit_task_side_effect(ctx.repo.as_ref(), &runner.attempt).await?;
    let start = runner
        .turn
        .start(
            InputRequest {
                items: vec![InputItem::text(prompt)],
                correlation: CorrelationKey(runner.attempt.clone()),
            },
            &selection,
            &fence,
        )
        .await;
    let turn_id = match start {
        Ok(TurnStartOutcome::Started { turn }) => turn.into_string(),
        outcome => {
            adapter.harness.remove_task_worker(&runner.worker);
            return Err(CalmError::ServiceUnavailable(format!(
                "worker turn did not start: {outcome:?}"
            )));
        }
    };
    runner.live.turn_started_native(&runner.thread, &turn_id);
    runner
        .snapshot(Some(&turn_id), crate::harness::HarnessPhaseTag::TurnRunning)
        .await?;
    tokio::spawn(async move {
        let mut cancel_requested = false;
        let result=async {
            loop {
                tokio::select! {
                    _=cancel.changed()=>{
                        cancel_requested=true;
                        break;
                    }
                    event=events.recv()=>{
                        let event=event.map_err(|e|CalmError::ServiceUnavailable(format!("worker event stream ended: {e}")))?;
                        if event.thread_id.as_deref()!=Some(&runner.thread) {continue;}
                        match event.kind {
                            PlannerEventKind::ReplyDelta {turn_id,item_id,delta}=>runner.live.delta(&turn_id,&item_id,&delta),
                            PlannerEventKind::Item {phase,params,..}=>{
                                runner.store(phase.method(),params.clone(),None).await?;
                                if let Some(id)=params.pointer("/item/id").and_then(Value::as_str) {
                                    if phase==ItemPhase::Completed {runner.live.item_stored(id);}
                                    else if params.pointer("/item/type").and_then(Value::as_str)==Some("agentMessage") {
                                        runner.live.reply_started(&turn_id,id,&params["item"]);
                                    }
                                }
                            }
                            PlannerEventKind::TurnCompleted {turn}=>{
                                runner.store("turn/completed",turn,None).await?;break;
                            }
                            PlannerEventKind::TurnAborted {..}=>break,
                            PlannerEventKind::ThreadSystemError=>return Err(CalmError::ServiceUnavailable("worker native thread failed".into())),
                            PlannerEventKind::PlanUpdated {params}=>{runner.store("turn/plan/updated",params,None).await?;}
                            _=>{}
                        }
                    }
                    _=tokio::time::sleep(std::time::Duration::from_millis(100))=>{
                        if !runner.turn.source_available() {return Err(CalmError::ServiceUnavailable("worker backend stopped".into()));}
                        if let Some(task)=runner.adapter.repo.task_get(&runner.attempt).await? && task.status==crate::model::TaskStatus::Canceled {
                            cancel_requested=true;break;
                        }
                    }
                }
            }
            Ok::<_,CalmError>(())
        }.await;
        let stop_reason = if cancel_requested {
            crate::provider_execution::StopReason::Interrupted
        } else if result.is_err() {
            crate::provider_execution::StopReason::OutputGap
        } else {
            crate::provider_execution::StopReason::Finished
        };
        if (result.is_err() || cancel_requested)
            && let provider::control::InterruptOutcome::Unconfirmed { error, .. } =
                runner.turn.interrupt(&turn_id).await
        {
            if runner.provider == AgentProvider::Codex {
                let stopped = async {
                    let runtime = runner
                        .ctx
                        .repo
                        .session_projection_by_id(&runner.worker)
                        .await?
                        .ok_or_else(|| CalmError::Conflict("worker identity disappeared".into()))?;
                    let daemon = runner.adapter.backends.for_session(&runner.worker).await?;
                    crate::harness::backend::factory::PlannerFactory::stop_timed_out_worker(
                        &runtime,
                        Some(daemon),
                    )
                    .await
                }
                .await;
                if let Err(error) = stopped {
                    tracing::error!(%error,worker=%runner.worker,"worker stop unconfirmed; retaining lease");
                    return;
                }
            } else {
                tracing::warn!(%error,worker=%runner.worker,"native cancel unconfirmed; stopping managed execution");
            }
        }
        if (result.is_err() || cancel_requested)
            && let Err(error) = runner.turn.join_interrupted().await
        {
            tracing::warn!(%error, worker=%runner.worker, "worker native cleanup did not join; stopping managed execution");
        }
        if let Err(error) = result {
            tracing::error!(%error,worker=%runner.worker,"worker event loop failed");
        }
        let host = &runner.adapter.wiring.acp;
        if let Err(error) = host
            .provider_execution
            .stop_session(
                &runner.ctx.operation_repo.sqlite_db(),
                host.execution.engine(),
                &runner.worker,
                stop_reason,
                std::time::Duration::from_secs(10),
            )
            .await
        {
            tracing::error!(%error,worker=%runner.worker,"worker execution could not stop; retaining lease");
            return;
        }
        for reply in runner.live.settle(&turn_id) {
            let mut item = reply.started;
            item["text"] = json!(reply.text);
            if let Err(error) = runner
                .store(
                    "item/completed",
                    json!({"threadId":runner.thread,"turnId":turn_id,"item":item,"_partial":true}),
                    None,
                )
                .await
            {
                tracing::error!(%error,"worker partial transcript write failed");
            } else {
                runner.live.item_stored(&reply.item_id);
            }
        }
        runner.live.discard(&turn_id);
        if let Err(error) = settlement::ended(
            &runner.adapter,
            &runner.ctx,
            &runner.attempt,
            &runner.card,
            &runner.worker,
        )
        .await
        {
            tracing::error!(%error,worker=%runner.worker,"worker settlement failed");
        }
        runner.adapter.harness.remove_task_worker(&runner.worker);
    });
    Ok(())
}
impl WorkerTurnRunner {
    async fn snapshot(
        &self,
        turn: Option<&str>,
        phase: crate::harness::HarnessPhaseTag,
    ) -> Result<()> {
        let thread = self.thread.clone();
        let worker = self.worker.clone();
        let turn = turn.map(str::to_owned);
        write_in_tx_typed(self.ctx.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let mut snapshot = crate::db::sqlite::session_handle_state_by_id_tx(tx, &worker)
                    .await?
                    .ok_or_else(|| CalmError::Conflict("worker snapshot is missing".into()))?;
                if crate::harness::HarnessSnapshot::parse_known(snapshot.clone()).is_none() {
                    return Err(CalmError::Conflict("worker snapshot is invalid".into()));
                }
                snapshot["phase"] = serde_json::to_value(phase)?;
                snapshot["last_thread_id"] = json!(thread);
                snapshot["last_turn_id"] = json!(turn);
                crate::db::sqlite::session_set_handle_state_tx(tx, &worker, Some(snapshot)).await?;
                crate::db::sqlite::session_set_active_turn_tx(
                    tx,
                    &worker,
                    if phase == crate::harness::HarnessPhaseTag::TurnRunning {
                        turn.as_deref()
                    } else {
                        None
                    },
                )
                .await?;
                Ok(())
            })
        })
        .await
    }
    async fn store(&self, method: &str, params: Value, segments: Option<String>) -> Result<()> {
        let card = self.card.clone();
        let worker = self.worker.clone();
        let thread = self.thread.clone();
        let method = method.to_owned();
        let track = self.track.clone();
        let roles = self.adapter.roles.clone();
        let areas = self.adapter.areas.clone();
        let id = params
            .pointer("/item/id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let kind = params
            .pointer("/item/type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let turn = params
            .get("turnId")
            .or_else(|| params.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let params_json = serde_json::to_string(&params)?;
        let mut projected_row = None;
        if segments.is_none()
            && calm_types::harness::is_native_user_message_type(kind.as_deref())
            && let Some(client) = params.pointer("/item/clientId").and_then(Value::as_str)
        {
            if method == ItemPhase::Started.method()
                && self
                    .ctx
                    .repo
                    .transcript_projection_id(&card, client)
                    .await?
                    .is_some()
            {
                return Ok(());
            }
            if method == ItemPhase::Completed.method()
                && let Some(id) = id.as_deref()
            {
                projected_row = self
                    .ctx
                    .repo
                    .transcript_admitted_projection_upgrade(
                        &card,
                        client,
                        turn.as_deref(),
                        id,
                        &params_json,
                    )
                    .await?;
            }
        }
        write_with_actor_events_typed(
            self.ctx.repo.as_ref(),
            None,
            &self.ctx.events,
            &crate::state::WriteContext::new(roles.clone(), areas.clone()),
            move |tx| {
                Box::pin(async move {
                    let row = match projected_row {
                        Some(row) => row,
                        None => {
                            let mut carrier =
                                crate::db::sqlite::conversation::current_native_fact_tx(
                                    tx, &card, &worker, true,
                                )
                                .await?;
                            crate::db::sqlite::record_native_item_tx(
                                &mut carrier,
                                &thread,
                                turn.as_deref(),
                                id.as_deref(),
                                kind.as_deref(),
                                &method,
                                &params_json,
                                segments.as_deref(),
                            )
                            .await?
                        }
                    };
                    Ok((
                        (),
                        vec![(
                            crate::ids::ActorId::Kernel,
                            crate::event::EventScope::Track {
                                track: track.clone().into(),
                                area: areas
                                    .area_of(&track.clone().into())
                                    .ok_or_else(|| CalmError::NotFound("worker area".into()))?,
                            },
                            crate::event::Event::HarnessItemAdded {
                                worker_session_id: worker,
                                card_id: card.into(),
                                track_id: track.into(),
                                item_db_id: row,
                                item_uuid: id,
                                item_type: kind,
                                turn_id: turn,
                                method,
                            },
                        )],
                    ))
                })
            },
        )
        .await
        .map(|_| ())
    }
}

impl Drop for WorkerTurnRunner {
    fn drop(&mut self) {
        self.adapter.harness.remove_task_worker(&self.worker);
        self.adapter
            .harness
            .live_replies()
            .retire_carrier(&self.card.clone().into(), &self.worker);
    }
}

//! The actual prompt writer owns cleanup and authoritative journal completion.
use super::*;

pub(super) struct DriveInput {
    pub(super) receipt: AcpSubmission,
    pub(super) canonical: bool,
}

pub(super) async fn drive(
    shared: Arc<Shared>,
    mut process: Process,
    input: DriveInput,
    mut translator: TurnTranslator,
    pending: std::result::Result<provider::acp::PendingResponse, provider::acp::Error>,
    mut cancelled: watch::Receiver<bool>,
    approvals: Approvals,
) -> DriverIdentity {
    let DriveInput { receipt, canonical } = input;
    let params = &shared.params;
    let mut retained = Vec::<Value>::new();
    let outcome=async {
        let pending=pending.map_err(wire_error)?;
        let response=pending.wait(Duration::from_secs(3600));tokio::pin!(response);
        let mut stop_at=None;
        loop {tokio::select! {
            result=&mut response=>{
                while let Ok(incoming)=process.connection.incoming.try_recv() {
                    match incoming {
                        Incoming::Notification{method,params:frame} if method=="session/update"=>{
                            for event in translator.update(&frame,crate::model::now_ms()).map_err(wire_error)? {retain_item(&mut retained,&event);let _=shared.events.send(event);}
                        },
                        Incoming::Request{..}=>return Err(CalmError::Conflict("ACP completed while a client request was still pending".into())),
                        _=>{},
                    }
                }
                let response=match result {
                    // The agent answered: the turn is over, so its outcome is known.
                    Err(provider::acp::Error::Remote{message,..})=>return Ok(translator.fail(&message,crate::model::now_ms())),
                    result=>result.map_err(wire_error)?,
                };
                let response:protocol::PromptResponse=protocol::decode(response).map_err(wire_error)?;
                return Ok::<_,CalmError>(translator.finish(response.stop_reason,crate::model::now_ms()));
            },
            changed=cancelled.changed(),if stop_at.is_none()=>{
                if changed.is_err() || *cancelled.borrow() {
                    stop_at=Some(tokio::time::Instant::now()+Duration::from_secs(5));
                    tokio::time::timeout(Duration::from_secs(3),async {
                        approvals.cancel(&receipt.native_session_id).await.map_err(wire_error)
                    }).await.map_err(|_|CalmError::Conflict("ACP cancellation writes timed out".into()))??;
                }
            },
            _=async{match stop_at{Some(at)=>tokio::time::sleep_until(at).await,None=>std::future::pending().await}}=>return Err(CalmError::Conflict("ACP cancellation outcome is unknown".into())),
            incoming=process.connection.incoming.recv()=>match incoming {
                Some(Incoming::Notification{method,params}) if method=="session/update"=>{
                    for event in translator.update(&params,crate::model::now_ms()).map_err(wire_error)? {retain_item(&mut retained,&event);let _=shared.events.send(event);}
                },
                Some(Incoming::Request{id,method,params})=>approvals.request(id,&method,params).await.map_err(wire_error)?,
                Some(Incoming::Notification{..})=>{},
                None=>return Err(CalmError::Conflict("ACP prompt connection closed".into())),
            }
        }}
    }.await;
    // Every frame of this process that will ever be read has been: its requests end here, before
    // any teardown await, so no answer reaches the agent after its turn.
    approvals.close().await;
    #[cfg(feature = "fixtures")]
    super::super::test_seams::wait_after_fence(&params.worker_session_id).await;
    drop(approvals);
    let revoked = revoke(params).await;
    if let Err(error) = &revoked {
        tracing::error!(%error,"ACP credential revocation failed");
    }
    let stopped = process.stop(&params.host, &params.worker_session_id).await;
    let (mut events, known) = match outcome {
        Ok(events) if stopped.is_ok() && revoked.is_ok() => (events, true),
        _ => (
            vec![PlannerEvent {
                thread_id: Some(receipt.thread_id.clone()),
                kind: PlannerEventKind::TurnCompleted {
                    turn: unknown_outcome(&receipt.turn_id),
                },
            }],
            false,
        ),
    };
    if canonical {
        for event in &mut events {
            if let PlannerEventKind::TurnCompleted { turn } = &mut event.kind {
                crate::harness::turn_outcome::canonical_diagnostic(turn);
            }
        }
    }
    let mut turn = events
        .iter()
        .find_map(|event| match &event.kind {
            PlannerEventKind::TurnCompleted { turn } => Some(turn.clone()),
            _ => None,
        })
        .expect("ACP terminal event");
    for event in &events {
        retain_item(&mut retained, event);
    }
    let mut durable = turn.clone();
    durable["items"] = Value::Array(retained);
    #[cfg(feature = "fixtures")]
    super::super::test_seams::wait_at_settlement(&params.worker_session_id).await;
    let settle = async {
        // The receipt owns settlement; projections can always be rebuilt from it.
        // Never publish success before its final frames are durable.
        acp_submission_finish(
            &params
                .repo
                .sqlite_pool()
                .ok_or_else(|| CalmError::Internal("ACP journal pool missing".into()))?,
            &receipt.worker_session_id,
            &receipt.client_id,
            known
                .then(|| serde_json::to_string(&durable))
                .transpose()?
                .as_deref(),
        )
        .await
    }
    .await;
    if let Err(error) = settle {
        tracing::error!(%error,"ACP settlement failed; durable submission remains fenced");
        turn = unknown_outcome(&receipt.turn_id);
        events = vec![PlannerEvent {
            thread_id: Some(receipt.thread_id.clone()),
            kind: PlannerEventKind::TurnCompleted { turn: turn.clone() },
        }];
    }
    if let Err(error) = crate::harness::turn_outcome::record(
        params.repo.as_ref(),
        &params.worker_session_id,
        &params.card_id,
        &params.track_id,
        &receipt.thread_id,
        &receipt.turn_id,
        &turn,
        crate::harness::turn_outcome::Confirmation::ProviderTerminal,
    )
    .await
    {
        tracing::error!(%error,"ACP terminal projection failed; receipt remains authoritative");
    }
    let mut state = shared.state.lock().expect("ACP state");
    state.active = None;
    for event in events {
        let _ = shared.events.send(event);
    }
    DriverIdentity::from_finished_receipt(&receipt)
}
fn retain_item(retained: &mut Vec<Value>, event: &PlannerEvent) {
    if let PlannerEventKind::Item { params, phase, .. } = &event.kind
        && let Some(id) = params["item"]["id"].as_str()
    {
        let frame = json!({"method":phase.method(),"params":params});
        if let Some(prior) = retained
            .iter_mut()
            .find(|frame| frame["params"]["item"]["id"] == id)
        {
            *prior = frame;
        } else {
            retained.push(frame);
        }
    }
}

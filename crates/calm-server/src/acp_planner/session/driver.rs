//! Prompt writer cleanup and durable settlement.
use super::*;

pub(super) async fn drive(
    shared: Arc<Shared>,
    mut process: Process,
    receipt: AcpSubmission,
    mut translator: TurnTranslator,
    pending: tokio::sync::oneshot::Receiver<
        std::result::Result<provider::acp::PendingResponse, provider::acp::WriteFailure>,
    >,
    mut cancelled: watch::Receiver<CancelRequest>,
    approvals: Arc<Approvals>,
) -> super::execution_end::Settlement {
    let params = &shared.params;
    let mut retained = Vec::<Value>::new();
    let outcome=async {
        let pending=pending.await.map_err(|_|CalmError::Conflict("ACP prompt handoff was cancelled".into()))?.map_err(|error| wire_error(error.into_error()))?;
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
                // A turn the agent ended itself after the turn declined one of its requests ends
                // with that reason, not as one the person interrupted.
                if matches!(response.stop_reason,protocol::StopReason::Cancelled) && stop_at.is_none()
                    && let Some(reason)=approvals.declined() {
                    return Ok(translator.fail(&reason,crate::model::now_ms()));
                }
                return Ok::<_,CalmError>(translator.finish(response.stop_reason,crate::model::now_ms()));
            },
            changed=cancelled.changed(),if stop_at.is_none()=>{
                if changed.is_err() || !matches!(*cancelled.borrow(), CancelRequest::Running) {
                    stop_at=Some(tokio::time::Instant::now()+Duration::from_secs(5));
                    if !matches!(*cancelled.borrow(), CancelRequest::Written) {
                    tokio::time::timeout(Duration::from_secs(3),async {
                        approvals.cancel(&receipt.native_session_id, shared.admission(&receipt.thread_id, None, Some(&receipt.native_session_id), Some(&receipt.turn_id)).gate()).await.map_err(wire_error)
                    }).await.map_err(|_|CalmError::Conflict("ACP cancellation writes timed out".into()))??;
                    }
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
    // What leaves the outcome unknown is kept here: the turn only says it is unknown.
    if let Err(error) = &outcome {
        tracing::warn!(%error, worker = %params.worker_session_id, turn = %receipt.turn_id,
            "ACP turn ended without a known outcome");
    }
    if let Err(error) = &stopped {
        tracing::warn!(%error, worker = %params.worker_session_id, turn = %receipt.turn_id,
            "ACP agent process stop failed; the turn's outcome is unknown");
    }
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
        let pool = params
            .repo
            .sqlite_db()
            .ok_or_else(|| CalmError::Internal("ACP journal pool missing".into()))?;
        // Serialize once: every persistence attempt must commit these exact bytes and items.
        let outcome_json = known.then(|| serde_json::to_string(&durable)).transpose()?;
        settle_receipt(&pool, &receipt, outcome_json.as_deref()).await
    }
    .await;
    let persisted = settle.is_ok();
    if let Err(error) = settle {
        #[cfg(feature = "fixtures")]
        super::super::test_seams::settlement_note(&params.worker_session_id, "failed");
        tracing::error!(%error,"ACP settlement failed; durable submission remains fenced");
        // Best effort checkpoint of the unknown fence. Spend at most one extra second:
        // if the writer is still unavailable, `sending` itself remains unresolved and
        // restart recovery marks it unknown. Never overwrite a completed CAS winner.
        if let Some(pool) = params.repo.sqlite_db() {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                acp_submission_finish(&pool, &receipt.worker_session_id, &receipt.client_id, None),
            )
            .await;
        }
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
    if known && persisted {
        super::execution_end::Settlement::Completed
    } else {
        super::execution_end::Settlement::Unconfirmed
    }
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

/// Persistence pending is not execution unknown. This function has no provider access.
async fn settle_receipt(
    pool: &crate::db::sqlite::SqliteDb,
    receipt: &AcpSubmission,
    outcome_json: Option<&str>,
) -> Result<()> {
    // Bound receipt-only persistence to 45s without holding the active session
    // indefinitely. Writer-pool admission, SQLite busy handling and backoff all
    // consume this same whole-future budget.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    // The active turn remains owned by drive until this returns. A crash loses the
    // in-memory known result; restart recovery fences the sending receipt as unknown.
    loop {
        // The receipt owns settlement; projections can always be rebuilt from it.
        // Never publish success before its final frames are durable.
        let result = tokio::time::timeout_at(
            deadline,
            acp_submission_finish(
                pool,
                &receipt.worker_session_id,
                &receipt.client_id,
                outcome_json,
            ),
        )
        .await;
        match result {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(calm_truth::TruthError::Db(ref error)))
                if crate::db::sqlite::is_sqlite_busy(error)
                    || matches!(error, sqlx::Error::PoolTimedOut) =>
            {
                #[cfg(feature = "fixtures")]
                super::super::test_seams::settlement_note(&receipt.worker_session_id, "retry");
                tracing::debug!(%error, "ACP receipt persistence pending; retrying settlement only");
                tokio::time::sleep_until(
                    deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
                )
                .await;
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => {
                return Err(CalmError::Conflict(
                    "ACP receipt settlement retry budget exhausted".into(),
                ));
            }
        }
    }
}

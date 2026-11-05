//! Shared maintenance retains one routed native command, never a chat receipt or process claim.
use super::*;
use crate::acp_planner::profile::CompactionWire;
use crate::acp_planner::session::compact::{CompactFailure, Correlation, before};
use crate::harness::backend::MaintenanceTarget;
use crate::harness::native_admission::NativePromptAdmission;

#[allow(clippy::too_many_arguments)]
pub(in crate::acp_planner::session) async fn compact(
    shared: &Arc<Shared>,
    registration: Registration<'_>,
    native: &str,
    wire: CompactionWire,
    target: MaintenanceTarget,
    selection: &provider::TurnModelSelection,
    admission: &NativePromptAdmission<'_>,
    track_fence: &crate::provider_execution::TrackFenceHandoff,
) -> std::result::Result<(), CompactFailure> {
    if wire == CompactionWire::XaiExtension {
        return Err(before(
            "Shared ACP does not support the maintenance extension",
        ));
    }
    let params = &shared.params;
    let permission = PlannerPermissionMode::Never;
    let mut owned = params.host.shared_server.lock().await;
    prepare::prepare(
        &mut owned,
        shared,
        registration,
        Some(native.to_owned()),
        &target.thread_id,
        selection,
        permission,
        admission,
    )
    .await
    .map_err(|error| CompactFailure::Before(error.into()))?;
    let server = owned.as_mut().expect("prepared shared ACP server");
    // The new/resumed session can publish its menu just after the setup result.
    // The same catalog consumes setup, routed and idle notifications.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !server.process.commands.declared(native) {
            let frame = server
                .process
                .connection
                .incoming
                .recv()
                .await
                .ok_or_else(|| before("ACP command catalog was not declared"))?;
            if let Incoming::Notification { params, .. } = &frame
                && server
                    .bindings
                    .values()
                    .any(|binding| Some(binding.native.as_str()) == params["sessionId"].as_str())
            {
                server.process.commands.observe(&frame);
            }
            if let Incoming::Request { id, method, .. } = frame {
                provider::acp::approvals::refuse(&server.process.connection.client, id, &method)
                    .await
                    .map_err(|error| CompactFailure::Before(wire_error(error)))?;
            }
        }
        Ok::<_, CompactFailure>(())
    })
    .await
    .map_err(|_| before("ACP command catalog timed out"))??;
    if !server.process.commands.contains(native, "compact") {
        return Err(before("ACP did not declare the compact command"));
    }
    let binding = server
        .bindings
        .get(&params.worker_session_id)
        .ok_or_else(|| before("Shared ACP maintenance binding is missing"))?;
    let cleanup = CleanupWitness {
        generation: server.generation.clone(),
        closed: binding.closed.clone(),
    };
    let client = server.process.connection.client.clone();
    let capabilities = server.process.capabilities.clone();
    let commands = server.process.commands.clone();
    let mut incoming = client
        .route_session(native)
        .map_err(|error| CompactFailure::Before(wire_error(error)))?;
    let turn = uuid::Uuid::new_v4().to_string();
    let (cancel, cancelled) = watch::channel(CancelRequest::Running);
    shared.state.lock().expect("ACP state").active = Some(Active {
        thread: target.thread_id.clone(),
        turn: turn.clone(),
        cancel,
        approvals: std::sync::Weak::new(),
        native_session_id: native.to_owned(),
        execution_id: None,
    });
    let (send, pending) = tokio::sync::oneshot::channel();
    let driver_client = client.clone();
    let task = shared.clone();
    let task_native = native.to_owned();
    let task_turn = turn.clone();
    let driver = tokio::spawn(async move {
        let outcome = collect(
            &driver_client,
            &commands,
            &mut incoming,
            &task_native,
            wire,
            pending,
            cancelled,
        )
        .await;
        // A routed overflow invalidates even a fast matching RPC result.
        let confirmed = matches!(outcome, Ok(true)) && !*incoming.overflow().borrow();
        if !confirmed {
            // Close/revoke only this exact attachment; no ordinary compact stops the server.
            let _ = detach_worker_with_witness(
                &task.params.host,
                &task.params.worker_session_id,
                Some((&task_native, &cleanup)),
            )
            .await;
        }
        drop(incoming);
        let mut state = task.state.lock().expect("ACP state");
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.turn == task_turn)
        {
            state.active = None;
        }
        if confirmed {
            execution_end::Settlement::Completed
        } else {
            execution_end::Settlement::Unconfirmed
        }
    });
    execution_end::install(
        shared,
        DriverSlot::maintenance(
            shared,
            driver,
            target.clone(),
            turn.clone(),
            native.to_owned(),
        ),
    );
    // Do not retain the global setup mutex during a card's command or its admission reads.
    drop(owned);
    let owner = shared.admission(
        &target.thread_id,
        Some(permission),
        Some(native),
        Some(&turn),
    );
    let combined = admission.combined(&owner);
    let result = protocol::prompt(
        &client,
        &capabilities,
        native,
        &[protocol::ContentBlock::text("/compact")],
        combined.gate(),
    )
    .await;
    let failure = match &result {
        Ok(_) => None,
        Err(provider::acp::WriteFailure::BeforeWrite(error)) => {
            Some(CompactFailure::Before(wire_error(error.clone())))
        }
        Err(provider::acp::WriteFailure::CrossedWrite(error)) => {
            Some(CompactFailure::Crossed(wire_error(error.clone())))
        }
    };
    let _ = send.send(result);
    track_fence.release();
    failure.map_or(Ok(()), Err)
}

type Pending = tokio::sync::oneshot::Receiver<
    std::result::Result<provider::acp::PendingResponse, provider::acp::WriteFailure>,
>;
async fn collect(
    client: &provider::acp::Client,
    commands: &process::Commands,
    incoming: &mut provider::acp::SessionIncoming,
    native: &str,
    wire: CompactionWire,
    pending: Pending,
    mut cancelled: watch::Receiver<CancelRequest>,
) -> Result<bool> {
    let pending = pending
        .await
        .map_err(|_| CalmError::Conflict("ACP compact handoff lost".into()))?
        .map_err(|error| wire_error(error.into_error()))?;
    let response = pending.wait(Duration::from_secs(3600));
    tokio::pin!(response);
    let mut correlation = Correlation::default();
    let mut overflow = incoming.overflow();
    loop {
        tokio::select! {
            biased;
            _ = async {
                if overflow.wait_for(|failed| *failed).await.is_err() {
                    std::future::pending::<()>().await;
                }
            } => return Ok(false),
            changed = cancelled.changed() => {
                if changed.is_err() || !matches!(*cancelled.borrow(), CancelRequest::Running) {
                    return Ok(false);
                }
            }
            result = &mut response => {
                while let Ok(frame) = incoming.try_recv() {
                    observe(commands, &mut correlation, &frame, native, wire)?;
                }
                return correlation.confirmed(wire, result.map_err(wire_error)?);
            }
            frame = incoming.recv() => {
                let Some(frame) = frame else { return Ok(false); };
                if let Incoming::Request { id, method, .. } = &frame {
                    provider::acp::approvals::refuse(client, id.clone(), method).await.map_err(wire_error)?;
                    return Ok(false);
                }
                observe(commands, &mut correlation, &frame, native, wire)?;
            }
        }
    }
}
fn observe(
    commands: &process::Commands,
    correlation: &mut Correlation,
    frame: &Incoming,
    native: &str,
    wire: CompactionWire,
) -> Result<()> {
    commands.observe_for(native, frame);
    match frame {
        Incoming::Notification { method, params } => {
            correlation.observe(wire, method, params, native)
        }
        Incoming::Request { .. } => Err(CalmError::Conflict(
            "ACP compact ended with a pending client request".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn shared_compact_overflow_rejects_even_a_matching_end_turn() {
        let (kernel, peer) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(kernel);
        let connection = provider::acp::Connection::new_shared(read, write);
        let client = connection.client.clone();
        let mut incoming = client.route_session("native").unwrap();
        let pending = protocol::prompt(
            &client,
            &Default::default(),
            "native",
            &[protocol::ContentBlock::text("/compact")],
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await
        .unwrap();
        let writer = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            for _ in 0..300 {
                write.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"native\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"x\"}}}}\n").await.unwrap();
            }
            write.write_all(format!("{}\n", json!({"jsonrpc":"2.0", "id":request["id"],"result":{"stopReason":"end_turn"}})).as_bytes()).await.unwrap();
            // Keep the real connection alive until the assertion finishes.
            std::future::pending::<()>().await;
        });
        tokio::time::timeout(
            Duration::from_secs(5),
            incoming.overflow().wait_for(|overflow| *overflow),
        )
        .await
        .unwrap()
        .unwrap();
        let (send, pending_rx) = tokio::sync::oneshot::channel();
        assert!(send.send(Ok(pending)).is_ok());
        let (_cancel, cancelled) = watch::channel(CancelRequest::Running);
        assert!(
            !collect(
                &client,
                &process::Commands::default(),
                &mut incoming,
                "native",
                CompactionWire::CommandPrompt,
                pending_rx,
                cancelled
            )
            .await
            .unwrap()
        );
        writer.abort();
    }
}

//! One ACP process per shared backend. Native sessions and turn receipts remain card-owned.
use super::super::{config::Registration, process};
use super::dispatch::{self, Carrier, Driver, PreparedTurn, TurnInput};
use super::*;
use provider::acp::configuration::Configuration;

mod maintenance;
mod prepare;
pub(super) use maintenance::compact;

pub(crate) use crate::acp_session::server::detach_worker;
use crate::acp_session::server::*;

pub(super) struct Turn {
    pub client: provider::acp::Client,
    pub capabilities: protocol::AgentCapabilities,
    commands: process::Commands,
    pub restart_owner: Option<crate::db::sqlite::AcpSharedProcessIdentity>,
    cleanup: CleanupWitness,
    snapshot: Option<super::super::context_snapshot::Selection>,
}

pub(super) async fn issue(
    shared: &Arc<Shared>,
    registration: Registration<'_>,
    original_native: Option<String>,
    selection: &provider::TurnModelSelection,
    input: TurnInput<'_, '_, '_>,
    presence: &mut ReceiptPresence,
    track_fence: &crate::provider_execution::TrackFenceHandoff,
) -> std::result::Result<ReceiptAttempt, StartFailure> {
    let params = &shared.params;
    let source = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    let source = input.contract.admission(&source);
    let crate::acp_session::attachment::Attached {
        mut owned,
        native,
        usage,
        ..
    } = prepare::prepare(
        shared,
        registration,
        original_native,
        input.thread,
        selection,
        input.permission,
        &source,
    )
    .await?;
    let server = owned.as_mut().expect("prepared shared ACP server");
    let tools = match registration.profile.mcp_tool_request {
        Some(_) => params.host.listed_tools(&params.worker_session_id).await?,
        None => Vec::new(),
    };
    let binding = server
        .bindings
        .get(&params.worker_session_id)
        .expect("attached ACP session");
    let asks = registration.profile.asks(&binding.mcp_key, tools);
    let cleanup = CleanupWitness {
        generation: server.generation.clone(),
        closed: binding.closed.clone(),
    };
    let client = server.process.connection.client.clone();
    if client.shared_retiring() {
        return Err(refused(
            "Shared ACP server is waiting for active sessions before retirement",
        ));
    }
    let carrier = Carrier::Resident(Turn {
        client,
        capabilities: server.process.capabilities.clone(),
        commands: server.process.commands.clone(),
        restart_owner: server.process.restart_owner.clone(),
        cleanup,
        snapshot: super::super::context_snapshot::Selection::freeze(
            registration,
            server.process.version.as_ref(),
            &binding.configuration,
        ),
    });
    let prepared = PreparedTurn {
        native,
        turn: uuid::Uuid::new_v4().to_string(),
        usage,
        asks,
    };
    let result = dispatch::send(shared, input, prepared, carrier, presence).await?;
    track_fence.release();
    Ok(result)
}

pub(super) async fn drive(
    driver: Driver,
    turn: Turn,
    mut incoming: provider::acp::SessionIncoming,
) -> execution_end::Settlement {
    let Driver {
        shared,
        receipt,
        mut translator,
        pending,
        mut cancelled,
        approvals,
    } = driver;
    let Turn {
        commands,
        cleanup,
        snapshot,
        ..
    } = turn;
    let mut retained = Vec::new();
    let overflow = incoming.overflow();
    let mut watch_overflow = overflow.clone();
    let mut outcome = tokio::select! {
        biased;
        _ = async {
            if watch_overflow.wait_for(|failed| *failed).await.is_err() {
                std::future::pending::<()>().await;
            }
        } => Err(CalmError::Conflict("ACP session event queue overflowed".into())),
        outcome = driver::collect_turn(
            &shared, &receipt, incoming.receiver_mut(), &commands, &mut translator, pending,
            &mut cancelled, &approvals, &mut retained,
        ) => if *overflow.borrow() {
            Err(CalmError::Conflict("ACP session event queue overflowed".into()))
        } else {
            outcome
        },
    };
    // A completed native turn can no longer accept answers to its earlier permission requests.
    // Optional telemetry retains admission, never the approval window.
    approvals.close().await;
    let snapshot_cancelled = cancelled.clone();
    // Keep the native turn's active slot while sampling: the next input/maintenance cannot
    // overtake this reading. Maintenance never enters this driver and keeps its invalidation.
    if !translator.context_received()
        && let Some(selection) = snapshot
        && outcome.as_ref().is_ok_and(|events| {
            events.iter().any(|event| matches!(
            &event.kind, PlannerEventKind::TurnCompleted { turn } if turn["status"] == "completed"
        ))
        })
        && snapshot_admitted(&shared, &receipt, &cleanup, &cancelled)
        && let Ok(registration) = shared.params.host.registration(&shared.params.provider)
        && let Some((used, window)) = super::super::context_snapshot::read(
            registration,
            &shared.params.cwd,
            &receipt.native_session_id,
            &selection,
            &mut cancelled,
            &shared.params.host.context_queries,
            || snapshot_admitted(&shared, &receipt, &cleanup, &snapshot_cancelled),
        )
        .await
        && snapshot_admitted(&shared, &receipt, &cleanup, &cancelled)
    {
        outcome.as_mut().expect("known native outcome").insert(
            0,
            PlannerEvent {
                thread_id: Some(receipt.thread_id.clone()),
                kind: PlannerEventKind::TokenUsage {
                    params: serde_json::json!({
                        "turnId":receipt.turn_id,
                        "tokenUsage":{"last":{"totalTokens":used},"modelContextWindow":window}
                    }),
                },
            },
        );
    }
    let cleaned = if outcome.is_err() {
        // A lost terminal response may leave native work running. Evidence
        // belongs to this exact attachment, never to a sibling's cleanup.
        cleanup_unknown(&shared.params, &receipt, &cleanup).await
    } else {
        false
    };
    drop(incoming);
    settle(&shared, &receipt, retained, outcome, &cleanup, cleaned).await
}

fn snapshot_admitted(
    shared: &Shared,
    receipt: &AcpSubmission,
    cleanup: &CleanupWitness,
    cancelled: &watch::Receiver<CancelRequest>,
) -> bool {
    let state = shared.state.lock().expect("ACP state");
    !state.closed
        && !cleanup.confirmed()
        && matches!(*cancelled.borrow(), CancelRequest::Running)
        && !shared.params.seals.is_sealed(&receipt.thread_id)
        && state.active.as_ref().is_some_and(|active| {
            active.turn == receipt.turn_id
                && active.thread == receipt.thread_id
                && active.native_session_id == receipt.native_session_id
                && active.execution_id.is_none()
        })
}

async fn settle(
    shared: &Shared,
    receipt: &AcpSubmission,
    mut retained: Vec<Value>,
    outcome: Result<Vec<PlannerEvent>>,
    cleanup: &CleanupWitness,
    mut cleaned: bool,
) -> execution_end::Settlement {
    let mut events = outcome.unwrap_or_default();
    let terminal = events.iter().find_map(|event| match &event.kind {
        PlannerEventKind::TurnCompleted { turn } => Some(turn.clone()),
        _ => None,
    });
    let durable = terminal.map(|mut terminal| {
        for event in &events {
            driver::retain_item(&mut retained, event);
        }
        terminal["items"] = Value::Array(retained);
        terminal.to_string()
    });
    let persisted = if let Some(pool) = shared.params.repo.sqlite_db() {
        driver::settle_receipt(&pool, receipt, durable.as_deref())
            .await
            .is_ok()
    } else {
        false
    };
    let known = persisted && durable.is_some();
    if !known && !cleaned {
        // A failed terminal receipt write also needs cleanup before canonical
        // Keep can consume the exact original attempt.
        cleaned = cleanup_unknown(&shared.params, receipt, cleanup).await;
    }
    let params = &shared.params;
    let projection = events
        .iter()
        .find_map(|event| match &event.kind {
            PlannerEventKind::TurnCompleted { turn } if known => Some(turn.clone()),
            _ => None,
        })
        .unwrap_or_else(|| unknown_outcome(&receipt.turn_id));
    if let Err(error) = crate::harness::turn_outcome::record(
        params.repo.as_ref(),
        &params.worker_session_id,
        &params.card_id,
        &params.track_id,
        &receipt.thread_id,
        &receipt.turn_id,
        &projection,
        if known {
            crate::harness::turn_outcome::Confirmation::ProviderTerminal
        } else {
            crate::harness::turn_outcome::Confirmation::Unconfirmed
        },
    )
    .await
    {
        tracing::error!(%error, "Shared ACP outcome projection failed");
    }
    if !known {
        events = vec![PlannerEvent {
            thread_id: Some(receipt.thread_id.clone()),
            kind: PlannerEventKind::TurnCompleted {
                turn: unknown_outcome(&receipt.turn_id),
            },
        }];
    }
    let mut state = shared.state.lock().expect("ACP state");
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.turn == receipt.turn_id && active.execution_id.is_none())
    {
        state.active = None;
        for event in events {
            let _ = shared.events.send(event);
        }
    }
    if known {
        execution_end::Settlement::Completed
    } else if cleaned {
        execution_end::Settlement::CleanUnknown
    } else {
        execution_end::Settlement::Unconfirmed
    }
}

async fn cleanup_unknown(
    params: &SessionParams,
    receipt: &AcpSubmission,
    witness: &CleanupWitness,
) -> bool {
    match detach_worker_with_witness(
        &params.host,
        &params.worker_session_id,
        Some((&receipt.native_session_id, witness)),
    )
    .await
    {
        Ok(()) => witness.confirmed(),
        Err(error) => {
            tracing::error!(%error, "Shared ACP original attachment cleanup is unconfirmed");
            false
        }
    }
}

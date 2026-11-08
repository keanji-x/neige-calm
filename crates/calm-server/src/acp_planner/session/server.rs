//! One ACP process per shared backend. Native sessions and turn receipts remain card-owned.
use super::super::{config::Registration, process};
use super::dispatch::{self, Carrier, Driver, PreparedTurn, TurnInput};
use super::*;
use provider::acp::{configuration::Configuration, usage::UsageTracker};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

mod maintenance;
mod prepare;
pub(super) use maintenance::compact;

#[derive(Debug)]
struct Binding {
    native: String,
    configuration: Configuration,
    closing: bool,
    mcp_key: String,
    closed: Arc<AtomicBool>,
}

struct Generation {
    id: uuid::Uuid,
    /// Set only after revoking this generation's grants, confirming the ACP
    /// service process exited, and executing the existing process-group cleanup.
    /// This is not the execution owner's descendants-complete quiescence proof.
    stopped: AtomicBool,
}

struct CleanupWitness {
    generation: Arc<Generation>,
    /// One attachment instance; resuming the same native id gets a fresh witness.
    closed: Arc<AtomicBool>,
}
impl CleanupWitness {
    fn confirmed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.generation.stopped.load(Ordering::Acquire)
    }
}

pub(in crate::acp_planner) struct Server {
    process: process::Process,
    bindings: HashMap<String, Binding>,
    credential_owners: HashSet<String>,
    generation: Arc<Generation>,
    provider: AgentProvider,
}
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedAcpServer")
            .field("bindings", &self.bindings.len())
            .finish()
    }
}

struct AttachmentGuard(Option<provider::acp::Client>);
impl Drop for AttachmentGuard {
    fn drop(&mut self) {
        if let Some(client) = &self.0 {
            client.retire_shared_when_idle();
        }
    }
}

pub(super) struct Turn {
    pub client: provider::acp::Client,
    pub capabilities: protocol::AgentCapabilities,
    commands: process::Commands,
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
    let mut owned = params.host.shared_server.lock().await;
    let (native, usage) = prepare::prepare(
        &mut owned,
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

pub(super) async fn ensure_native_owner<'e>(
    executor: impl sqlx::SqliteExecutor<'e>,
    provider: &AgentProvider,
    worker: &str,
    native: &str,
) -> Result<()> {
    let collision: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions WHERE provider=?1 AND agent_session_id=?2 AND id<>?3)",
    )
    .bind(provider.wire_name())
    .bind(native)
    .bind(worker)
    .fetch_one(executor)
    .await?;
    if collision {
        return Err(CalmError::Conflict(
            "Shared ACP native session belongs to another worker".into(),
        ));
    }
    Ok(())
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

pub(crate) async fn detach_worker(host: &AcpPlannerHost, worker: &str) -> Result<()> {
    detach_worker_with_witness(host, worker, None).await
}

async fn detach_worker_with_witness(
    host: &AcpPlannerHost,
    worker: &str,
    expected: Option<(&str, &CleanupWitness)>,
) -> Result<()> {
    let mut owned = host.shared_server.lock().await;
    if expected.is_some_and(|(_, witness)| witness.confirmed()) {
        return Ok(());
    }
    let Some(server) = owned.as_mut() else {
        return if expected.is_none() {
            Ok(())
        } else {
            Err(CalmError::Conflict(
                "Original ACP generation stop is unconfirmed".into(),
            ))
        };
    };
    if let Some((native, witness)) = expected {
        let matches = server.generation.id == witness.generation.id
            && server.bindings.get(worker).is_some_and(|binding| {
                binding.native == native && Arc::ptr_eq(&binding.closed, &witness.closed)
            });
        if !matches {
            return Err(CalmError::Conflict(
                "Original ACP attachment changed before cleanup".into(),
            ));
        }
    }
    if server.process.connection.client.terminal_error().is_some() {
        // EOF is not process-stop evidence. Retire and confirm the original
        // process before any driver may claim the generation is quiescent.
        return retire(host, &mut owned).await;
    }
    if let Some(binding) = server.bindings.get_mut(worker) {
        binding.closing = true;
    }
    if server.credential_owners.contains(worker) {
        revoke_worker(host.completion.repo()?.as_ref(), worker).await?;
        server.credential_owners.remove(worker);
    }
    let Some(binding) = server.bindings.get(worker) else {
        return Ok(());
    };
    let gate = provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) }));
    if let Err(error) = setup_request(
        &mut server.process.connection,
        &server.process.commands,
        "session/close",
        protocol::encode_close_session(&binding.native),
        &gate,
    )
    .await
    {
        server.process.connection.client.retire_shared_when_idle();
        return Err(error.into());
    }
    // Credential revocation and the matching native close both succeeded.
    binding.closed.store(true, Ordering::Release);
    server.process.commands.forget(&binding.native);
    server.bindings.remove(worker);
    Ok(())
}

/// Drain unsolicited traffic while the setup owner is idle, and retire a failed generation
/// under the same lock that admits attachment and mints credentials.
async fn supervise(host: std::sync::Weak<AcpPlannerHost>, generation: uuid::Uuid) {
    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let Some(host) = host.upgrade() else { return };
        let Ok(mut owned) = host.shared_server.try_lock() else {
            continue;
        };
        let Some(server) = owned
            .as_mut()
            .filter(|server| server.generation.id == generation)
        else {
            return;
        };
        let orphaned: Vec<_> = server
            .credential_owners
            .iter()
            .filter(|worker| {
                server
                    .bindings
                    .get(*worker)
                    .is_none_or(|binding| binding.closing)
            })
            .cloned()
            .collect();
        for worker in orphaned {
            let revoked = match host.completion.repo() {
                Ok(repo) => revoke_worker(repo.as_ref(), &worker).await,
                Err(error) => Err(error),
            };
            if let Err(error) = revoked {
                tracing::error!(%error, "Shared ACP orphan credential revocation failed");
            } else {
                server.credential_owners.remove(&worker);
            }
        }
        if server.process.connection.client.terminal_error().is_some()
            || server.process.connection.client.can_recycle_shared()
        {
            if let Err(error) = retire(&host, &mut owned).await {
                tracing::error!(%error, "Shared ACP server retirement failed");
                continue;
            }
            return;
        }
        for _ in 0..64 {
            let Ok(message) = server.process.connection.incoming.try_recv() else {
                break;
            };
            if let Incoming::Notification { params, .. } = &message
                && server
                    .bindings
                    .values()
                    .any(|binding| Some(binding.native.as_str()) == params["sessionId"].as_str())
            {
                server.process.commands.observe(&message);
            }
            if let Incoming::Request { id, method, .. } = message
                && provider::acp::approvals::refuse(&server.process.connection.client, id, &method)
                    .await
                    .is_err()
            {
                server.process.connection.client.retire_shared_when_idle();
                break;
            }
        }
    }
}

async fn retire(host: &AcpPlannerHost, owned: &mut Option<Server>) -> Result<()> {
    if let Some(server) = owned.as_ref() {
        for worker in &server.credential_owners {
            revoke_worker(host.completion.repo()?.as_ref(), worker).await?;
        }
    }
    if let Some(server) = owned.take() {
        let generation = server.generation.clone();
        server.process.stop().await?;
        generation.stopped.store(true, Ordering::Release);
    }
    Ok(())
}

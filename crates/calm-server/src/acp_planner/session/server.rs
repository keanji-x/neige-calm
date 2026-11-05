//! One ACP process per shared backend. Native sessions and turn receipts remain card-owned.
use super::super::{config::Registration, process};
use super::*;
use provider::acp::{configuration::Configuration, usage::UsageTracker};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
struct Binding {
    native: String,
    configuration: Configuration,
    closing: bool,
    mcp_key: String,
}

pub(in crate::acp_planner) struct Server {
    process: process::Process,
    bindings: HashMap<String, Binding>,
    credential_owners: HashSet<String>,
    generation: uuid::Uuid,
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn issue(
    shared: &Arc<Shared>,
    registration: Registration<'_>,
    original_native: Option<String>,
    thread: &str,
    correlation: &str,
    input: &InputContract<'_, '_>,
    items: Vec<InputItem>,
    blocks: Vec<protocol::ContentBlock>,
    selection: provider::TurnModelSelection,
    permission: PlannerPermissionMode,
    held: HeldRequestSender,
    presence: &mut ReceiptPresence,
    track_fence: &crate::provider_execution::TrackFenceHandoff,
) -> std::result::Result<ReceiptAttempt, StartFailure> {
    let params = &shared.params;
    let pool = params
        .repo
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let mut server = params.host.shared_server.lock().await;
    if server.as_ref().is_some_and(|server| {
        server.process.connection.client.terminal_error().is_some()
            || server.process.connection.client.can_recycle_shared()
    }) {
        retire(&params.host, &mut server).await?;
    }
    if server.is_none() {
        let process = process::Process::spawn_control(
            &params.host,
            registration,
            registration.config.provider.wire_name(),
            &std::env::temp_dir(),
        )
        .await?;
        if process.capabilities.session_capabilities.close.is_none() {
            return Err(refused("Shared ACP requires session close"));
        }
        let generation = uuid::Uuid::new_v4();
        *server = Some(Server {
            process,
            bindings: HashMap::new(),
            credential_owners: HashSet::new(),
            generation,
            provider: registration.config.provider.clone(),
        });
        tokio::spawn(supervise(Arc::downgrade(&params.host), generation));
    }
    let server = server.as_mut().expect("shared ACP server installed");
    if server.provider != params.provider {
        return Err(refused(
            "The shared ACP host already owns a different provider",
        ));
    }
    if server.process.connection.client.shared_retiring() {
        return Err(refused(
            "Shared ACP server is waiting for active sessions before retirement",
        ));
    }
    let owner = shared.admission(thread, Some(permission), original_native.as_deref(), None);
    let admission = input.admission(&owner);
    let mut usage = UsageTracker::new(registration.profile.usage);
    let (native, mut configuration) = if let Some(binding) =
        server.bindings.get(&params.worker_session_id)
    {
        if binding.closing || original_native.as_deref() != Some(binding.native.as_str()) {
            return Err(refused("Shared ACP binding changed"));
        }
        (binding.native.clone(), binding.configuration.clone())
    } else {
        if let Some(native) = original_native.as_deref() {
            if server
                .bindings
                .values()
                .any(|binding| binding.native == native)
            {
                return Err(refused(
                    "Shared ACP native session is owned by another card",
                ));
            }
            ensure_native_owner(
                pool.read_pool(),
                &params.provider,
                &params.worker_session_id,
                native,
            )
            .await?;
        }
        server
            .credential_owners
            .insert(params.worker_session_id.clone());
        let mut attachment_guard = AttachmentGuard(Some(server.process.connection.client.clone()));
        let token = mint_token(params).await?;
        let mut attached_native = None;
        let key = format!(
            "{}-{}",
            registration.profile.mcp_server_key(),
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let attachment = async {
            let mcp = [params.mcp_server(key.clone(), &token)];
            let (method, request) = setup::attachment_request(
                &server.process.capabilities,
                &params.cwd,
                original_native.as_deref(),
                &mcp,
            )?;
            let response = setup_request(
                &mut server.process.connection,
                &mut server.process.commands,
                method,
                request,
                admission.gate(),
            )
            .await?;
            usage.configure(&response);
            let native = match original_native.as_deref() {
                Some(native) => native.to_owned(),
                None => {
                    let response: protocol::NewSessionResponse =
                        protocol::decode(response.clone()).map_err(wire_error)?;
                    if response.session_id.is_empty() {
                        return Err(refused("ACP agent returned an empty native identity"));
                    }
                    if server
                        .bindings
                        .values()
                        .any(|binding| binding.native == response.session_id)
                    {
                        return Err(refused(
                            "Shared ACP returned a session already owned by another card",
                        ));
                    }
                    ensure_native_owner(
                        pool.read_pool(),
                        &params.provider,
                        &params.worker_session_id,
                        &response.session_id,
                    )
                    .await?;
                    response.session_id
                }
            };
            attached_native = Some(native.clone());
            let configuration = Configuration::from_response(&response).map_err(wire_error)?;
            if original_native.is_none() {
                bind_native(params, thread, &native).await?;
            }
            Ok::<_, StartFailure>((native, configuration))
        }
        .await;
        let (native, configuration) = match attachment {
            Ok(attachment) => attachment,
            Err(error) => {
                if matches!(error, StartFailure::Failed(_)) {
                    // A lost setup reply can leave a native session attached
                    // whose identity never reached Neige. Drain this generation.
                    server.process.connection.client.retire_shared_when_idle();
                }
                let revoked = revoke(params).await;
                if revoked.is_ok() {
                    server.credential_owners.remove(&params.worker_session_id);
                }
                if let Some(native) = attached_native {
                    let gate =
                        provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) }));
                    if let Err(close) = setup_request(
                        &mut server.process.connection,
                        &mut server.process.commands,
                        "session/close",
                        protocol::encode_close_session(&native),
                        &gate,
                    )
                    .await
                    {
                        tracing::warn!(
                            ?close,
                            "Shared ACP attachment rollback could not close the native session"
                        );
                        server.bindings.insert(
                            params.worker_session_id.clone(),
                            Binding {
                                native,
                                configuration: Configuration::default(),
                                closing: true,
                                mcp_key: key,
                            },
                        );
                    }
                }
                revoked?;
                return Err(error);
            }
        };
        attachment_guard.0 = None;
        server.bindings.insert(
            params.worker_session_id.clone(),
            Binding {
                native: native.clone(),
                configuration: configuration.clone(),
                closing: false,
                mcp_key: key,
            },
        );
        (native, configuration)
    };
    let owner = shared.admission(thread, Some(permission), Some(&native), None);
    let admission = input.admission(&owner);
    let mut configuration_guard = AttachmentGuard(Some(server.process.connection.client.clone()));
    server
        .bindings
        .get_mut(&params.worker_session_id)
        .expect("attached ACP session")
        .closing = true;
    let configured = shared
        .configure_connection(
            &mut server.process.connection,
            &mut server.process.commands,
            &native,
            &mut configuration,
            &selection,
            &admission,
            &mut usage,
        )
        .await;
    server
        .bindings
        .get_mut(&params.worker_session_id)
        .expect("attached ACP session")
        .configuration = configuration;
    if matches!(configured, Err(StartFailure::Failed(_))) {
        revoke(params).await?;
    } else {
        server
            .bindings
            .get_mut(&params.worker_session_id)
            .expect("attached ACP session")
            .closing = false;
        configuration_guard.0 = None;
    }
    configured?;
    let tools = match registration.profile.mcp_tool_request {
        Some(_) => params.host.listed_tools(&params.worker_session_id).await?,
        None => Vec::new(),
    };
    let binding = server
        .bindings
        .get(&params.worker_session_id)
        .expect("attached ACP session");
    let asks = registration.profile.asks(&binding.mcp_key, tools);
    let client = server.process.connection.client.clone();
    if client.shared_retiring() {
        return Err(refused(
            "Shared ACP server is waiting for active sessions before retirement",
        ));
    }
    let capabilities = server.process.capabilities.clone();
    protocol::validate_prompt(&capabilities, &native, &blocks)
        .map_err(|error| refused(&error.to_string()))?;
    let mut incoming = client.route_session(&native).map_err(wire_error)?;
    let turn = uuid::Uuid::new_v4().to_string();
    let receipt = AcpSubmission {
        worker_session_id: params.worker_session_id.clone(),
        client_id: correlation.into(),
        turn_id: turn.clone(),
        thread_id: thread.into(),
        native_session_id: native.clone(),
        input_json: input.receipt(&items, &blocks)?,
        state: crate::db::sqlite::AcpSubmissionState::Sending,
        kept_cleanup: None,
        outcome_json: None,
    };
    let attempt = || ReceiptAttempt {
        turn: turn.clone(),
        native_session_id: native.clone(),
    };
    match acp_submission_prepare(&pool, &receipt, crate::model::now_ms()).await {
        Ok(()) => *presence = ReceiptPresence::Present(attempt()),
        Err(crate::db::sqlite::AcpPrepareError::BeforeReceipt(error)) => {
            return Err(CalmError::from(error).into());
        }
        Err(crate::db::sqlite::AcpPrepareError::CommitUncertain(error)) => {
            *presence = ReceiptPresence::Present(attempt());
            return Err(CalmError::from(error).into());
        }
    }
    let approvals = Arc::new(Approvals::for_turn(permission, &held, &turn, &client, asks));
    let (cancel, mut cancelled) = watch::channel(CancelRequest::Running);
    shared.state.lock().expect("ACP state").active = Some(Active {
        thread: thread.into(),
        turn: turn.clone(),
        cancel,
        approvals: Arc::downgrade(&approvals),
        native_session_id: native.clone(),
        execution_id: None,
    });
    let mut translator = TurnTranslator::new(TurnContext {
        thread_id: thread.into(),
        turn_id: turn.clone(),
        native_session_id: native.clone(),
        usage,
    });
    let _ = shared.events.send(translator.started());
    let (send_pending, pending) = tokio::sync::oneshot::channel();
    let task = Arc::clone(shared);
    let retained_receipt = receipt.clone();
    let driver = tokio::spawn(async move {
        let mut retained = Vec::new();
        let overflow = incoming.overflow();
        let mut watch_overflow = overflow.clone();
        let outcome = tokio::select! {
            biased;
            _ = async {
                if watch_overflow.wait_for(|failed| *failed).await.is_err() {
                    std::future::pending::<()>().await;
                }
            } => Err(CalmError::Conflict("ACP session event queue overflowed".into())),
            outcome = driver::collect_turn(
                &task, &receipt, incoming.receiver_mut(), &mut translator, pending,
                &mut cancelled, &approvals, &mut retained,
            ) => if *overflow.borrow() {
                Err(CalmError::Conflict("ACP session event queue overflowed".into()))
            } else {
                outcome
            },
        };
        approvals.close().await;
        if outcome.is_err() {
            // A lost terminal response leaves the agent able to act until its
            // credential is revoked and the native session is closed.
            if let Err(error) = revoke(&task.params).await {
                tracing::error!(%error, "Shared ACP unknown-turn credential revocation failed");
            }
            if let Err(error) =
                detach_worker(&task.params.host, &task.params.worker_session_id).await
            {
                tracing::error!(%error, "Shared ACP unknown-turn detach failed");
            }
        }
        drop(incoming);
        settle(&task, &receipt, retained, outcome).await
    });
    execution_end::install(
        shared,
        DriverSlot::new(shared, driver, retained_receipt, input.association()),
    );
    // Admission and receipt are installed before any prompt can reach the shared writer.
    let owner = shared.admission(thread, Some(permission), Some(&native), Some(&turn));
    let admission = input.admission(&owner);
    let pending =
        protocol::prompt(&client, &capabilities, &native, &blocks, admission.gate()).await;
    let _ = send_pending.send(pending);
    track_fence.release();
    Ok(attempt())
}

async fn settle(
    shared: &Shared,
    receipt: &AcpSubmission,
    mut retained: Vec<Value>,
    outcome: Result<Vec<PlannerEvent>>,
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

pub(crate) async fn detach_worker(host: &AcpPlannerHost, worker: &str) -> Result<()> {
    let mut server = host.shared_server.lock().await;
    let Some(server) = server.as_mut() else {
        return Ok(());
    };
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
    if server.process.connection.client.terminal_error().is_none() {
        let gate = provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) }));
        if let Err(error) = setup_request(
            &mut server.process.connection,
            &mut server.process.commands,
            "session/close",
            protocol::encode_close_session(&binding.native),
            &gate,
        )
        .await
        {
            server.process.connection.client.retire_shared_when_idle();
            return Err(error.into());
        }
    }
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
            .filter(|server| server.generation == generation)
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
        server.process.stop().await?;
    }
    Ok(())
}

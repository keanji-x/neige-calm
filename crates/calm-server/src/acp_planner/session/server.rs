//! One ACP process per shared backend. Native sessions and turn receipts remain card-owned.
use super::super::{config::Registration, process};
use super::*;
use provider::acp::{approvals::AgentAsks, configuration::Configuration, usage::UsageTracker};
use std::collections::HashMap;

#[derive(Debug)]
struct Binding {
    native: String,
    configuration: Configuration,
}

pub(in crate::acp_planner) struct Server {
    process: process::Process,
    bindings: HashMap<String, Binding>,
}
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedAcpServer")
            .field("bindings", &self.bindings.len())
            .finish()
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
    asks: AgentAsks,
    presence: &mut ReceiptPresence,
    track_fence: &crate::provider_execution::TrackFenceHandoff,
) -> std::result::Result<ReceiptAttempt, StartFailure> {
    let params = &shared.params;
    let pool = params
        .repo
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let mut server = params.host.shared_server.lock().await;
    if server
        .as_ref()
        .is_some_and(|server| server.process.connection.client.terminal_error().is_some())
        && let Some(dead) = server.take()
    {
        dead.process.stop().await?;
    }
    if server.is_none() {
        let process = process::Process::spawn_control(
            &params.host,
            registration,
            "shared-antigravity",
            &std::env::temp_dir(),
        )
        .await?;
        if process.capabilities.session_capabilities.close.is_none() {
            return Err(refused("Shared ACP requires session close"));
        }
        *server = Some(Server {
            process,
            bindings: HashMap::new(),
        });
    }
    let server = server.as_mut().expect("shared ACP server installed");
    let owner = shared.admission(thread, Some(permission), original_native.as_deref(), None);
    let admission = input.admission(&owner);
    let mut usage = UsageTracker::new(registration.profile.usage);
    let (native, mut configuration) =
        if let Some(binding) = server.bindings.get(&params.worker_session_id) {
            if original_native.as_deref() != Some(binding.native.as_str()) {
                return Err(refused("Shared ACP binding changed"));
            }
            (binding.native.clone(), binding.configuration.clone())
        } else {
            if original_native.as_ref().is_some_and(|native| {
                server
                    .bindings
                    .values()
                    .any(|binding| binding.native == *native)
            }) {
                return Err(refused(
                    "Shared ACP native session is owned by another card",
                ));
            }
            let token = mint_token(params).await?;
            let attachment = async {
                let key = format!(
                    "{}-{}",
                    registration.profile.mcp_server_key(),
                    &uuid::Uuid::new_v4().simple().to_string()[..12]
                );
                let mcp = [params.mcp_server(key, &token)];
                let (method, request) = match original_native.as_deref() {
                    Some(native)
                        if server
                            .process
                            .capabilities
                            .session_capabilities
                            .resume
                            .is_some() =>
                    {
                        (
                            "session/resume",
                            protocol::encode_resume_session(native, &params.cwd, &mcp),
                        )
                    }
                    Some(native) if server.process.capabilities.load_session => (
                        "session/load",
                        protocol::encode_load_session(native, &params.cwd, &mcp),
                    ),
                    Some(_) => return Err(refused("Shared ACP cannot restore the native session")),
                    None => (
                        "session/new",
                        protocol::encode_new_session(&params.cwd, &mcp),
                    ),
                };
                let response = setup_request(
                    &mut server.process.connection,
                    &mut server.process.commands,
                    method,
                    request,
                    admission.gate(),
                )
                .await?;
                usage.configure(&response);
                let configuration = Configuration::from_response(&response).map_err(wire_error)?;
                let native = match original_native.as_deref() {
                    Some(native) => native.to_owned(),
                    None => {
                        let response: protocol::NewSessionResponse =
                            protocol::decode(response).map_err(wire_error)?;
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
                        bind_native(params, thread, &response.session_id).await?;
                        response.session_id
                    }
                };
                Ok::<_, StartFailure>((native, configuration))
            }
            .await;
            let (native, configuration) = match attachment {
                Ok(attachment) => attachment,
                Err(error) => {
                    revoke(params).await?;
                    return Err(error);
                }
            };
            server.bindings.insert(
                params.worker_session_id.clone(),
                Binding {
                    native: native.clone(),
                    configuration: configuration.clone(),
                },
            );
            (native, configuration)
        };
    let owner = shared.admission(thread, Some(permission), Some(&native), None);
    let admission = input.admission(&owner);
    for (category, value) in [
        ("model", selection.model.as_deref()),
        ("thought_level", selection.effort.as_deref()),
    ] {
        let Some(value) = value else { continue };
        let option = configuration
            .category(category)
            .filter(|option| option.accepts(value))
            .ok_or_else(|| refused("The ACP agent did not declare that model or effort choice"))?;
        if option.current_value == value {
            continue;
        }
        let result = setup_request(
            &mut server.process.connection,
            &mut server.process.commands,
            "session/set_config_option",
            protocol::encode_set_config_option(&native, &option.id, value),
            admission.gate(),
        )
        .await?;
        usage.configure(&result);
        configuration = Configuration::from_response(&result).map_err(wire_error)?;
        if configuration
            .category(category)
            .is_none_or(|option| option.current_value != value)
        {
            return Err(refused("ACP did not confirm the selected model or effort"));
        }
    }
    server
        .bindings
        .get_mut(&params.worker_session_id)
        .expect("attached ACP session")
        .configuration = configuration.clone();
    params
        .host
        .record_configuration(&params.card_id, configuration);
    let client = server.process.connection.client.clone();
    let capabilities = server.process.capabilities.clone();
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
        let outcome = driver::collect_turn(
            &task,
            &receipt,
            incoming.receiver_mut(),
            &mut translator,
            pending,
            &mut cancelled,
            &approvals,
            &mut retained,
        )
        .await;
        approvals.close().await;
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

pub(crate) async fn detach_worker(host: &AcpPlannerHost, worker: &str) -> Result<()> {
    let mut server = host.shared_server.lock().await;
    let Some(server) = server.as_mut() else {
        return Ok(());
    };
    let Some(binding) = server.bindings.get(worker) else {
        return Ok(());
    };
    revoke_worker(host.completion.repo()?.as_ref(), worker).await?;
    if server.process.connection.client.terminal_error().is_none() {
        let gate = provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) }));
        setup_request(
            &mut server.process.connection,
            &mut server.process.commands,
            "session/close",
            json!({"sessionId": binding.native}),
            &gate,
        )
        .await?;
    }
    server.bindings.remove(worker);
    Ok(())
}

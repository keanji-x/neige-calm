//! Shared attachment and model configuration, independent of input or maintenance storage.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare(
    owned: &mut Option<Server>,
    shared: &Arc<Shared>,
    registration: Registration<'_>,
    original_native: Option<String>,
    thread: &str,
    selection: &provider::TurnModelSelection,
    permission: PlannerPermissionMode,
    source: &NativePromptAdmission<'_>,
) -> std::result::Result<(String, UsageTracker), StartFailure> {
    let params = &shared.params;
    let pool = params
        .repo
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    if owned.as_ref().is_some_and(|server| {
        server.process.connection.client.terminal_error().is_some()
            || server.process.connection.client.can_recycle_shared()
    }) {
        retire(&params.host, owned).await?;
    }
    if owned.is_none() {
        let process =
            process::Process::spawn_shared(&params.host, registration, &std::env::temp_dir())
                .await?;
        if process.capabilities.session_capabilities.close.is_none() {
            return Err(refused("Shared ACP requires session close"));
        }
        let generation = uuid::Uuid::new_v4();
        *owned = Some(Server {
            process,
            bindings: HashMap::new(),
            credential_owners: HashSet::new(),
            generation: Arc::new(Generation {
                id: generation,
                stopped: AtomicBool::new(false),
            }),
            provider: registration.config.provider.clone(),
        });
        tokio::spawn(supervise(Arc::downgrade(&params.host), generation));
    }
    let server = owned.as_mut().expect("shared ACP server installed");
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
    let admission = source.combined(&owner);
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
        // The durable worker owns this attachment across reconnects and shared-process
        // generations. Keep the suffix length (and therefore the shim's tool-name budget)
        // unchanged; credentials still rotate on every new attachment.
        let key = format!(
            "{}-{}",
            registration.profile.mcp_server_key(),
            &blake3::hash(params.worker_session_id.as_bytes()).to_hex()[..12]
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
                &server.process.commands,
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
                        &server.process.commands,
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
                                closed: Arc::new(AtomicBool::new(false)),
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
                closed: Arc::new(AtomicBool::new(false)),
            },
        );
        (native, configuration)
    };
    {
        let owner = shared.admission(thread, Some(permission), Some(&native), None);
        let admission = source.combined(&owner);
        let mut configuration_guard =
            AttachmentGuard(Some(server.process.connection.client.clone()));
        server
            .bindings
            .get_mut(&params.worker_session_id)
            .expect("attached ACP session")
            .closing = true;
        let configured = shared
            .configure_connection(
                &mut server.process.connection,
                &server.process.commands,
                &native,
                &mut configuration,
                selection,
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
        server.process.commands.retain_sessions(
            &server
                .bindings
                .values()
                .map(|binding| binding.native.as_str())
                .collect(),
        );
        configured?;
    }
    Ok((native, usage))
}

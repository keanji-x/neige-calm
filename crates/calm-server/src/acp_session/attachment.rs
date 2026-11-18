//! One shared attachment transaction: reserve, attach, configure, publish or roll back.
use super::{
    failure::SetupFailure,
    host::{AcpHost, Registration},
    process::setup_request,
    server::{self, AttachmentGuard, Binding, CleanupWitness, Server},
    setup,
};
use crate::error::{CalmError, Result};
use provider::{
    acp::{configuration::Configuration, protocol, usage::UsageTracker},
    send_boundary::{PreSendGate, SendRefusal},
};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::MutexGuard;

/// Role policy only: durable admission, credential issuance and role-owned publication.
/// The transport owns ordering, identity collision checks and all rollback writes.
#[async_trait::async_trait]
pub(crate) trait Role: Send + Sync {
    async fn check(&self, native: Option<&str>) -> Result<()>;
    async fn mint(&mut self) -> Result<String>;
    /// Retain this exact generation before the first attachment write, even without a native id.
    fn pending(&mut self, witness: CleanupWitness);
    async fn bind(&mut self, native: &str) -> Result<()>;
    fn observe(&self, configuration: &Configuration);
    async fn publish(&mut self, native: &str, configuration: &Configuration) -> Result<()>;
}

pub(crate) struct Request<'a> {
    pub worker: &'a str,
    pub track_id: &'a str,
    pub cwd: &'a Path,
    pub original_native: Option<&'a str>,
    pub reuse_binding: bool,
    pub selection: &'a provider::TurnModelSelection,
}

/// Retains the setup lock through the caller's native turn handoff.
pub(crate) struct Attached<'a> {
    pub owned: MutexGuard<'a, Option<Server>>,
    pub native: String,
    pub usage: UsageTracker,
    #[allow(dead_code)]
    pub cleanup: CleanupWitness,
}

fn conflict(message: &str) -> SetupFailure {
    CalmError::Conflict(message.into()).into()
}
fn wire(error: provider::acp::Error) -> SetupFailure {
    conflict(&error.to_string())
}
fn gate<'a>(role: &'a dyn Role, native: Option<&'a str>) -> PreSendGate<'a> {
    PreSendGate::new(move || {
        Box::pin(async move {
            role.check(native)
                .await
                .map_err(|error| SendRefusal(error.to_string()))
        })
    })
}

pub(crate) async fn attach<'a>(
    host: &'a Arc<AcpHost>,
    registration: Registration<'_>,
    request: Request<'_>,
    role: &mut dyn Role,
) -> std::result::Result<Attached<'a>, SetupFailure> {
    let mut owned = host
        .shared_servers
        .get(registration.config.provider.wire_name())
        .ok_or_else(|| conflict("Shared ACP server is not configured"))?
        .lock()
        .await;
    server::ensure(host, registration, &mut owned).await?;
    let server = owned.as_mut().expect("shared ACP installed");
    if server.provider != registration.config.provider {
        return Err(conflict(
            "The shared ACP host already owns a different provider",
        ));
    }
    if server.process.connection.client.shared_retiring() {
        return Err(conflict(
            "Shared ACP server is waiting for active sessions before retirement",
        ));
    }
    let pool = host
        .completion
        .repo()?
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    // Both roles fence a resumed identity before credentials or attachment can reach the agent.
    if let Some(native) = request.original_native {
        server::ensure_native_owner(pool.read_pool(), &server.provider, request.worker, native)
            .await?;
        if server
            .bindings
            .iter()
            .any(|(worker, binding)| worker != request.worker && binding.native == native)
        {
            return Err(conflict(
                "Shared ACP native session is owned by another worker",
            ));
        }
    }
    let existing = server.bindings.get(request.worker);
    if existing.is_some_and(|binding| {
        !request.reuse_binding
            || binding.closing
            || request.original_native != Some(binding.native.as_str())
    }) {
        return Err(conflict("Shared ACP binding changed"));
    }
    let mut usage = UsageTracker::new(registration.profile.usage);
    let cleanup = CleanupWitness {
        generation: server.generation.clone(),
        closed: existing
            .map(|binding| binding.closed.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
    };
    let mut guard = AttachmentGuard(None);
    let attempt = async {
        let (native, mut configuration) = if let Some(binding) = server.bindings.get(request.worker)
        {
            guard.0 = Some(server.process.connection.client.clone());
            (binding.native.clone(), binding.configuration.clone())
        } else {
            if let Some(id) = server.process.supervision_id() {
                server.touched_workers.insert(request.worker.into());
                let (execution, worker, track, cwd) = (
                    id.as_str().to_owned(),
                    request.worker.to_owned(),
                    request.track_id.to_owned(),
                    request.cwd.to_path_buf(),
                );
                crate::db::write_in_tx_typed(host.completion.repo()?.as_ref(), move |tx| {
                    Box::pin(async move {
                        calm_truth::db::sqlite::acp_resident::bind_workspace(
                            tx, &execution, &worker, &track, &cwd,
                        )
                        .await?;
                        Ok(())
                    })
                })
                .await?;
            }
            let token = role.mint().await?;
            server.credential_owners.insert(
                request.worker.into(),
                crate::mcp_server::auth::hash_token(&token),
            );
            // Credential publication starts this generation's cleanup obligation, including
            // capability refusal before a request can be constructed or written.
            role.pending(cleanup.clone());
            guard.0 = Some(server.process.connection.client.clone());
            let key = registration.profile.shared_mcp_server_key(request.worker);
            let mcp = [setup::mcp_server(
                host,
                &server.provider,
                key.clone(),
                &token,
            )];
            let (method, params) = setup::attachment_request(
                &server.process.capabilities,
                request.cwd,
                request.original_native,
                &mcp,
            )?;
            let response = setup_request(
                &mut server.process.connection,
                &server.process.commands,
                method,
                params,
                &gate(role, request.original_native),
            )
            .await?;
            let native = match request.original_native {
                Some(native) => native.to_owned(),
                None => {
                    protocol::decode::<protocol::NewSessionResponse>(response.clone())
                        .map_err(wire)?
                        .session_id
                }
            };
            if native.trim().is_empty()
                || server
                    .bindings
                    .values()
                    .any(|binding| binding.native == native)
            {
                return Err(conflict(
                    "Shared ACP returned an invalid or already-owned session",
                ));
            }
            server::ensure_native_owner(
                pool.read_pool(),
                &server.provider,
                request.worker,
                &native,
            )
            .await?;
            // Install before decoding/configuring so every later failure can close this attachment.
            server.bindings.insert(
                request.worker.into(),
                Binding {
                    native: native.clone(),
                    configuration: Configuration::default(),
                    closing: true,
                    mcp_key: key,
                    closed: cleanup.closed.clone(),
                },
            );
            usage.configure(&response);
            let configuration = Configuration::from_response(&response).map_err(wire)?;
            if request.original_native.is_none() {
                role.bind(&native).await?;
            }
            (native, configuration)
        };
        server
            .bindings
            .get_mut(request.worker)
            .expect("attached ACP")
            .closing = true;
        setup::configure(
            &mut server.process.connection,
            &server.process.commands,
            &native,
            &mut configuration,
            request.selection,
            &gate(role, Some(&native)),
            &mut usage,
            |configuration| role.observe(configuration),
        )
        .await?;
        role.publish(&native, &configuration).await?;
        let binding = server
            .bindings
            .get_mut(request.worker)
            .expect("attached ACP");
        binding.configuration = configuration;
        binding.closing = false;
        Ok::<_, SetupFailure>(native)
    }
    .await;
    match attempt {
        Ok(native) => {
            guard.0 = None;
            server.process.commands.retain_sessions(
                &server
                    .bindings
                    .values()
                    .map(|binding| binding.native.as_str())
                    .collect(),
            );
            Ok(Attached {
                owned,
                native,
                usage,
                cleanup,
            })
        }
        Err(error) => {
            drop(guard); // A lost setup reply drains this generation after its live siblings finish.
            if let Err(rollback) =
                server::detach_locked(host, &mut owned, request.worker, None).await
            {
                tracing::warn!(%rollback, "Shared ACP attachment rollback remains pending");
            }
            Err(error)
        }
    }
}

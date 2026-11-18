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

/// Captures the minted or reused credential hash before the mint transaction commits so
/// cancellation between commit and return still registers exact-hash cleanup ownership.
#[derive(Clone, Default)]
pub(crate) struct MintCleanup(Arc<std::sync::Mutex<Option<String>>>);

impl MintCleanup {
    pub(crate) fn register_hash(&self, hash: &str) {
        *self.0.lock().expect("mint cleanup") = Some(hash.to_owned());
    }

    fn take(&self) -> Option<String> {
        self.0.lock().expect("mint cleanup").take()
    }
}

struct CredentialCleanupGuard<'a> {
    owners: &'a mut std::collections::HashMap<String, String>,
    worker: &'a str,
    cleanup: MintCleanup,
    armed: bool,
}

impl Drop for CredentialCleanupGuard<'_> {
    fn drop(&mut self) {
        if self.armed
            && let Some(hash) = self.cleanup.take()
        {
            self.owners.insert(self.worker.to_owned(), hash);
        }
    }
}

/// Role policy only: durable admission, credential issuance and role-owned publication.
/// The transport owns ordering, identity collision checks and all rollback writes.
#[async_trait::async_trait]
pub(crate) trait Role: Send + Sync {
    async fn check(&self, native: Option<&str>) -> Result<()>;
    async fn mint(&mut self, cleanup: &MintCleanup) -> Result<String>;
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
    let had_existing_binding = existing.is_some();
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
    let mut latest_configuration: Option<Configuration> = None;
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
            // Credential publication starts this generation's cleanup obligation, including
            // capability refusal before a request can be constructed or written, and
            // cancellation between mint commit and return.
            role.pending(cleanup.clone());
            let mint_cleanup = MintCleanup::default();
            let mut credential_guard = CredentialCleanupGuard {
                owners: &mut server.credential_owners,
                worker: request.worker,
                cleanup: mint_cleanup.clone(),
                armed: true,
            };
            let token = role.mint(&mint_cleanup).await?;
            credential_guard.armed = false;
            drop(credential_guard);
            server.credential_owners.insert(
                request.worker.into(),
                crate::mcp_server::auth::hash_token(&token),
            );
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
        latest_configuration = Some(configuration.clone());
        setup::configure(
            &mut server.process.connection,
            &server.process.commands,
            &native,
            &mut configuration,
            request.selection,
            &gate(role, Some(&native)),
            &mut usage,
            |configuration| {
                latest_configuration = Some(configuration.clone());
                role.observe(configuration);
            },
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
        Err(error @ SetupFailure::Refused(_)) if had_existing_binding => {
            guard.0 = None;
            let server = owned.as_mut().expect("shared ACP installed");
            if let Some(binding) = server.bindings.get_mut(request.worker) {
                if let Some(configuration) = latest_configuration {
                    binding.configuration = configuration;
                }
                binding.closing = false;
            }
            server.process.commands.retain_sessions(
                &server
                    .bindings
                    .values()
                    .map(|binding| binding.native.as_str())
                    .collect(),
            );
            Err(error)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::prelude::*;
    use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
    use crate::mcp_server::managed_tokens::ManagedSessionTokens;
    use crate::model::{CardId, CardRole, NewArea, NewCard, NewTrack, new_id};
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use calm_types::worker::WorkerSessionId;
    use std::collections::HashMap;

    async fn setup_worker() -> (SqlxRepo, CardId, WorkerSessionId) {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "mint cleanup".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "mint cleanup".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let worker = new_id();
        let w = worker.clone();
        let card = crate::db::write_in_tx_typed(&repo, move |tx| {
            Box::pin(async move {
                let card = card_create_with_id_tx(
                    tx,
                    new_id(),
                    NewCard {
                        track_id: track.id,
                        title: None,
                        kind: "codex".into(),
                        sort: None,
                        payload: serde_json::json!({"planner_provider":"antigravity"}),
                    },
                    CardRole::Planner,
                    true,
                    &crate::card_role_cache::CardRoleCache::new(),
                )
                .await?;
                session_start_runtime_tx(
                    tx,
                    WorkerSessionInit {
                        id: w,
                        card_id: card.id.to_string(),
                        kind: WorkerSessionKind::SharedPlanner,
                        agent_provider: Some(AgentProvider::Antigravity),
                        status: WorkerSessionState::Idle,
                        terminal_run_id: None,
                        thread_id: None,
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        spawn_op_id: None,
                        now_ms: crate::model::now_ms(),
                    },
                )
                .await?;
                Ok(card.id)
            })
        })
        .await
        .unwrap();
        (repo, card, WorkerSessionId(worker))
    }

    async fn mint_and_cancel_after_commit(
        repo: &SqlxRepo,
        tokens: &ManagedSessionTokens,
        card: &CardId,
        worker: &WorkerSessionId,
        owners: &mut HashMap<String, String>,
    ) -> String {
        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let mint_cleanup = MintCleanup::default();
        let cleanup_for_observer = mint_cleanup.clone();
        let worker_id = worker.as_str();
        let mint_future = async move {
            let _credential_guard = CredentialCleanupGuard {
                owners,
                worker: worker_id,
                cleanup: mint_cleanup,
                armed: true,
            };
            let _token = tokens
                .ensure_with_hash_observer(repo, card, worker, move |hash| {
                    cleanup_for_observer.register_hash(hash);
                })
                .await
                .unwrap();
            let _ = committed_tx.send(());
            std::future::pending::<()>().await;
        };
        tokio::pin!(mint_future);
        tokio::select! {
            _ = &mut mint_future => unreachable!("mint_future pends after commit"),
            res = committed_rx => res.unwrap(),
        }
        // Dropping `mint_future` here simulates cancellation after commit and before return.
        String::new()
    }

    #[tokio::test]
    async fn mint_cancellation_after_commit_revokes_issued_grant_without_clobbering_replacement() {
        let (repo, card, worker) = setup_worker().await;
        let tokens = ManagedSessionTokens::default();
        let mut owners = HashMap::new();

        mint_and_cancel_after_commit(&repo, &tokens, &card, &worker, &mut owners).await;

        let issued_hash = owners
            .remove(worker.as_str())
            .expect("cleanup ownership must be registered before commit");
        assert_eq!(
            repo.session_get(&worker)
                .await
                .unwrap()
                .unwrap()
                .mcp_token_hash
                .as_deref(),
            Some(issued_hash.as_str())
        );

        server::revoke_worker(&repo, worker.as_str(), &issued_hash)
            .await
            .unwrap();
        assert_eq!(
            repo.session_get(&worker)
                .await
                .unwrap()
                .unwrap()
                .mcp_token_hash,
            None,
            "exact-hash cleanup must clear the orphaned grant"
        );

        // Second fault injection: an independent writer replaces the grant before cleanup runs.
        mint_and_cancel_after_commit(&repo, &tokens, &card, &worker, &mut owners).await;
        let second_issued_hash = owners
            .remove(worker.as_str())
            .expect("cleanup ownership must be registered before second commit");
        assert_ne!(second_issued_hash, issued_hash);

        repo.db()
            .writer()
            .execute(
                sqlx::query("UPDATE worker_sessions SET mcp_token_hash=?1 WHERE id=?2")
                    .bind("independent-replacement-hash")
                    .bind(worker.as_str()),
            )
            .await
            .unwrap();

        server::revoke_worker(&repo, worker.as_str(), &second_issued_hash)
            .await
            .unwrap();
        assert_eq!(
            repo.session_get(&worker)
                .await
                .unwrap()
                .unwrap()
                .mcp_token_hash
                .as_deref(),
            Some("independent-replacement-hash"),
            "exact-hash cleanup must preserve an independently replaced grant"
        );
    }
}

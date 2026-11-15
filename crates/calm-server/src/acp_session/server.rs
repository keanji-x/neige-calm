//! Shared ACP process binding and exact-attachment cleanup. Role admission and outcomes live in callers.
use crate::acp_session::{
    host::{AcpHost, Registration},
    process::{self, setup_request},
};
use crate::{
    db::{RepoEventWrite, write_in_tx_typed},
    error::{CalmError, Result},
};
use calm_types::runtime::AgentProvider;
use provider::acp::{Incoming, configuration::Configuration, protocol};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[derive(Debug)]
pub(crate) struct Binding {
    pub(crate) native: String,
    pub(crate) configuration: Configuration,
    pub(crate) closing: bool,
    pub(crate) mcp_key: String,
    pub(crate) closed: Arc<AtomicBool>,
}

pub(crate) struct Generation {
    pub(crate) id: uuid::Uuid,
    /// Set only after revoking this generation's grants, confirming the ACP
    /// service process exited, and executing the existing process-group cleanup.
    /// This is not the execution owner's descendants-complete quiescence proof.
    pub(crate) stopped: AtomicBool,
    retired: tokio::sync::Notify,
}

impl Generation {
    pub(crate) async fn wait_stopped(&self) {
        loop {
            let notified = self.retired.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.stopped.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone)]
pub(crate) struct CleanupWitness {
    pub(crate) generation: Arc<Generation>,
    /// One attachment instance; resuming the same native id gets a fresh witness.
    pub(crate) closed: Arc<AtomicBool>,
}
impl CleanupWitness {
    pub(crate) fn confirmed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.generation.stopped.load(Ordering::Acquire)
    }
}

pub(crate) struct Server {
    pub(crate) process: process::Process,
    pub(crate) bindings: HashMap<String, Binding>,
    pub(crate) credential_owners: HashMap<String, String>,
    pub(crate) generation: Arc<Generation>,
    pub(crate) provider: AgentProvider,
}
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedAcpServer")
            .field("bindings", &self.bindings.len())
            .finish()
    }
}

pub(crate) struct AttachmentGuard(pub(crate) Option<provider::acp::Client>);
impl Drop for AttachmentGuard {
    fn drop(&mut self) {
        if let Some(client) = &self.0 {
            client.retire_shared_when_idle();
        }
    }
}

pub(crate) async fn ensure_native_owner<'e>(
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

pub(crate) async fn detach_worker(host: &AcpHost, worker: &str) -> Result<()> {
    detach_worker_with_witness(host, worker, None).await
}

pub(crate) async fn detach_worker_with_witness(
    host: &AcpHost,
    worker: &str,
    expected: Option<(&str, &CleanupWitness)>,
) -> Result<()> {
    let mut owned = host.shared_server.lock().await;
    detach_locked(host, &mut owned, worker, expected).await
}

pub(crate) async fn detach_locked(
    host: &AcpHost,
    owned: &mut Option<Server>,
    worker: &str,
    expected: Option<(&str, &CleanupWitness)>,
) -> Result<()> {
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
        return retire(host, owned).await;
    }
    if let Some(binding) = server.bindings.get_mut(worker) {
        binding.closing = true;
    }
    if let Some(hash) = server.credential_owners.get(worker) {
        revoke_worker(host.completion.repo()?.as_ref(), worker, hash).await?;
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
pub(crate) async fn supervise(host: std::sync::Weak<AcpHost>, generation: uuid::Uuid) {
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
            .filter(|(worker, _)| {
                server
                    .bindings
                    .get(*worker)
                    .is_none_or(|binding| binding.closing)
            })
            .map(|(worker, hash)| (worker.clone(), hash.clone()))
            .collect();
        for (worker, hash) in orphaned {
            let revoked = match host.completion.repo() {
                Ok(repo) => revoke_worker(repo.as_ref(), &worker, &hash).await,
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

pub(crate) async fn retire(host: &AcpHost, owned: &mut Option<Server>) -> Result<()> {
    if let Some(server) = owned.as_ref() {
        for (worker, hash) in &server.credential_owners {
            revoke_worker(host.completion.repo()?.as_ref(), worker, hash).await?;
        }
    }
    if let Some(server) = owned.take() {
        let generation = server.generation.clone();
        server.process.stop().await?;
        generation.stopped.store(true, Ordering::Release);
        generation.retired.notify_waiters();
    }
    Ok(())
}
pub(crate) async fn revoke_worker(
    repo: &dyn RepoEventWrite,
    worker: &str,
    expected_hash: &str,
) -> Result<()> {
    let worker = worker.to_owned();
    let expected_hash = expected_hash.to_owned();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1 AND mcp_token_hash=?2",
            )
            .bind(worker)
            .bind(expected_hash)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
    })
    .await
}
pub(crate) async fn ensure(
    host: &Arc<AcpHost>,
    registration: Registration<'_>,
    owned: &mut Option<Server>,
) -> Result<()> {
    if owned.as_ref().is_some_and(|server| {
        server.process.connection.client.terminal_error().is_some()
            || server.process.connection.client.can_recycle_shared()
    }) {
        retire(host, owned).await?;
    }
    if owned.is_none() {
        let process =
            process::Process::spawn_shared(host, registration, &std::env::temp_dir()).await?;
        if process.capabilities.session_capabilities.close.is_none() {
            return Err(CalmError::Conflict(
                "Shared ACP requires session close".into(),
            ));
        }
        let generation = uuid::Uuid::new_v4();
        *owned = Some(Server {
            process,
            bindings: HashMap::new(),
            credential_owners: HashMap::new(),
            generation: Arc::new(Generation {
                id: generation,
                stopped: AtomicBool::new(false),
                retired: Default::default(),
            }),
            provider: registration.config.provider.clone(),
        });
        tokio::spawn(supervise(Arc::downgrade(host), generation));
    }
    Ok(())
}

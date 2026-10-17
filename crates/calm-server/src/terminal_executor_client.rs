//! Configured transport to the existing executor. Domain selection and source
//! preparation belong to the workspace/operation owners, not this client.

use calm_session::control::execution::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum TerminalExecutionTransport {
    Legacy,
    Registered(RegisteredExecutorConfig),
}

#[derive(Clone, Debug)]
pub struct RegisteredExecutorConfig {
    socket: PathBuf,
    secret: CapabilityToken,
}

impl RegisteredExecutorConfig {
    pub fn from_files(socket: &Path, key_file: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            socket.is_absolute() && key_file.is_absolute(),
            "registered executor socket and client key must be absolute configured paths"
        );
        let secret = serde_json::from_reader(std::fs::File::open(key_file)?)?;
        Ok(Self {
            socket: socket.into(),
            secret,
        })
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) async fn connect(&self) -> std::io::Result<RegisteredExecutorConnection> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            RegisteredExecutorConnection::connect_with_query(&self.socket, self.secret.clone()),
        )
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "registered executor connection timed out",
            )
        })?
    }
}

/// Exact original process controls. Observing/stopping never opens a cwd or
/// acquires fresh source admission, and does not pretend that Exited is Drained.
pub(crate) struct ExecutionControlHandle {
    config: RegisteredExecutorConfig,
    request: PrepareExecution,
    receipt: ExecutionReceipt,
}

impl ExecutionControlHandle {
    pub(crate) fn config(&self) -> &RegisteredExecutorConfig {
        &self.config
    }
    pub(crate) fn identity(&self) -> &ExecutionIdentity {
        &self.request.identity
    }
    pub(crate) fn new(
        config: RegisteredExecutorConfig,
        request: PrepareExecution,
        receipt: ExecutionReceipt,
    ) -> Self {
        Self {
            config,
            request,
            receipt,
        }
    }

    pub(crate) async fn control(&self, action: ProcessAction) -> std::io::Result<ExecutionReply> {
        let (purpose, token) = match &action {
            ProcessAction::Observe { .. } => (
                ExecutionControlPurpose::Observe,
                &self.request.control_tokens.observe,
            ),
            ProcessAction::WriteStdin { .. } | ProcessAction::Resize { .. } => (
                ExecutionControlPurpose::Interactive,
                &self.request.control_tokens.interactive,
            ),
            ProcessAction::Signal(_) | ProcessAction::Cleanup => (
                ExecutionControlPurpose::Lifecycle,
                &self.request.control_tokens.lifecycle,
            ),
        };
        let mut client = self.config.connect().await?;
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: ExecutionCapability {
                    identity: self.request.identity.clone(),
                    slot_version: self.receipt.slot_version,
                    token: token.clone(),
                    purpose,
                },
                action,
            }))
            .await
    }

    pub(crate) async fn revoke(&self) -> std::io::Result<ExecutionReply> {
        let mut client = self.config.connect().await?;
        client
            .request(ExecutionRequest::Revoke(ConsumeExecution {
                execution_id: self.request.identity.execution_id.clone(),
                ticket_id: self.request.identity.ticket_id.clone(),
                ticket: self.request.ticket.clone(),
            }))
            .await
    }
}

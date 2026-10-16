//! Versioned, authenticated kernel/executor protocol. Legacy control is negotiated separately.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
pub use workspace::execution::*;
use workspace::identity::WorkspaceId;

use super::{IoMode, ProcSignal};

pub mod attachment;
mod attachment_client;
mod directory_relation;
pub mod rights;
pub use attachment::*;
pub use directory_relation::*;

pub const EXECUTION_ENVELOPE_VERSION: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionHandshake {
    pub required_contract: u32,
    pub client_secret: CapabilityToken,
    pub mode: ExecutionProtocolMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionProtocolMode {
    RegisteredV3,
    LegacyWorktree,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryRegistration {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionRegistration {
    Worktree {
        workspace_id: WorkspaceId,
        root: DirectoryRegistration,
        binding_id: WorkspaceBindingId,
        binding_version: u64,
        generation: u64,
        install_epoch: u64,
        admission_open: bool,
        authority: ExecutionAuthority,
    },
    ExternalDir {
        directory_id: ExternalDirectoryId,
        root: DirectoryRegistration,
        registration_version: u64,
        admission_open: bool,
        authority: ExecutionAuthority,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSpecification {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Complete explicit child environment. No ambient values are added.
    pub environment: Vec<(String, String)>,
    pub io_mode: IoMode,
    pub replay_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareExecution {
    pub identity: ExecutionIdentity,
    pub ticket: CapabilityToken,
    pub control_tokens: ExecutionControlTokens,
    pub handoff: Option<AliasHandoff>,
    pub specification: ExecutionSpecification,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionControlTokens {
    pub observe: CapabilityToken,
    pub interactive: CapabilityToken,
    pub lifecycle: CapabilityToken,
}
impl ExecutionControlTokens {
    pub fn fresh() -> std::io::Result<Self> {
        Ok(Self {
            observe: CapabilityToken::fresh()?,
            interactive: CapabilityToken::fresh()?,
            lifecycle: CapabilityToken::fresh()?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsumeExecution {
    pub execution_id: ExecutionId,
    pub ticket_id: LaunchTicketId,
    pub ticket: CapabilityToken,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExecutionRequest {
    Handshake(ExecutionHandshake),
    Attach(AttachRegistration),
    Export(ExportRegistration),
    ExportAck(ExportAcknowledgement),
    Prepare(PrepareExecution),
    Launch(ConsumeExecution),
    Revoke(ConsumeExecution),
    Control(ExecutionControl),
    ObserveDirectories(Box<DirectoryRelationRequest>),
}

/// All process actions, including future variants, must first resolve this exact capability.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionControl {
    pub capability: ExecutionCapability,
    pub action: ProcessAction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProcessAction {
    Observe {
        from_cursor: Option<u64>,
    },
    WriteStdin {
        bytes: Vec<u8>,
    },
    Resize {
        cols: u16,
        rows: u16,
        pixel_w: u16,
        pixel_h: u16,
    },
    Signal(ProcSignal),
    Cleanup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionState {
    Prepared,
    Revoked,
    Forking,
    Spawned,
    Exited { status: Option<i32> },
    Drained { status: Option<i32> },
    NoChild { reason: String },
    Unknown { reason: String },
}

impl ExecutionState {
    pub fn permits_handoff(&self) -> bool {
        matches!(
            self,
            Self::Revoked | Self::Drained { .. } | Self::NoChild { .. }
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExecutionReply {
    Handshake {
        envelope_version: u32,
        executor_boot: ExecutorBootId,
        mode: ExecutionProtocolMode,
    },
    Registered(ObjectRegistrationReceipt),
    Exported {
        receipt: ObjectRegistrationReceipt,
        nonce: TransferNonce,
    },
    ExportAcknowledged,
    Prepared,
    Receipt {
        receipt: Option<ExecutionReceipt>,
        state: ExecutionState,
    },
    Observation {
        receipt: Option<ExecutionReceipt>,
        state: ExecutionState,
        cursor: u64,
        output: Vec<u8>,
    },
    /// Durable exact-ticket revocation; a spawned process still requires independent drain evidence.
    RevocationAcknowledged {
        execution_id: ExecutionId,
        ticket_id: LaunchTicketId,
        receipt: Option<ExecutionReceipt>,
        state: ExecutionState,
    },
    ActionComplete,
    Refused {
        reason: String,
    },
    DirectoryRelation(Box<DirectoryRelationReport>),
    ObjectQualificationFailed {
        failure: workspace::execution::objects::ObjectQualificationFailure,
    },
}

/// Explicit worktree compatibility, qualified by the released v2 Probe contract or v3 handshake.
/// EOF, decoding errors and unknown versions never authorize legacy control.
pub async fn connect_legacy(
    path: impl AsRef<std::path::Path>,
) -> std::io::Result<tokio::net::UnixStream> {
    tokio::time::timeout(std::time::Duration::from_secs(5), legacy_negotiation(path))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "supervisor negotiation timed out",
            )
        })?
}

async fn legacy_negotiation(
    path: impl AsRef<std::path::Path>,
) -> std::io::Result<tokio::net::UnixStream> {
    let mut stream = tokio::net::UnixStream::connect(path).await?;
    crate::write_frame(
        &mut stream,
        &super::ControlMsg::Probe(super::ProbeRequest {
            proc_id: String::new(),
        }),
    )
    .await
    .map_err(std::io::Error::other)?;
    let version = match crate::read_frame::<super::ControlReply, _>(&mut stream)
        .await
        .map_err(std::io::Error::other)?
    {
        super::ControlReply::ProbeOk {
            supervisor_version, ..
        } => supervisor_version,
        _ => {
            return Err(std::io::Error::other(
                "supervisor supplied no known compatibility contract",
            ));
        }
    };
    // Released control v2 uses the unchanged Frame v4 Probe/legacy shapes. It cannot host typed slots.
    if version == 2 {
        return Ok(stream);
    }
    if version != crate::SUPERVISOR_CONTROL_VERSION {
        return Err(std::io::Error::other(
            "unsupported legacy supervisor version",
        ));
    }
    crate::write_frame(
        &mut stream,
        &super::ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: 0,
            client_secret: CapabilityToken::fresh()?,
            mode: ExecutionProtocolMode::LegacyWorktree,
        }))),
    )
    .await
    .map_err(std::io::Error::other)?;
    match crate::read_frame::<super::ControlReply, _>(&mut stream)
        .await
        .map_err(std::io::Error::other)?
    {
        super::ControlReply::Execution(reply)
            if matches!(
                *reply,
                ExecutionReply::Handshake {
                    mode: ExecutionProtocolMode::LegacyWorktree,
                    ..
                }
            ) =>
        {
            Ok(stream)
        }
        _ => Err(std::io::Error::other(
            "legacy supervisor negotiation refused",
        )),
    }
}

/// A negotiated authenticated kernel connection. Source grants still come from kernel resolve/intent.
pub struct RegisteredExecutorConnection {
    stream: tokio::net::UnixStream,
    executor_boot: ExecutorBootId,
    registrations: Vec<ObjectRegistrationReceipt>,
    in_flight: bool,
}

impl RegisteredExecutorConnection {
    pub async fn connect(
        path: impl AsRef<std::path::Path>,
        client_secret: CapabilityToken,
    ) -> std::io::Result<Self> {
        let mut stream = tokio::net::UnixStream::connect(path).await?;
        crate::write_frame(
            &mut stream,
            &super::ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(
                ExecutionHandshake {
                    required_contract: EXECUTION_ENVELOPE_VERSION,
                    client_secret,
                    mode: ExecutionProtocolMode::RegisteredV3,
                },
            ))),
        )
        .await
        .map_err(std::io::Error::other)?;
        let reply = crate::read_frame::<super::ControlReply, _>(&mut stream)
            .await
            .map_err(std::io::Error::other)?;
        let super::ControlReply::Execution(reply) = reply else {
            return Err(std::io::Error::other(
                "registered executor negotiation refused",
            ));
        };
        match *reply {
            ExecutionReply::Handshake {
                envelope_version: EXECUTION_ENVELOPE_VERSION,
                executor_boot,
                mode: ExecutionProtocolMode::RegisteredV3,
            } => Ok(Self {
                stream,
                executor_boot,
                registrations: Vec::new(),
                in_flight: false,
            }),
            _ => Err(std::io::Error::other(
                "registered executor negotiation refused",
            )),
        }
    }
    pub fn executor_boot(&self) -> &ExecutorBootId {
        &self.executor_boot
    }
    pub fn registrations(&self) -> &[ObjectRegistrationReceipt] {
        &self.registrations
    }
    fn begin(&mut self) -> std::io::Result<()> {
        if self.in_flight {
            return Err(std::io::Error::other(
                "connection interrupted during a frame; reconnect",
            ));
        }
        self.in_flight = true;
        Ok(())
    }
    pub async fn request(&mut self, request: ExecutionRequest) -> std::io::Result<ExecutionReply> {
        if matches!(request, ExecutionRequest::Handshake(_)) {
            return Err(std::io::Error::other("connection already negotiated"));
        }
        self.begin()?;
        rights::send(
            &self.stream,
            &super::ControlMsg::Execution(Box::new(request)),
            &[],
        )
        .await?;
        let (message, descriptors) = rights::receive::<super::ControlReply>(&self.stream).await?;
        if !descriptors.is_empty() {
            return Err(std::io::Error::other("unexpected response descriptors"));
        }
        match message {
            super::ControlReply::Execution(reply) => {
                self.in_flight = false;
                Ok(*reply)
            }
            _ => Err(std::io::Error::other(
                "unexpected executor protocol response",
            )),
        }
    }
}

#[cfg(test)]
mod tests;

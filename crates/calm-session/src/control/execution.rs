//! Versioned, authenticated kernel/executor protocol. Legacy control is negotiated separately.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
pub use workspace::execution::*;
use workspace::identity::WorkspaceId;

use super::{IoMode, ProcSignal};

pub const EXECUTION_ENVELOPE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionHandshake {
    pub client_secret: CapabilityToken,
    pub mode: ExecutionProtocolMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionProtocolMode {
    RegisteredV1,
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
    Register(ExecutionRegistration),
    Prepare(PrepareExecution),
    Launch(ConsumeExecution),
    Revoke(ConsumeExecution),
    Control(ExecutionControl),
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
    Registered,
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
    ActionComplete,
    Refused {
        reason: String,
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
        &super::ControlMsg::Execution(ExecutionRequest::Handshake(ExecutionHandshake {
            client_secret: CapabilityToken::fresh()?,
            mode: ExecutionProtocolMode::LegacyWorktree,
        })),
    )
    .await
    .map_err(std::io::Error::other)?;
    match crate::read_frame::<super::ControlReply, _>(&mut stream)
        .await
        .map_err(std::io::Error::other)?
    {
        super::ControlReply::Execution(ExecutionReply::Handshake {
            mode: ExecutionProtocolMode::LegacyWorktree,
            ..
        }) => Ok(stream),
        _ => Err(std::io::Error::other(
            "legacy supervisor negotiation refused",
        )),
    }
}

/// A negotiated authenticated kernel connection. Source grants still come from kernel resolve/intent.
pub struct RegisteredExecutorConnection {
    stream: tokio::net::UnixStream,
    executor_boot: ExecutorBootId,
}

impl RegisteredExecutorConnection {
    pub async fn connect(
        path: impl AsRef<std::path::Path>,
        client_secret: CapabilityToken,
    ) -> std::io::Result<Self> {
        let mut stream = tokio::net::UnixStream::connect(path).await?;
        crate::write_frame(
            &mut stream,
            &super::ControlMsg::Execution(ExecutionRequest::Handshake(ExecutionHandshake {
                client_secret,
                mode: ExecutionProtocolMode::RegisteredV1,
            })),
        )
        .await
        .map_err(std::io::Error::other)?;
        match crate::read_frame::<super::ControlReply, _>(&mut stream)
            .await
            .map_err(std::io::Error::other)?
        {
            super::ControlReply::Execution(ExecutionReply::Handshake {
                envelope_version: EXECUTION_ENVELOPE_VERSION,
                executor_boot,
                mode: ExecutionProtocolMode::RegisteredV1,
            }) => Ok(Self {
                stream,
                executor_boot,
            }),
            _ => Err(std::io::Error::other(
                "registered executor negotiation refused",
            )),
        }
    }
    pub fn executor_boot(&self) -> &ExecutorBootId {
        &self.executor_boot
    }
    pub async fn request(&mut self, request: ExecutionRequest) -> std::io::Result<ExecutionReply> {
        if matches!(request, ExecutionRequest::Handshake(_)) {
            return Err(std::io::Error::other("connection already negotiated"));
        }
        crate::write_frame(&mut self.stream, &super::ControlMsg::Execution(request))
            .await
            .map_err(std::io::Error::other)?;
        match crate::read_frame::<super::ControlReply, _>(&mut self.stream)
            .await
            .map_err(std::io::Error::other)?
        {
            super::ControlReply::Execution(reply) => Ok(reply),
            _ => Err(std::io::Error::other(
                "unexpected executor protocol response",
            )),
        }
    }
}

#[cfg(test)]
mod tests;

//! Execution identity and sealed executor inputs. Kernel policy supplies authorization.

use std::{fmt, io::Read};

use serde::{Deserialize, Serialize};

use crate::identity::{WorkspaceCwd, WorkspaceId};

macro_rules! identity {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn fresh() -> std::io::Result<Self> {
                let mut bytes = [0u8; 16];
                std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
                Ok(Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect()))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(value: String) -> Result<Self, String> {
                if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                    return Err(concat!("invalid ", stringify!($name)).into());
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for String { fn from(value: $name) -> Self { value.0 } }
    )+};
}

identity!(
    ExecutionId,
    ExecutorBootId,
    LaunchTicketId,
    WorkspaceBindingId,
    ExternalDirectoryId,
    AuthorizationBindingId,
    ExecutionGrantId,
    SpecificationId
);

/// An unpredictable capability. Debug never exposes its bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityToken([u8; 32]);

impl CapabilityToken {
    pub fn fresh() -> std::io::Result<Self> {
        let mut bytes = [0; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(Self(bytes))
    }
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn matches(&self, other: &Self) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

impl fmt::Debug for CapabilityToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CapabilityToken([redacted])")
    }
}

/// Opaque authorization supplied by the existing kernel owner, not a user/role declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAuthority {
    pub grant_id: ExecutionGrantId,
    pub authorization_binding: AuthorizationBindingId,
    pub authorization_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionScope {
    Worktree {
        workspace_id: WorkspaceId,
        binding_id: WorkspaceBindingId,
        binding_version: u64,
        generation: u64,
        install_epoch: u64,
        cwd: WorkspaceCwd,
    },
    ExternalDir {
        directory_id: ExternalDirectoryId,
        registration_version: u64,
        cwd: WorkspaceCwd,
    },
}

/// The durable alias slot is independent of historical execution receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasHandoff {
    pub previous_execution: ExecutionId,
    pub expected_slot_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub execution_id: ExecutionId,
    pub executor_boot: ExecutorBootId,
    pub ticket_id: LaunchTicketId,
    pub authority: ExecutionAuthority,
    pub scope: ExecutionScope,
    pub operation_id: String,
    pub specification_id: SpecificationId,
    pub process_alias: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeProcessIdentity {
    /// The owned scope helper; payload PID/PGID values are not control targets.
    pub pid: u32,
    pub start_time_ticks: u64,
    pub nonce: CapabilityToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeMechanismIdentity {
    pub executable_device: u64,
    pub executable_inode: u64,
    pub protocol_version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeDrainProof {
    pub helper_process: NativeProcessIdentity,
    pub mechanism: ScopeMechanismIdentity,
    pub kernel_state: KernelChildState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelChildState {
    NoChildrenAfterForkEntryClosed,
}

/// Frozen once fork/registration succeeds. Exit/drain are separate journal facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub identity: ExecutionIdentity,
    pub slot_version: u64,
    pub process: NativeProcessIdentity,
    pub scope_mechanism: ScopeMechanismIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionControlPurpose {
    Observe,
    Interactive,
    Lifecycle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCapability {
    pub identity: ExecutionIdentity,
    pub slot_version: u64,
    pub token: CapabilityToken,
    pub purpose: ExecutionControlPurpose,
}

//! Dedicated provider endpoints. Callers persist checkpoints in existing Operations;
//! this module does not write task state or record transcripts.
mod admission;
mod bootstrap;
mod home;
mod layout;
mod policy;
mod session;

pub use admission::{TurnAdmission, TurnLaunch};
pub use bootstrap::WorkspaceRequirement;
pub use home::{HomeReceipt, HomeSeed, NativeMcp, PrivateHome, ProviderSettings};
pub(crate) use policy::MCP_TOOL_ALLOWLIST;
pub use policy::{
    DELIVERY_PROFILE, RECOVER_CHANGES, executor_environment, executor_environment_with_plugins,
};
pub use session::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("workspace preparation must satisfy {0} before baseline capture")]
    WorkspacePrecondition(WorkspaceRequirement),
    #[error("dedicated provider unsupported: {0}")]
    Unsupported(String),
    #[error("dedicated provider conflict: {0}")]
    Conflict(String),
    #[error("dedicated provider evidence unavailable: {0}")]
    Unknown(String),
    #[error("dedicated provider configuration invalid: {0}")]
    Configuration(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Boundary(#[from] calm_worker_runtime::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
